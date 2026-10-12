import XCTest
import VoiceBridgeC
@testable import VoiceBridge

/// Collects what the bridge delivers, from the event queue.
final class Box: @unchecked Sendable {
    private let lock = NSLock()
    private var items: [Event] = []
    func append(_ e: Event) { lock.lock(); items.append(e); lock.unlock() }
    var all: [Event] { lock.lock(); defer { lock.unlock() }; return items }
}

final class OutboxTests: XCTestCase {
    private func bridge() -> (Bridge, Box) {
        let bridge = Bridge()
        let box = Box()
        bridge.install { box.append($0) }
        return (bridge, box)
    }

    /// Review Focus 4: late events from an old generation are dropped (spec §10).
    func testSessionEventsOfAnotherGenerationAreDropped() {
        let (bridge, box) = bridge()
        bridge.setCurrent(5)
        bridge.emitSession(Event(generation: 4, kind: VB_EV_FINAL, text: "approve"))
        bridge.emitPartial(5, "de")
        bridge.emitSession(Event(generation: 0, kind: VB_EV_FINAL, text: "zero is never a session"))
        bridge.events.sync {}
        bridge.setCurrent(0)
        bridge.emitSession(Event(generation: 5, kind: VB_EV_FINAL, text: "deny"))
        bridge.events.sync {}
        XCTAssertEqual(box.all.map(\.generation), [5])
        XCTAssertEqual(box.all.map(\.kind), [VB_EV_PARTIAL])
    }

    /// The generation is checked at delivery: an event queued before a cancel is dropped.
    func testAnEventQueuedBeforeACancelIsDropped() {
        let (bridge, box) = bridge()
        bridge.setCurrent(7)
        bridge.events.suspend()
        bridge.emitSession(Event(generation: 7, kind: VB_EV_FINAL, text: "approve"))
        bridge.setCurrent(0)
        bridge.events.resume()
        bridge.events.sync {}
        XCTAssertTrue(box.all.isEmpty)
    }

    /// Review Focus 2, plan S5: the backlog is bounded, and an overflow is
    /// reported before anything that follows it.
    func testOverflowIsLatchedAndDeliveredFirst() {
        let (bridge, box) = bridge()
        bridge.events.suspend()
        for _ in 0..<(Bridge.capacity + 10) {
            bridge.emit(Event(kind: VB_EV_OTHER_KEY))
        }
        bridge.events.resume()
        bridge.events.sync {}
        XCTAssertEqual(box.all.first?.kind, VB_EV_OVERFLOW)
        XCTAssertEqual(box.all.count, Bridge.capacity + 1)
        bridge.emit(Event(kind: VB_EV_OTHER_KEY))
        bridge.events.sync {}
        XCTAssertEqual(box.all.filter { $0.kind == VB_EV_OVERFLOW }.count, 1, "the latch clears once delivered")
    }

    /// Plan S5, S18: partials and capture state keep only the latest.
    func testPartialsAndCaptureStateCoalesce() {
        let (bridge, box) = bridge()
        bridge.setCurrent(3)
        bridge.events.suspend()
        for i in 0..<10 { bridge.emitPartial(3, "word \(i)") }
        bridge.emitCapture(3, on: true)
        bridge.emitCapture(3, on: false)
        bridge.events.resume()
        bridge.events.sync {}
        XCTAssertEqual(box.all.map(\.kind), [VB_EV_CAPTURE, VB_EV_PARTIAL])
        XCTAssertEqual(box.all.first?.value, 0, "the latest capture state")
        XCTAssertEqual(box.all.last?.text, "word 9")
    }

    /// Review Focus 5: a lock is latched, never dropped, and goes first.
    func testALockIsNeverDropped() {
        let (bridge, box) = bridge()
        bridge.events.suspend()
        for _ in 0..<(Bridge.capacity + 10) { bridge.emit(Event(kind: VB_EV_OTHER_KEY)) }
        bridge.emitSystem(VB_SYS_SCREEN_LOCKED)
        bridge.events.resume()
        bridge.events.sync {}
        XCTAssertEqual(box.all.prefix(2).map(\.kind), [VB_EV_OVERFLOW, VB_EV_SYSTEM])
    }

    /// Spec §4.2: one drain block at a time, however many events arrive.
    func testOneDrainBlockAtATime() {
        let (bridge, _) = bridge()
        bridge.events.suspend()
        for _ in 0..<1000 { bridge.emitPartial(1, "x") }
        XCTAssertEqual(bridge.scheduledForTest, true)
        bridge.events.resume()
        bridge.events.sync {}
        XCTAssertEqual(bridge.scheduledForTest, false)
    }
}
