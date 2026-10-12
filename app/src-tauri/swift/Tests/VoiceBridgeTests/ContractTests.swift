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

    /// Spec §4.2: the text is lent for the call; the receiver copies it.
    func testTextReachesTheCallbackAsBorrowedUtf8() {
        // Qualified: VoiceBridgeC's header also declares these names.
        XCTAssertEqual(VoiceBridge.vb_init(nil, copyText), VB_OK)
        XCTAssertEqual(VoiceBridge.vb_init(nil, copyText), VB_OK, "a second init changes nothing")
        Bridge.shared.emit(Event(kind: VB_EV_PROBE, text: "{\"é\":1}"))
        Bridge.shared.events.sync {}
        // The capabilities event may arrive in between; look for ours.
        XCTAssertTrue(copied.contains("{\"é\":1}"))
    }
}

private var copied: [String] = []

private func copyText(_ ctx: UnsafeMutableRawPointer?, _ event: UnsafePointer<vb_event>?) {
    guard let ev = event?.pointee, let text = ev.text else { return }
    copied.append(String(decoding: UnsafeBufferPointer(start: text, count: ev.text_len), as: UTF8.self))
}
