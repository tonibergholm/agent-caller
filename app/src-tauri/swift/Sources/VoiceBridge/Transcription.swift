import AVFoundation
import Foundation

/// How a recording sounded, measured in Swift; Rust applies the rules (S10).
struct Heard: Sendable, Equatable {
    let text: String
    let audioMs: UInt64
    let voicedMs: UInt64
}

/// Transcription (spec §4.1, Transcriber). A protocol so the session
/// lifecycle is tested with a fake (Task 5C).
protocol SpeechWork: AnyObject, Sendable {
    func format() async -> AVAudioFormat?
    /// Starts reading `chunks`; returns once the analyzer runs.
    func start(_ chunks: AsyncStream<Chunk>, partial: @escaping @Sendable (String) -> Void) async throws
    /// Waits for the chunks to end, then for the final text.
    func finish() async throws -> Heard
    /// Stops all work now, also while `start` or `finish` waits. Terminal.
    func cancel() async
}

/// What `Transcription` needs from SpeechAnalyzer. A seam so the
/// cancellation rules are tested with a fake engine (plan S27).
protocol AnalyzerEngine: AnyObject, Sendable {
    func format() async -> AVAudioFormat?
    /// Starts analysis; returns once it runs. May stay suspended a while.
    func start() async throws
    func feed(_ buffer: AVAudioPCMBuffer)
    func endInput()
    /// (text, isFinal) pairs. Called at most once, after `start`.
    func results() -> AsyncThrowingStream<(String, Bool), Error>
    func finalize() async throws
    /// Idempotent.
    func cancel() async
}

/// Transcription over an engine. Cancellation is terminal (plan S27):
/// `cancel` sets the latch before anything else; `start` checks it on entry
/// and again after the engine's startup returns, and creates no reader or
/// pump once it is set; `finish` after a cancel throws. Actor state; `cancel`
/// runs while `start` or `finish` is suspended, because actors are reentrant
/// at each `await`.
actor Transcription: SpeechWork {
    private let engine: AnalyzerEngine
    private var cancelled = false
    private var failed = false
    private var reader: Task<String, Error>?
    private var pump: Task<Void, Never>?
    private var audio: Double = 0
    private var voiced: Double = 0

    init(engine: AnalyzerEngine) {
        self.engine = engine
    }

    func format() async -> AVAudioFormat? {
        await engine.format()
    }

    /// `partial` gets the live text at most every 100 ms, latest wins (S5).
    func start(_ chunks: AsyncStream<Chunk>, partial: @escaping @Sendable (String) -> Void) async throws {
        guard !cancelled else { throw CancellationError() }
        do {
            try await engine.start()
        } catch {
            failed = true
            await engine.cancel() // a failed startup leaves nothing running
            throw error
        }
        // `cancel` may have run while the engine started.
        guard !cancelled else {
            await engine.cancel()
            throw CancellationError()
        }
        let results = engine.results()
        reader = Task {
            var final = ""
            var last = ContinuousClock.now - .seconds(1)
            for try await (text, isFinal) in results {
                if isFinal {
                    final += text
                } else if last.duration(to: .now) >= .milliseconds(100) {
                    last = .now
                    partial(final + text)
                }
            }
            return final
        }
        pump = Task { await self.consume(chunks) }
    }

    private func consume(_ chunks: AsyncStream<Chunk>) async {
        for await chunk in chunks {
            if cancelled { break }
            audio += chunk.seconds
            if chunk.voiced { voiced += chunk.seconds }
            engine.feed(chunk.buffer)
        }
        engine.endInput()
    }

    func finish() async throws -> Heard {
        guard !cancelled, !failed else { throw CancellationError() }
        await pump?.value
        let text: String
        do {
            try await engine.finalize()
            text = try await reader?.value ?? ""
        } catch {
            // Every error path ends the analyzer and the reader.
            failed = true
            reader?.cancel()
            await engine.cancel()
            throw error
        }
        guard !cancelled else { throw CancellationError() }
        return Heard(text: text, audioMs: UInt64(audio * 1000), voicedMs: UInt64(voiced * 1000))
    }

    func cancel() async {
        guard !cancelled else { return }
        cancelled = true
        pump?.cancel()
        reader?.cancel()
        await engine.cancel()
    }
}

#if compiler(>=6.2)
import Speech

/// SpeechAnalyzer and SpeechTranscriber (spec §3.6: on-device only).
@available(macOS 26.0, *)
final class SpeechEngine: AnalyzerEngine, @unchecked Sendable {
    private let transcriber: SpeechTranscriber
    private let analyzer: SpeechAnalyzer
    private let inputs: AsyncStream<AnalyzerInput>
    private let feedSink: AsyncStream<AnalyzerInput>.Continuation

    init(locale: Locale) {
        transcriber = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [.volatileResults], attributeOptions: [])
        analyzer = SpeechAnalyzer(modules: [transcriber])
        (inputs, feedSink) = AsyncStream.makeStream(of: AnalyzerInput.self, bufferingPolicy: .bufferingNewest(64))
    }

    func format() async -> AVAudioFormat? {
        await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber])
    }

    func start() async throws {
        try await analyzer.start(inputSequence: inputs)
    }

    func feed(_ buffer: AVAudioPCMBuffer) {
        feedSink.yield(AnalyzerInput(buffer: buffer))
    }

    func endInput() {
        feedSink.finish()
    }

    /// The forwarding task exists only once `results` is called, which
    /// `Transcription` does after its latch check.
    func results() -> AsyncThrowingStream<(String, Bool), Error> {
        let source = transcriber.results
        return AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    for try await r in source {
                        continuation.yield((String(r.text.characters), r.isFinal))
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    func finalize() async throws {
        try await analyzer.finalizeAndFinishThroughEndOfInput()
    }

    func cancel() async {
        feedSink.finish()
        await analyzer.cancelAndFinishNow()
    }
}
#endif
