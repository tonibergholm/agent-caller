import AVFoundation
import XCTest
import VoiceBridgeC
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

    /// Spec §10 "synthesis runs", plan V17: off the main thread, at volume 0.
    func testSynthesisRunsOffTheMainThread() throws {
        let synth = AVSynth()
        synth.volume = 0
        let done = expectation(description: "spoken")
        DispatchQueue(label: "live.speaker").async {
            synth.speak("test", voice: nil, rate: AVSpeechUtteranceDefaultSpeechRate) { finished in
                XCTAssertTrue(finished)
                done.fulfill()
            }
        }
        wait(for: [done], timeout: 10)
    }

    /// Plan V17: `say` and `stop` must not need the main thread. The main
    /// thread is blocked here, as a sync Tauri command waiting on the
    /// controller's lock would block it. A timeout means a hidden main-thread
    /// hop, and a deadlock in the app.
    func testSayAndStopNeverWaitForTheMainThread() throws {
        let synth = AVSynth()
        synth.volume = 0
        let speaker = Speaker(engine: synth, bridge: Bridge())
        let finished = DispatchSemaphore(value: 0)
        DispatchQueue(label: "live.lock-holder").async {
            speaker.say(1, text: "test", voice: nil, rate: VB_RATE_NORMAL)
            speaker.stop()
            finished.signal()
        }
        XCTAssertEqual(finished.wait(timeout: .now() + 5), .success, "say or stop waited for the main thread")
    }
}
