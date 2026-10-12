import AVFoundation

/// The microphone (spec §4.1, Capture).
protocol AudioCapture: AnyObject, Sendable {
    /// Starts the microphone. The stream ends when `stop` runs.
    func start(to format: AVAudioFormat, onLost: @escaping @Sendable () -> Void) throws -> AsyncStream<Chunk>
    /// Stops the microphone now and ends the stream. Synchronous, idempotent,
    /// any thread; returns after the engine stopped.
    func stop()
}

/// AVAudioEngine capture. The tap measures and converts one buffer and
/// yields it into a bounded stream. It takes no lock of ours and never calls
/// Rust (spec §4.2). Conversion in the tap follows Apple's SpeechAnalyzer
/// sample. `lock` guards `sink`, `observer` and `running`.
final class Capture: AudioCapture, @unchecked Sendable {
    private let lock = NSLock()
    private let engine = AVAudioEngine()
    private var sink: AsyncStream<Chunk>.Continuation?
    private var observer: NSObjectProtocol?
    private var running = false

    func start(to format: AVAudioFormat, onLost: @escaping @Sendable () -> Void) throws -> AsyncStream<Chunk> {
        lock.lock()
        defer { lock.unlock() }
        guard !running else { throw BridgeError.micUnavailable }
        let input = engine.inputNode
        let micFormat = input.outputFormat(forBus: 0)
        guard micFormat.sampleRate > 0, micFormat.channelCount > 0,
              let converter = AVAudioConverter(from: micFormat, to: format) else {
            throw BridgeError.micUnavailable
        }
        let (stream, sink) = AsyncStream.makeStream(of: Chunk.self, bufferingPolicy: .bufferingNewest(64))
        input.installTap(onBus: 0, bufferSize: 4096, format: micFormat) { buffer, _ in
            if let chunk = Chunk.make(buffer, converter: converter, to: format),
               case .dropped = sink.yield(chunk) {
                // The transcriber fell behind and a chunk was lost: words
                // would silently go missing. End the session instead of
                // transcribing audio with a gap in it.
                DispatchQueue.global().async { onLost() }
            }
        }
        // The device went away or changed (spec §9: "Microphone unavailable").
        observer = NotificationCenter.default.addObserver(forName: .AVAudioEngineConfigurationChange, object: engine, queue: nil) { _ in
            // Off the engine's posting thread: `onLost` takes the session
            // lock, which a stop or cancel may hold while it stops the engine.
            DispatchQueue.global().async { onLost() }
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            sink.finish()
            if let observer { NotificationCenter.default.removeObserver(observer) }
            observer = nil
            throw BridgeError.micUnavailable
        }
        self.sink = sink
        running = true
        return stream
    }

    func stop() {
        lock.lock()
        defer { lock.unlock() }
        guard running else { return }
        running = false
        if let observer { NotificationCenter.default.removeObserver(observer) }
        observer = nil
        engine.stop()
        engine.inputNode.removeTap(onBus: 0)
        sink?.finish()
        sink = nil
    }
}
