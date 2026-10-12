import AVFoundation
import XCTest
@testable import VoiceBridge

final class FakeEngine: AnalyzerEngine, @unchecked Sendable {
    let startEntered = Gate()
    let startGate = Gate()
    var gateStart = false
    private let lock = NSLock()
    private var counts = (starts: 0, results: 0, cancels: 0)

    var starts: Int { lock.lock(); defer { lock.unlock() }; return counts.starts }
    var resultCalls: Int { lock.lock(); defer { lock.unlock() }; return counts.results }
    var cancels: Int { lock.lock(); defer { lock.unlock() }; return counts.cancels }

    func format() async -> AVAudioFormat? { AVAudioFormat(standardFormatWithSampleRate: 16_000, channels: 1) }

    private func bump(_ f: (inout (starts: Int, results: Int, cancels: Int)) -> Void) {
        lock.lock(); f(&counts); lock.unlock()
    }

    func start() async throws {
        bump { $0.starts += 1 }
        startEntered.release()
        if gateStart { await startGate.wait() }
    }

    func feed(_ buffer: AVAudioPCMBuffer) {}
    func endInput() {}

    func results() -> AsyncThrowingStream<(String, Bool), Error> {
        bump { $0.results += 1 }
        return AsyncThrowingStream { c in
            c.yield(("approve", true))
            c.finish()
        }
    }

    func finalize() async throws {}

    func cancel() async {
        bump { $0.cancels += 1 }
    }
}

/// Plan S27: cancellation is terminal, whatever it interleaves with.
final class TranscriptionTests: XCTestCase {
    func testCancelBeforeStartIsTerminal() async {
        let engine = FakeEngine()
        let t = Transcription(engine: engine)
        await t.cancel()
        let (chunks, _) = AsyncStream.makeStream(of: Chunk.self)
        do {
            try await t.start(chunks) { _ in }
            XCTFail("start after cancel must throw")
        } catch {}
        XCTAssertEqual(engine.starts, 0, "the analyzer never starts")
        XCTAssertEqual(engine.resultCalls, 0, "no reader")
        do {
            _ = try await t.finish()
            XCTFail("finish after cancel must throw")
        } catch {}
    }

    /// Cancel lands while `start` is suspended inside the engine's startup.
    func testCancelDuringSuspendedStartupIsTerminal() async {
        let engine = FakeEngine()
        engine.gateStart = true
        let t = Transcription(engine: engine)
        let (chunks, _) = AsyncStream.makeStream(of: Chunk.self)
        let starting = Task { try await t.start(chunks) { _ in } }
        await engine.startEntered.wait() // the barrier: start is suspended in the engine
        await t.cancel()
        engine.startGate.release()
        if case .success = await starting.result {
            XCTFail("start must throw after a cancel during startup")
        }
        XCTAssertEqual(engine.resultCalls, 0, "no reader or pump was created")
        XCTAssertGreaterThanOrEqual(engine.cancels, 1)
    }

    func testAnUncancelledRunTranscribes() async throws {
        let t = Transcription(engine: FakeEngine())
        let (chunks, sink) = AsyncStream.makeStream(of: Chunk.self)
        try await t.start(chunks) { _ in }
        sink.finish()
        let heard = try await t.finish()
        XCTAssertEqual(heard.text, "approve")
    }
}

/// Carry-over from Task 1A's review: an error path leaves nothing running.
final class FailingEngine: AnalyzerEngine, @unchecked Sendable {
    private let lock = NSLock()
    private var cancelCount = 0
    var cancels: Int { lock.lock(); defer { lock.unlock() }; return cancelCount }
    struct Boom: Error {}
    var failStart = false

    func format() async -> AVAudioFormat? { nil }
    func start() async throws { if failStart { throw Boom() } }
    func feed(_ buffer: AVAudioPCMBuffer) {}
    func endInput() {}
    func results() -> AsyncThrowingStream<(String, Bool), Error> { AsyncThrowingStream { $0.finish() } }
    func finalize() async throws { throw Boom() }
    func cancel() async { bump() }
    private func bump() { lock.lock(); cancelCount += 1; lock.unlock() }
}

final class TranscriptionErrorPathTests: XCTestCase {
    func testAFailedStartupCancelsTheEngine() async {
        let engine = FailingEngine()
        engine.failStart = true
        let t = Transcription(engine: engine)
        let (chunks, _) = AsyncStream.makeStream(of: Chunk.self)
        do { try await t.start(chunks) { _ in }; XCTFail("must throw") } catch {}
        XCTAssertEqual(engine.cancels, 1)
    }

    func testAFailedFinalizeCancelsTheEngine() async throws {
        let engine = FailingEngine()
        let t = Transcription(engine: engine)
        let (chunks, sink) = AsyncStream.makeStream(of: Chunk.self)
        try await t.start(chunks) { _ in }
        sink.finish()
        do { _ = try await t.finish(); XCTFail("must throw") } catch {}
        XCTAssertEqual(engine.cancels, 1)
    }
}
