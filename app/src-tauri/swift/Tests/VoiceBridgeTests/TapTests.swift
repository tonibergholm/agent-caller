import CoreGraphics
import XCTest
import VoiceBridgeC
@testable import VoiceBridge

final class FakePort: TapPort, @unchecked Sendable {
    private let lock = NSLock()
    private var on = false
    private var ever = false
    private var gone = false
    var source: CFRunLoopSource? { nil }
    func enable(_ on: Bool) { lock.lock(); self.on = on; if on { ever = true }; lock.unlock() }
    var isEnabled: Bool { lock.lock(); defer { lock.unlock() }; return on }
    var wasEverEnabled: Bool { lock.lock(); defer { lock.unlock() }; return ever }
    var invalidated: Bool { lock.lock(); defer { lock.unlock() }; return gone }
    func invalidate() { lock.lock(); gone = true; on = false; lock.unlock() }
}

/// Hands out ports in order, from whichever tap thread asks.
final class Ports: @unchecked Sendable {
    private let lock = NSLock()
    private var queue: [FakePort]
    init(_ ports: [FakePort]) { queue = ports }
    func next() -> FakePort? { lock.lock(); defer { lock.unlock() }; return queue.isEmpty ? nil : queue.removeFirst() }
}

/// Plan S11, S26. Synthetic events and fake ports need no permission.
final class TapTests: XCTestCase {
    private func flags(_ keycode: CGKeyCode, _ raw: UInt64) -> CGEvent {
        let e = CGEvent(keyboardEventSource: nil, virtualKey: keycode, keyDown: true)!
        e.type = .flagsChanged
        e.flags = CGEventFlags(rawValue: raw)
        return e
    }

    func testHoldKeyDownUpAndChordsCarryNoKeyCode() {
        let bridge = Bridge()
        let box = Box()
        bridge.install { box.append($0) }
        let ctx = TapContext(keycode: 61, mask: 0x40, token: 1, bridge: bridge)
        let typed = CGEvent(keyboardEventSource: nil, virtualKey: 14, keyDown: true)! // "e"
        ctx.handle(.keyDown, typed) // not held: nothing
        ctx.handle(.flagsChanged, flags(61, 0x80040)) // Right Option down
        ctx.handle(.flagsChanged, flags(61, 0x80040)) // repeat: nothing
        ctx.handle(.keyDown, typed) // a chord
        ctx.handle(.flagsChanged, flags(56, 0x80042)) // Shift while held: a chord too
        ctx.handle(.flagsChanged, flags(61, 0x0)) // Right Option up
        ctx.handle(.flagsChanged, flags(58, 0x80020)) // Left Option is not the hold key
        bridge.events.sync {}
        XCTAssertEqual(box.all.map(\.kind), [VB_EV_HOLD, VB_EV_OTHER_KEY, VB_EV_OTHER_KEY, VB_EV_HOLD])
        XCTAssertEqual(box.all.map(\.value), [1, 0, 0, 0])
        XCTAssertEqual(Set(box.all.map(\.generation)), [1], "every event names its tap (S26)")
        XCTAssertTrue(box.all.allSatisfy { $0.code == 0 && $0.text == nil }, "no key code or text leaves the tap")
    }

    func testStartWithoutInputMonitoringIsRefused() {
        let tap = HoldTap(bridge: Bridge(), granted: { false }, create: { _ in
            XCTFail("never created")
            return nil
        })
        XCTAssertEqual(tap.start(keycode: 61, mask: 0x40).code, VB_ERR_INPUT_MONITORING_DENIED)
        XCTAssertFalse(tap.running)
    }

    /// S26: stop returns only after the tap thread exited; by then the port
    /// is disabled and invalidated.
    func testStopWaitsForTheTapThreadToExit() {
        let port = FakePort()
        let tap = HoldTap(bridge: Bridge(), granted: { true }, create: { _ in port })
        XCTAssertEqual(tap.start(keycode: 61, mask: 0x40).code, VB_OK)
        XCTAssertTrue(port.isEnabled)
        tap.stop()
        XCTAssertTrue(port.invalidated, "the thread finished before stop returned")
        XCTAssertFalse(tap.running)
    }

    /// S26: a start that timed out never leaves a tap running, even if the
    /// tap is created later.
    func testATimedOutStartNeverEnablesALateTap() {
        let gate = DispatchSemaphore(value: 0)
        let port = FakePort()
        let tap = HoldTap(bridge: Bridge(), granted: { true }, readyTimeout: .milliseconds(50), create: { _ in
            gate.wait()
            return port
        })
        XCTAssertEqual(tap.start(keycode: 61, mask: 0x40).code, VB_ERR_TAP_FAILED)
        gate.signal()
        let deadline = Date().addingTimeInterval(2)
        while !port.invalidated && Date() < deadline { usleep(2_000) }
        XCTAssertTrue(port.invalidated)
        XCTAssertFalse(port.wasEverEnabled)
        XCTAssertFalse(tap.running)
    }

    /// S26: a restart replaces the old tap only after the old thread exited.
    func testARestartReplacesTheOldTapAfterItExited() {
        let (first, second) = (FakePort(), FakePort())
        let ports = Ports([first, second])
        let tap = HoldTap(bridge: Bridge(), granted: { true }, create: { _ in ports.next() })
        let a = tap.start(keycode: 61, mask: 0x40)
        let b = tap.start(keycode: 58, mask: 0x20)
        XCTAssertEqual([a.code, b.code], [VB_OK, VB_OK])
        XCTAssertGreaterThan(b.tap, a.tap, "each tap has a new generation (S26)")
        XCTAssertTrue(first.invalidated)
        XCTAssertTrue(second.isEnabled)
        tap.stop()
        XCTAssertTrue(second.invalidated)
    }

    /// A context asked to stop forwards nothing more.
    func testAStoppedContextForwardsNothing() {
        let bridge = Bridge()
        let box = Box()
        bridge.install { box.append($0) }
        let ctx = TapContext(keycode: 61, mask: 0x40, token: 1, bridge: bridge)
        ctx.lock.lock()
        ctx.stopRequested = true
        ctx.lock.unlock()
        ctx.handle(.flagsChanged, flags(61, 0x80040))
        bridge.events.sync {}
        XCTAssertTrue(box.all.isEmpty)
    }
}
