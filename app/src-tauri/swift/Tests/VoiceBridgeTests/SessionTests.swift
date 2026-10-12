import AVFoundation
import XCTest
import VoiceBridgeC
@testable import VoiceBridge

final class FakeCapture: AudioCapture, @unchecked Sendable {
    private let lock = NSLock()
    private var sink: AsyncStream<Chunk>.Continuation?
    private var startCount = 0
    private var stopGate: DispatchSemaphore?
    /// Signalled when a blocked `stop` is entered: the test's barrier.
    let stopEntered = DispatchSemaphore(value: 0)

    /// A blocked `stop` has been entered and not yet released.
    var isStopping: Bool { lock.lock(); defer { lock.unlock() }; return stopGate != nil && sink != nil }

    /// The next `stop` waits inside, until `releaseStop`.
    func blockStop() { lock.lock(); stopGate = DispatchSemaphore(value: 0); lock.unlock() }
    func releaseStop() { lock.lock(); let g = stopGate; lock.unlock(); g?.signal() }

    var running: Bool { lock.lock(); defer { lock.unlock() }; return sink != nil }
    var starts: Int { lock.lock(); defer { lock.unlock() }; return startCount }

    func start(to format: AVAudioFormat, onLost: @escaping @Sendable () -> Void) throws -> AsyncStream<Chunk> {
        lock.lock()
        defer { lock.unlock() }
        startCount += 1
        let (stream, sink) = AsyncStream.makeStream(of: Chunk.self)
        self.sink = sink
        return stream
    }

    func stop() {
        lock.lock()
        let gate = stopGate
        lock.unlock()
        if let gate {
            stopEntered.signal()
            gate.wait()
            lock.lock()
            stopGate = nil
            lock.unlock()
        }
        lock.lock()
        let s = sink
        sink = nil
        lock.unlock()
        s?.finish()
    }
}

final class FakeWork: SpeechWork, @unchecked Sendable {
    let formatGate = Gate("formatGate")
    let finishGate = Gate("finishGate")
    var gateFormat = false
    var hangFinish = false
    private let lock = NSLock()
    private var chunks: AsyncStream<Chunk>?
    private var cancelledFlag = false

    var cancelled: Bool { lock.lock(); defer { lock.unlock() }; return cancelledFlag }

    func format() async -> AVAudioFormat? {
        if gateFormat { await formatGate.wait() }
        return AVAudioFormat(standardFormatWithSampleRate: 16_000, channels: 1)
    }

    func start(_ chunks: AsyncStream<Chunk>, partial: @escaping @Sendable (String) -> Void) async throws {
        lock.lock()
        self.chunks = chunks
        lock.unlock()
    }

    func finish() async throws -> Heard {
        lock.lock()
        let c = chunks
        lock.unlock()
        if let c { for await _ in c {} }
        if hangFinish {
            await finishGate.wait()
            throw CancellationError()
        }
        return Heard(text: "approve", audioMs: 900, voicedMs: 600)
    }

    func cancel() async {
        lock.lock()
        cancelledFlag = true
        lock.unlock()
        finishGate.release()
    }
}

final class SessionBox: @unchecked Sendable {
    private let lock = NSLock()
    private var session: Session?
    func set(_ s: Session) { lock.lock(); session = s; lock.unlock() }
    func get() -> Session? { lock.lock(); defer { lock.unlock() }; return session }
}

final class SessionTests: XCTestCase {
    private func until(_ what: String, _ cond: () -> Bool, file: StaticString = #filePath, line: UInt = #line) {
        let deadline = Date().addingTimeInterval(2)
        while !cond() {
            if Date() > deadline { return XCTFail("timed out: \(what)", file: file, line: line) }
            usleep(2_000)
        }
    }

    private final class Rig {
        let bridge = Bridge()
        let box = Box()
        let work = FakeWork()
        let admission: MicAdmission
        /// Runs inside each session right after capture starts (S18).
        var onCaptureStarted: ((Session) -> Void)?
        private let lock = NSLock()
        private var made: [FakeCapture] = []
        private var finished: Set<UInt64> = []
        lazy var sessions: Sessions = {
            let s = Sessions(bridge: bridge, admission: admission) { [weak self] g in
                guard let self else { return nil }
                let capture = FakeCapture()
                self.lock.lock(); self.made.append(capture); self.lock.unlock()
                let session = Session(generation: g, capture: capture, work: self.work, bridge: self.bridge)
                session.onCaptureStartedForTest = self.onCaptureStarted
                return session
            }
            s.onDone = { [weak self] g in self?.lock.lock(); self?.finished.insert(g); self?.lock.unlock() }
            return s
        }()

        init(admission: MicAdmission = MicAdmission()) {
            self.admission = admission
            bridge.install { [box] in box.append($0) }
        }

        func capture(_ i: Int) -> FakeCapture? { lock.lock(); defer { lock.unlock() }; return i < made.count ? made[i] : nil }
        func done(_ g: UInt64) -> Bool { lock.lock(); defer { lock.unlock() }; return finished.contains(g) }
        /// `on` reached the sink. The slot coalesces, so wait for it before `off`.
        func captureOnDelivered() -> Bool {
            bridge.events.sync {}
            return box.all.contains { $0.kind == VB_EV_CAPTURE && $0.value == 1 }
        }
        func kinds() -> [UInt32] { bridge.events.sync {}; return box.all.map(\.kind) }
    }

    /// Plan S4: stop ends capture before it returns; the finishing session
    /// stays tracked, and cancel still reaches it.
    func testStopEndsCaptureAndTheFinishingSessionStaysCancellable() {
        let r = Rig()
        r.work.hangFinish = true
        XCTAssertEqual(r.sessions.start(1, maxMs: 60_000), VB_OK)
        until("capture on") { r.captureOnDelivered() }
        r.sessions.stop(1)
        XCTAssertFalse(r.capture(0)!.running, "the microphone stopped before stop returned")
        XCTAssertEqual(r.sessions.count, 1, "the finishing session is still tracked")
        r.sessions.cancel(1)
        until("work cancelled") { r.work.cancelled }
        until("session done") { r.done(1) }
        XCTAssertEqual(r.sessions.count, 0)
        XCTAssertEqual(r.kinds().filter { $0 == VB_EV_CAPTURE }.count, 2, "on, then off")
    }

    /// Review Focus 5: a lock reaches a session that is still finishing.
    func testCancelAllReachesFinishingSessions() {
        let r = Rig()
        r.work.hangFinish = true
        _ = r.sessions.start(1, maxMs: 60_000)
        until("capture") { r.capture(0)?.running == true }
        r.sessions.stop(1)
        r.sessions.cancelAll()
        until("work cancelled") { r.work.cancelled }
        until("session done") { r.done(1) }
        XCTAssertFalse(r.kinds().contains(VB_EV_FINAL))
    }

    /// Plan S4: a new start stops the old capture before it returns.
    func testANewStartStopsTheOldCaptureFirst() {
        let r = Rig()
        _ = r.sessions.start(1, maxMs: 60_000)
        until("capture 1") { r.capture(0)?.running == true }
        _ = r.sessions.start(2, maxMs: 60_000)
        XCTAssertFalse(r.capture(0)!.running, "stopped inside start(2)")
        until("capture 2") { r.capture(1)?.running == true }
        XCTAssertEqual(r.bridge.currentGeneration, 2)
    }

    func testCancelBeforeCaptureNeverOpensTheMicrophone() {
        let r = Rig()
        r.work.gateFormat = true
        _ = r.sessions.start(1, maxMs: 60_000)
        r.sessions.cancel(1)
        r.work.formatGate.release()
        until("session done") { r.done(1) }
        XCTAssertEqual(r.capture(0)?.starts, 0)
        XCTAssertFalse(r.kinds().contains(VB_EV_CAPTURE))
    }

    /// A release before the microphone opened ends with nothing heard,
    /// which Rust ignores as too short.
    func testAReleaseBeforeCaptureEndsWithNothingHeard() {
        let r = Rig()
        r.work.gateFormat = true
        _ = r.sessions.start(1, maxMs: 60_000)
        r.sessions.stop(1)
        r.work.formatGate.release()
        until("session done") { r.done(1) }
        XCTAssertEqual(r.capture(0)?.starts, 0)
        _ = r.kinds() // flush the event queue
        let final = r.box.all.first { $0.kind == VB_EV_FINAL }
        XCTAssertEqual(final?.audioMs, 0)
    }

    /// Plan S9: the failsafe counts from capture start, not from the call,
    /// and stops capture. Barrier: `format()` is held longer than the cap, so
    /// a timer that began at the call would already have fired.
    func testTheFailsafeCountsFromCaptureStartAndStopsCapture() {
        let r = Rig()
        r.work.gateFormat = true
        _ = r.sessions.start(1, maxMs: 200)
        usleep(400_000) // longer than the cap: the timer must not have started
        r.work.formatGate.release()
        until("capture") { r.capture(0)?.starts == 1 }
        XCTAssertEqual(r.capture(0)?.running, true, "still running right after it opened")
        until("failsafe") { r.capture(0)?.running == false }
        until("session done") { r.done(1) }
    }

    func testAFinalCarriesItsMeasurements() {
        let r = Rig()
        _ = r.sessions.start(1, maxMs: 60_000)
        until("capture") { r.capture(0)?.running == true }
        r.sessions.stop(1)
        until("final") { r.box.all.contains { $0.kind == VB_EV_FINAL } }
        let final = r.box.all.first { $0.kind == VB_EV_FINAL }!
        XCTAssertEqual(final.generation, 1)
        XCTAssertEqual(final.text, "approve")
        XCTAssertEqual(final.audioMs, 900)
        XCTAssertEqual(final.voicedMs, 600)
    }

    func testStartRefusesGenerationZero() {
        XCTAssertEqual(Rig().sessions.start(0, maxMs: 60_000), VB_ERR_FAILED)
    }

    /// S4: a lock holds the registry's transition until every old capture
    /// has stopped, so a start racing it cannot open a microphone first.
    /// Barrier: the old capture's `stop` blocks until the test releases it.
    func testALockFinishesStoppingCaptureBeforeAStartCanRun() {
        let r = Rig()
        _ = r.sessions.start(1, maxMs: 60_000)
        until("capture 1") { r.capture(0)?.running == true }
        let old = r.capture(0)!
        old.blockStop()
        let locking = DispatchGroup()
        DispatchQueue.global().async(group: locking) { r.sessions.cancelAll() }
        old.stopEntered.wait() // barrier: cancelAll is inside the old capture's stop
        let starting = DispatchGroup()
        var started: Int32 = -1
        DispatchQueue.global().async(group: starting) { started = r.sessions.start(2, maxMs: 60_000) }
        until("the start is blocked on the transition") { r.sessions.startsWaitingForTest == 1 }
        XCTAssertNil(r.capture(1), "no replacement session, so no replacement capture, before the release")
        XCTAssertTrue(old.isStopping, "the old capture is still stopping")
        old.releaseStop()
        locking.wait()
        starting.wait()
        XCTAssertEqual(started, VB_OK)
        XCTAssertFalse(old.running)
        until("capture 2") { r.capture(1)?.running == true }
    }

    /// S18: `on` is published inside the capture transition, under the
    /// session lock, so a racing release can only publish `off` after it.
    /// Barrier: the hook runs between the capture start and the publish,
    /// and starts the release there.
    func testCaptureOnIsPublishedBeforeARacingRelease() {
        let r = Rig()
        let heldAtPublish = Box()
        let current = SessionBox()
        let releasing = DispatchGroup()
        r.onCaptureStarted = { session in
            current.set(session)
            DispatchQueue.global().async(group: releasing) { r.sessions.stop(1) }
        }
        // Observes the session lock at the moment `on` is published.
        r.bridge.onEmitCaptureForTest = { _, on in
            if on { heldAtPublish.append(Event(kind: current.get()?.lockHeldForTest == true ? 1 : 0)) }
        }
        _ = r.sessions.start(1, maxMs: 60_000)
        until("capture") { r.capture(0)?.starts == 1 }
        releasing.wait()
        _ = r.kinds() // flush the event queue
        XCTAssertEqual(heldAtPublish.all.map(\.kind), [1], "`on` is published while the session lock is held")
        let values = r.box.all.filter { $0.kind == VB_EV_CAPTURE }.map(\.value)
        XCTAssertEqual(values.last, 0, "the last state is off")
        if let off = values.firstIndex(of: 0) {
            XCTAssertFalse(values[off...].contains(1), "never on after off")
        }
    }

    /// S20: the probe and voice sessions share one microphone admission,
    /// whichever starts first.
    func testTheProbeAndSessionsShareOneMicrophoneAdmission() {
        let admission = MicAdmission()
        let r = Rig(admission: admission)
        XCTAssertTrue(admission.claimForProbe())
        XCTAssertEqual(r.sessions.start(1, maxMs: 60_000), VB_ERR_BUSY, "probe first: the session is refused")
        admission.releaseProbe()
        XCTAssertEqual(r.sessions.start(2, maxMs: 60_000), VB_OK)
        XCTAssertFalse(admission.claimForProbe(), "session first: the probe is refused")
        r.sessions.cancel(2)
        XCTAssertTrue(admission.claimForProbe(), "free again once no session is left")
    }
}
