import XCTest
import VoiceBridgeC
@testable import VoiceBridge

final class FakePlanEngine: PlanEngine, @unchecked Sendable {
    private let lock = NSLock()
    private var code: UInt32 = VB_MA_AVAILABLE
    private var reply: Result<String, PlanFailure> = .success("{\"step\":\"none\"}")
    private var counts = (plans: 0, finished: 0, prewarms: 0, discards: 0)
    private var open = false
    private var gatedValue = false
    private var ignoresCancelValue = false
    /// A gated plan waits until `release`, or until its task is cancelled
    /// (a `Gate` would ignore cancellation, and a task group waits for it).
    var gated: Bool {
        get { lock.withLock { gatedValue } }
        set { lock.withLock { gatedValue = newValue } }
    }
    /// Models a `respond` that ignores task cancellation (review item 11b).
    var ignoresCancel: Bool {
        get { lock.withLock { ignoresCancelValue } }
        set { lock.withLock { ignoresCancelValue = newValue } }
    }
    /// Runs inside `prewarm`, to model a discard that lands mid-prewarm.
    var duringPrewarm: (@Sendable () -> Void)? {
        get { lock.withLock { duringPrewarmValue } }
        set { lock.withLock { duringPrewarmValue = newValue } }
    }
    private var duringPrewarmValue: (@Sendable () -> Void)?

    func release() { lock.withLock { open = true } }
    func set(code: UInt32) { lock.withLock { self.code = code } }
    func set(reply: Result<String, PlanFailure>) { lock.withLock { self.reply = reply } }
    var finished: Int { lock.withLock { counts.finished } }
    /// Engine calls that began (a request cancelled before it starts never gets here).
    var plans: Int { lock.withLock { counts.plans } }
    var prewarms: Int { lock.withLock { counts.prewarms } }
    var discards: Int { lock.withLock { counts.discards } }

    func availability() -> UInt32 { lock.withLock { code } }
    func prewarm(instructions: String) {
        lock.withLock { counts.prewarms += 1 }
        duringPrewarm?()
    }
    func discard() { lock.withLock { counts.discards += 1 } }
    func plan(instructions: String, input: String) async throws -> String {
        lock.withLock { counts.plans += 1 }
        defer { lock.withLock { counts.finished += 1 } }
        let ignores = ignoresCancel
        while gated && !lock.withLock({ open }) && (ignores || !Task.isCancelled) {
            try? await Task.sleep(nanoseconds: 1_000_000)
        }
        if !ignores { try Task.checkCancellation() }
        return try lock.withLock { reply }.get()
    }
}

/// Collects what a private Bridge delivers.
final class Collected: @unchecked Sendable {
    private let lock = NSLock()
    private var all: [Event] = []
    func add(_ e: Event) { lock.withLock { all.append(e) } }
    var events: [Event] { lock.withLock { all } }
}

final class ModelTests: XCTestCase {
    private func rig() -> (ModelRequests, FakePlanEngine, Bridge, Collected) {
        let bridge = Bridge()
        let got = Collected()
        bridge.install { got.add($0) }
        let engine = FakePlanEngine()
        return (ModelRequests(engine: engine, bridge: bridge), engine, bridge, got)
    }

    private func until(_ what: String, _ cond: () -> Bool, file: StaticString = #filePath, line: UInt = #line) {
        let deadline = Date().addingTimeInterval(3)
        while !cond() {
            if Date() > deadline { return XCTFail("timed out: \(what)", file: file, line: line) }
            usleep(2_000)
        }
    }

    private func replies(_ got: Collected) -> [Event] { got.events.filter { $0.kind == VB_EV_MODEL } }

    func testAReplyCarriesItsIdAndTheJson() {
        let (requests, _, bridge, got) = rig()
        XCTAssertEqual(requests.start(7, instructions: "I", input: "D", timeoutMs: 5_000), VB_OK)
        until("reply") { bridge.events.sync {}; return !replies(got).isEmpty }
        XCTAssertEqual(replies(got), [Event(generation: 7, kind: VB_EV_MODEL, code: UInt32(VB_OK), text: "{\"step\":\"none\"}")])
        XCTAssertEqual(requests.countForTest, 0)
    }

    /// Review Focus 1.
    func testACancelledRequestNeverReplies() {
        let (requests, engine, bridge, got) = rig()
        engine.gated = true
        _ = requests.start(8, instructions: "I", input: "D", timeoutMs: 5_000)
        until("the plan began") { engine.plans == 1 }
        requests.cancel(8)
        until("the plan ended") { engine.finished == 1 }
        bridge.events.sync {}
        XCTAssertTrue(replies(got).isEmpty)
    }

    /// Review Focus 2, plan V6.
    func testAvailabilityIsCheckedOnEveryRequest() {
        let (requests, engine, bridge, got) = rig()
        engine.set(code: VB_MA_INTELLIGENCE_OFF)
        _ = requests.start(1, instructions: "I", input: "D", timeoutMs: 5_000)
        until("first reply") { bridge.events.sync {}; return replies(got).count == 1 }
        XCTAssertEqual(replies(got)[0].code, UInt32(VB_ERR_MODEL_UNAVAILABLE))
        XCTAssertEqual(replies(got)[0].value, VB_MA_INTELLIGENCE_OFF)
        XCTAssertNil(replies(got)[0].text)
        engine.set(code: VB_MA_AVAILABLE)
        _ = requests.start(2, instructions: "I", input: "D", timeoutMs: 5_000)
        until("second reply") { bridge.events.sync {}; return replies(got).count == 2 }
        XCTAssertEqual(replies(got)[1].code, UInt32(VB_OK))
        let caps = got.events.filter { $0.kind == VB_EV_MODEL_CAPS }.map(\.code)
        XCTAssertEqual(caps, [VB_MA_INTELLIGENCE_OFF, VB_MA_AVAILABLE], "reported once per change")
    }

    func testQueryReportsOnlyChanges() {
        let (requests, engine, bridge, got) = rig()
        requests.query()
        requests.query()
        engine.set(code: VB_MA_NOT_READY)
        requests.query()
        bridge.events.sync {}
        XCTAssertEqual(got.events.filter { $0.kind == VB_EV_MODEL_CAPS }.map(\.code), [VB_MA_AVAILABLE, VB_MA_NOT_READY])
    }

    /// Review item 6: after an overflow Rust forces a report, so a lost first
    /// report cannot leave Settings on "Checking".
    func testAForcedQueryReportsAgain() {
        let (requests, _, bridge, got) = rig()
        requests.query()
        requests.query(force: true)
        bridge.events.sync {}
        XCTAssertEqual(got.events.filter { $0.kind == VB_EV_MODEL_CAPS }.map(\.code), [VB_MA_AVAILABLE, VB_MA_AVAILABLE])
    }

    /// Spec §6.3: a failure is a code, never the error's text.
    func testAFailureNeverCarriesText() {
        let (requests, engine, bridge, got) = rig()
        engine.set(reply: .failure(.failed))
        _ = requests.start(3, instructions: "I", input: "D", timeoutMs: 5_000)
        until("reply") { bridge.events.sync {}; return !replies(got).isEmpty }
        XCTAssertEqual(replies(got)[0].code, UInt32(VB_ERR_MODEL_FAILED))
        XCTAssertNil(replies(got)[0].text)
    }

    func testSwiftGivesUpAfterTheTimeoutPlusASecond() {
        let (requests, engine, bridge, got) = rig()
        engine.gated = true
        let started = Date()
        _ = requests.start(4, instructions: "I", input: "D", timeoutMs: 0)
        until("timeout reply") { bridge.events.sync {}; return !replies(got).isEmpty }
        XCTAssertEqual(replies(got)[0].code, UInt32(VB_ERR_MODEL_TIMEOUT))
        XCTAssertGreaterThan(Date().timeIntervalSince(started), 0.9, "the backstop is timeoutMs + 1000 ms")
    }

    /// Spec §6.3: voice off, lock, sleep and quit discard model work.
    func testCancelAllCancelsEveryRequest() {
        let (requests, engine, bridge, got) = rig()
        engine.gated = true
        _ = requests.start(5, instructions: "I", input: "D", timeoutMs: 5_000)
        _ = requests.start(6, instructions: "I", input: "D", timeoutMs: 5_000)
        until("both plans began") { engine.plans == 2 }
        requests.cancelAll()
        XCTAssertEqual(requests.countForTest, 0)
        XCTAssertEqual(engine.discards, 1, "the prewarmed session is dropped")
        until("both plans ended") { engine.finished == 2 }
        bridge.events.sync {}
        XCTAssertTrue(replies(got).isEmpty)
    }

    func testPrewarmOnlyWhenAvailable() {
        let (requests, engine, _, _) = rig()
        requests.prewarm(instructions: "I")
        engine.set(code: VB_MA_INTELLIGENCE_OFF)
        requests.prewarm(instructions: "I")
        XCTAssertEqual(engine.prewarms, 1)
    }

    func testIdZeroIsRefused() {
        let (requests, _, _, _) = rig()
        XCTAssertEqual(requests.start(0, instructions: "I", input: "D", timeoutMs: 5_000), VB_ERR_FAILED)
    }

    /// Review item 11b: if the model ignores cancellation, cancelled work
    /// keeps a slot until it ends; past `maxRunning` a new plan is refused
    /// at once instead of waiting behind it.
    func testCancelledWorkThatKeepsRunningCannotPileUp() {
        let (requests, engine, bridge, got) = rig()
        engine.gated = true
        engine.ignoresCancel = true
        _ = requests.start(1, instructions: "I", input: "D", timeoutMs: 5_000)
        _ = requests.start(2, instructions: "I", input: "D", timeoutMs: 5_000)
        until("both slots taken") { requests.runningForTest == 2 }
        requests.cancel(1)
        requests.cancel(2)
        _ = requests.start(3, instructions: "I", input: "D", timeoutMs: 5_000)
        until("refused") { bridge.events.sync {}; return !replies(got).isEmpty }
        XCTAssertEqual(replies(got).map(\.generation), [3])
        XCTAssertEqual(replies(got)[0].code, UInt32(VB_ERR_MODEL_FAILED))
        engine.release()
        until("slots freed") { requests.runningForTest == 0 }
        engine.gated = false
        _ = requests.start(4, instructions: "I", input: "D", timeoutMs: 5_000)
        until("answered") { bridge.events.sync {}; return replies(got).count == 2 }
        XCTAssertEqual(replies(got)[1].code, UInt32(VB_OK))
        XCTAssertEqual(replies(got).map(\.generation), [3, 4], "the two cancelled requests never replied")
    }

    /// Review: a model that ignores cancellation and then finishes must not
    /// reply for a request that was cancelled (the `live` guard).
    func testACancelledRequestThatIgnoresCancellationStillNeverReplies() {
        let (requests, engine, bridge, got) = rig()
        engine.gated = true
        engine.ignoresCancel = true
        _ = requests.start(9, instructions: "I", input: "D", timeoutMs: 5_000)
        until("running") { requests.runningForTest == 1 }
        requests.cancel(9)
        engine.release()
        until("the plan ended") { engine.finished == 1 && requests.runningForTest == 0 }
        bridge.events.sync {}
        XCTAssertTrue(replies(got).isEmpty)
        XCTAssertEqual(requests.countForTest, 0)
    }

    /// Review: an id still in flight is refused, so its task stays cancellable.
    func testAnIdAlreadyInFlightIsRefused() {
        let (requests, engine, bridge, got) = rig()
        engine.gated = true
        XCTAssertEqual(requests.start(5, instructions: "I", input: "D", timeoutMs: 5_000), VB_OK)
        XCTAssertEqual(requests.start(5, instructions: "I", input: "D", timeoutMs: 5_000), VB_ERR_FAILED)
        until("the plan began") { engine.plans == 1 }
        requests.cancel(5)
        until("the plan ended") { engine.finished == 1 }
        bridge.events.sync {}
        XCTAssertTrue(replies(got).isEmpty)
    }

    /// Review: a prewarm still running when a discard lands is dropped too
    /// (spec §6.3: sessions are discarded).
    func testAPrewarmThatOverlapsADiscardLeavesNoSession() {
        let (requests, engine, _, _) = rig()
        engine.duringPrewarm = { requests.cancelAll() }
        requests.prewarm(instructions: "I")
        XCTAssertEqual(engine.discards, 2, "cancelAll's own, and the one for the session made meanwhile")
    }

    /// Final review: a prewarm whose task starts after a discard (the epoch it was
    /// requested at is stale) leaves no session behind either.
    func testAPrewarmRequestedBeforeADiscardIsDroppedWhenItRunsLate() {
        let (requests, engine, _, _) = rig()
        let requestedAt = requests.currentEpoch
        requests.cancelAll()                    // lands before the prewarm task starts
        requests.prewarm(instructions: "I", requestedAt: requestedAt)
        XCTAssertEqual(engine.prewarms, 1)
        XCTAssertEqual(engine.discards, 2, "cancelAll's own, and the stale prewarm's session")
    }

    /// `copyText` is the one place C bytes become Swift text (spec §6.3).
    func testCopyTextHandlesEmptyNilAndInvalidBytes() {
        XCTAssertEqual(copyText(nil, 0), "")
        XCTAssertNil(copyText(nil, 3))
        XCTAssertNil([0xff, 0xfe].withUnsafeBufferPointer { copyText($0.baseAddress, 2) })
        XCTAssertNil([0x61, 0xc3].withUnsafeBufferPointer { copyText($0.baseAddress, 2) }, "a cut multi-byte character")
        XCTAssertEqual(Array("é".utf8).withUnsafeBufferPointer { copyText($0.baseAddress, 2) }, "é")
    }

    func testTheCEntryPointRefusesBadTextAndAnUninitializedBridge() {
        guard !Bridge.shared.isInitialized else { return }
        let bytes: [UInt8] = [0x61]
        XCTAssertEqual(bytes.withUnsafeBufferPointer { vb_model_plan(1, $0.baseAddress, 1, $0.baseAddress, 1, 5_000) },
                       VB_ERR_NOT_INITIALIZED)
    }

    /// Plan V4: model replies are not session events; no recording needs to be live.
    func testAModelReplyPassesWithNoSpeechSession() {
        XCTAssertFalse(Bridge.isStale(Event(generation: 5, kind: VB_EV_MODEL), current: 0))
        XCTAssertFalse(Bridge.isStale(Event(kind: VB_EV_MODEL_CAPS), current: 0))
    }
}
