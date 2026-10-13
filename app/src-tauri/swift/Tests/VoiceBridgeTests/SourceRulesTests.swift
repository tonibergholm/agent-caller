import XCTest

/// Spec §3.7 and §6.3 for Swift: nothing in the bridge logs, no error's
/// text is ever read, and no cloud model can be reached (review item 10:
/// request details go to the on-device model only).
final class SourceRulesTests: XCTestCase {
    static let banned = [
        "print(", "NSLog", "os_log", "Logger(", "localizedDescription",
        "debugDescription", "dump(", "fputs",
        // Cloud models in the macOS 27 SDK; add any new cloud model type here.
        "PrivateCloudCompute",
        // The model is always named: `SystemLanguageModel.default`.
        "model: .default",
    ]

    func testTheBridgeNeverLogsReadsErrorTextOrUsesACloudModel() throws {
        var dir = URL(fileURLWithPath: #filePath)
        for _ in 0..<2 { dir.deleteLastPathComponent() } // up to Tests
        dir = dir.deletingLastPathComponent().appendingPathComponent("Sources/VoiceBridge")
        let walker = try XCTUnwrap(FileManager.default.enumerator(at: dir, includingPropertiesForKeys: nil))
        let files = walker.compactMap { $0 as? URL }.filter { $0.pathExtension == "swift" }
        XCTAssertFalse(files.isEmpty)
        for file in files {
            let text = try String(contentsOf: file, encoding: .utf8)
            for banned in SourceRulesTests.banned {
                XCTAssertFalse(text.contains(banned), "\(file.lastPathComponent) contains \(banned)")
            }
        }
    }
}
