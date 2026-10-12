import XCTest
import VoiceBridgeC
@testable import VoiceBridge

final class ContractTests: XCTestCase {
    /// Rust mirrors this struct (`VbEvent`); both sides pin the layout.
    func testEventLayoutMatchesRust() {
        XCTAssertEqual(MemoryLayout<vb_event>.size, 56)
        XCTAssertEqual(MemoryLayout<vb_event>.stride, 56)
        XCTAssertEqual(MemoryLayout<vb_event>.offset(of: \vb_event.text), 24)
        XCTAssertEqual(MemoryLayout<vb_event>.offset(of: \vb_event.audio_ms), 40)
    }
}
