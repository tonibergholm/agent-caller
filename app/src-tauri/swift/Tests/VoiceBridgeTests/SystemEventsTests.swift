import XCTest
@testable import VoiceBridge

/// Review finding: a wake is not an unlock. Waking a locked Mac fires the wake
/// notifications while the lock screen is still up, so only a verified
/// "not locked" may let alerts speak again; anything else stays closed.
final class SystemEventsTests: XCTestCase {
    func testOnlyAVerifiedUnlockedScreenResumes() {
        XCTAssertTrue(SystemEvents.mayResume(screenLocked: false))
    }

    func testALockedScreenNeverResumes() {
        XCTAssertFalse(SystemEvents.mayResume(screenLocked: true))
    }

    /// Fail closed: if the session state cannot be read, alerts stay silent
    /// until the explicit unlock notification.
    func testAnUnknownStateNeverResumes() {
        XCTAssertFalse(SystemEvents.mayResume(screenLocked: nil))
    }

    func testTheSessionDictionaryMapsToTheLockedState() {
        XCTAssertEqual(SystemEvents.screenLocked(from: ["CGSSessionScreenIsLocked": true]), true)
        XCTAssertEqual(SystemEvents.screenLocked(from: ["CGSSessionScreenIsLocked": false]), false)
        // The key is absent while the screen is not locked on some releases; an
        // empty dictionary is a readable session, a missing one is unknown.
        XCTAssertEqual(SystemEvents.screenLocked(from: [:]), false)
        XCTAssertNil(SystemEvents.screenLocked(from: nil))
    }
}
