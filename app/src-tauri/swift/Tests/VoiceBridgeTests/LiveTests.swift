import AVFoundation
import XCTest
@testable import VoiceBridge

/// Run by hand on a Mac (spec §10): `swift test --package-path app/src-tauri/swift --filter LiveTests`.
/// Skipped in `npm run verify`.
final class LiveTests: XCTestCase {
    #if compiler(>=6.2)
    func testTranscribesASpokenClip() async throws {
        guard #available(macOS 26.0, *) else { throw XCTSkip("needs macOS 26") }
        guard let locale = await Locales.resolve(), await Locales.isInstalled(locale) else {
            throw XCTSkip("no English speech model installed")
        }
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("vb-live-\(UUID().uuidString).aiff")
        defer { try? FileManager.default.removeItem(at: url) }
        let say = Process()
        say.executableURL = URL(fileURLWithPath: "/usr/bin/say")
        say.arguments = ["-o", url.path, "approve the request"]
        try say.run()
        say.waitUntilExit()
        let file = try AVAudioFile(forReading: url)
        let transcription = Transcription(engine: SpeechEngine(locale: locale))
        let maybeFormat = await transcription.format()
        let format = try XCTUnwrap(maybeFormat)
        let converter = try XCTUnwrap(AVAudioConverter(from: file.processingFormat, to: format))
        let (chunks, sink) = AsyncStream.makeStream(of: Chunk.self)
        try await transcription.start(chunks) { _ in }
        while file.framePosition < file.length {
            let buffer = try XCTUnwrap(AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: 4096))
            try file.read(into: buffer)
            if let chunk = Chunk.make(buffer, converter: converter, to: format) { sink.yield(chunk) }
        }
        sink.finish()
        let heard = try await transcription.finish()
        XCTAssertTrue(heard.text.lowercased().contains("approve"))
        let seconds = Double(file.length) / file.processingFormat.sampleRate
        XCTAssertEqual(Double(heard.audioMs) / 1000, seconds, accuracy: 0.1)
        XCTAssertGreaterThan(heard.voicedMs, 300)
    }

    /// `cancel` ends a transcription that is waiting in `finish`.
    func testCancelEndsAWaitingFinish() async throws {
        guard #available(macOS 26.0, *) else { throw XCTSkip("needs macOS 26") }
        guard let locale = await Locales.resolve(), await Locales.isInstalled(locale) else {
            throw XCTSkip("no English speech model installed")
        }
        let transcription = Transcription(engine: SpeechEngine(locale: locale))
        let (chunks, _sink) = AsyncStream.makeStream(of: Chunk.self) // never finishes
        try await transcription.start(chunks) { _ in }
        let finishing = Task { try? await transcription.finish() }
        await transcription.cancel()
        _ = await finishing.value // returns: nothing hangs
        _ = _sink
    }
    #endif

    func testCaptureStartsAndStops() throws {
        guard AVCaptureDevice.authorizationStatus(for: .audio) == .authorized else {
            throw XCTSkip("the microphone is not granted to the test runner")
        }
        let format = try XCTUnwrap(AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16_000, channels: 1, interleaved: true))
        let capture = Capture()
        _ = try capture.start(to: format) {}
        capture.stop()
        capture.stop() // twice is safe
    }
}
