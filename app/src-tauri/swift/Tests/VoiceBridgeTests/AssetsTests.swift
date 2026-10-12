import XCTest
import VoiceBridgeC
@testable import VoiceBridge

final class AssetsTests: XCTestCase {
    /// Capability queries can finish out of order. An older result never
    /// replaces a newer one, in the cache or on the wire.
    /// Barrier: the first check waits on a gate until the second finished.
    func testAnOlderCapabilityResultNeverOverwritesANewerOne() async {
        let bridge = Bridge()
        let box = Box()
        bridge.install { [box] in box.append($0) }
        let gate = Gate()
        let calls = Counter()
        let assets = Assets(bridge: bridge) {
            let n = calls.next()
            if n == 1 {
                await gate.wait()
                return (Event(kind: VB_EV_CAPS, code: VB_UA_ASSETS_MISSING), nil)
            }
            return (Event(kind: VB_EV_CAPS, code: VB_UA_NONE), Locale(identifier: "en_US"))
        }
        let older = assets.query()
        let newer = assets.query()
        await newer.value
        gate.release()
        await older.value
        bridge.events.sync {}
        XCTAssertEqual(assets.caps?.code, VB_UA_NONE, "the newer result stays")
        XCTAssertNotNil(assets.locale)
        XCTAssertEqual(box.all.filter { $0.kind == VB_EV_CAPS }.map(\.code), [VB_UA_NONE], "the stale one is not sent")
    }
}

final class Counter: @unchecked Sendable {
    private let lock = NSLock()
    private var n = 0
    func next() -> Int { lock.lock(); defer { lock.unlock() }; n += 1; return n }
}
