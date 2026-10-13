import AVFoundation
import Foundation
import VoiceBridgeC

struct VoiceInfo: Encodable, Equatable {
    let id: String
    let name: String
    let language: String
}

/// Speech synthesis, behind a protocol so tests run without audio.
protocol SynthEngine: AnyObject, Sendable {
    /// `done(true)` when it finished, `done(false)` if stopped or failed;
    /// once, on any thread.
    func speak(_ text: String, voice: String?, rate: Float, done: @escaping @Sendable (Bool) -> Void)
    /// Stops at once. Audio already handed to the output may still play
    /// briefly; `isSpeaking` turns false when it is done.
    func stop()
    var isSpeaking: Bool { get }
    func voices() -> [VoiceInfo]
}

/// AVSpeechSynthesizer. `lock` guards `done`.
final class AVSynth: NSObject, SynthEngine, AVSpeechSynthesizerDelegate, @unchecked Sendable {
    private let synth = AVSpeechSynthesizer()
    private let lock = NSLock()
    private var done: [ObjectIdentifier: @Sendable (Bool) -> Void] = [:]
    /// Live tests only.
    var volume: Float = 1

    override init() {
        super.init()
        synth.delegate = self
    }

    func speak(_ text: String, voice: String?, rate: Float, done: @escaping @Sendable (Bool) -> Void) {
        let utterance = AVSpeechUtterance(string: text)
        utterance.voice = voice.flatMap { AVSpeechSynthesisVoice(identifier: $0) } ?? AVSynth.english()
        utterance.rate = rate
        utterance.volume = volume
        lock.withLock { self.done[ObjectIdentifier(utterance)] = done }
        synth.speak(utterance)
    }

    func stop() {
        synth.stopSpeaking(at: .immediate)
    }

    var isSpeaking: Bool { synth.isSpeaking }

    func voices() -> [VoiceInfo] {
        AVSpeechSynthesisVoice.speechVoices()
            .filter { $0.language.hasPrefix("en") }
            .map { VoiceInfo(id: $0.identifier, name: $0.name, language: $0.language) }
            .sorted { ($0.language, $0.name) < ($1.language, $1.name) }
    }

    /// The templates are English (spec §7): the system language if it is
    /// English, else en-US (owner decision 5).
    static func english() -> AVSpeechSynthesisVoice? {
        let current = AVSpeechSynthesisVoice.currentLanguageCode()
        return AVSpeechSynthesisVoice(language: current.hasPrefix("en") ? current : "en-US")
    }

    func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didFinish utterance: AVSpeechUtterance) {
        finish(utterance, true)
    }

    func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didCancel utterance: AVSpeechUtterance) {
        finish(utterance, false)
    }

    private func finish(_ utterance: AVSpeechUtterance, _ ok: Bool) {
        let d = lock.withLock { done.removeValue(forKey: ObjectIdentifier(utterance)) }
        d?(ok)
    }
}

/// One voice at a time, by generation (plan V10, V17). Speech runs on
/// `queue`, a private serial queue, never the main thread; `current` is
/// confined to it. Listing voices runs on `voicesQueue`, so a slow
/// `speechVoices()` never delays `say` or `stop`, which Rust calls under its
/// core lock (review item 5); `lastVoices` is confined to `voicesQueue`.
/// Nothing on either queue waits for Rust.
final class Speaker: @unchecked Sendable {
    static let shared = Speaker(engine: AVSynth(), bridge: .shared)
    /// How long a capture start waits for the synthesizer to go quiet.
    static let drainMs = 150
    /// After it reports quiet, a short fixed moment for the audio output: the
    /// flag follows the synthesizer, not the speaker hardware (a probe showed
    /// it turn false about 1 ms after a stop).
    static let tailMs = 40

    private let engine: SynthEngine
    private let bridge: Bridge
    private let queue = DispatchQueue(label: "dev.agentcaller.voice.speaker")
    private let voicesQueue = DispatchQueue(label: "dev.agentcaller.voice.voices")
    private var current: UInt64 = 0
    /// When speech that was playing was last cut, on `queue`. `silenceForCapture`
    /// measures the audio tail from here: Rust stops speech before it calls
    /// Swift, so Swift's own look finds nothing playing.
    private var cutAt: DispatchTime?
    private var lastVoices: [VoiceInfo]?

    init(engine: SynthEngine, bridge: Bridge) {
        self.engine = engine
        self.bridge = bridge
    }

    static func avRate(_ code: UInt32) -> Float {
        switch code {
        case VB_RATE_SLOW: return 0.42
        case VB_RATE_FAST: return 0.58
        default: return AVSpeechUtteranceDefaultSpeechRate
        }
    }

    func say(_ generation: UInt64, text: String, voice: String?, rate: UInt32) {
        queue.sync {
            if current != 0 { engine.stop() }
            current = generation
            engine.speak(text, voice: voice, rate: Speaker.avRate(rate)) { [weak self] finished in
                self?.queue.async { self?.ended(generation, finished) }
            }
        }
    }

    /// Returns after the synthesizer was told to stop.
    func stop() {
        queue.sync {
            if current != 0 || engine.isSpeaking { cutAt = .now() }
            current = 0
            engine.stop()
        }
    }

    /// Before a capture opens (review item 11c, final review): make sure speech
    /// is stopped, then wait until `drainMs` at most for the synthesizer to
    /// report it is no longer speaking, and until `tailMs` have passed since the
    /// speech was cut, so the transcriber does not hear the tail of the app's
    /// own voice. The wait is measured from the cut, whoever made it: on a press
    /// Rust stops speech first, a moment before this runs. Bounded, so a
    /// synthesizer that never reports quiet cannot hold the microphone back, and
    /// nothing to wait for when no speech was cut lately.
    func silenceForCapture() {
        stop()
        // Read on the speech queue, where the synthesizer is used (its flags are nonatomic).
        guard let cut = queue.sync(execute: { cutAt }),
              DispatchTime.now() < cut + .milliseconds(Speaker.drainMs) else { return }
        // Monotonic, so a clock change cannot stretch the wait.
        let quietBy = cut + .milliseconds(Speaker.drainMs)
        while queue.sync(execute: { engine.isSpeaking }) && DispatchTime.now() < quietBy {
            usleep(5_000)
        }
        let tailEnd = cut + .milliseconds(Speaker.tailMs)
        let now = DispatchTime.now()
        if now < tailEnd {
            usleep(UInt32((tailEnd.uptimeNanoseconds - now.uptimeNanoseconds) / 1_000))
        }
    }

    /// `force`: report even if unchanged (Rust asks after an overflow).
    func reportVoices(force: Bool = false) {
        voicesQueue.async {
            let voices = self.engine.voices()
            guard force || voices != self.lastVoices else { return }
            self.lastVoices = voices
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            let json = (try? encoder.encode(voices)).map { String(decoding: $0, as: UTF8.self) } ?? "[]"
            self.bridge.emit(Event(kind: VB_EV_VOICES, text: json))
        }
    }

    /// Tests: waits until work queued so far on both queues ran.
    func flushForTest() {
        queue.sync {}
        voicesQueue.sync {}
    }

    private func ended(_ generation: UInt64, _ finished: Bool) {
        guard current == generation else { return }
        current = 0
        bridge.emit(Event(generation: generation, kind: VB_EV_SPOKEN, value: finished ? 1 : 0))
    }
}

@_cdecl("vb_speech_say")
public func vb_speech_say(_ generation: UInt64, _ text: UnsafePointer<UInt8>?, _ textLen: Int,
                          _ voice: UnsafePointer<UInt8>?, _ voiceLen: Int, _ rate: UInt32) {
    guard generation != 0 else { return }
    guard let t = copyText(text, textLen), let v = copyText(voice, voiceLen) else {
        // Text that is not UTF-8: report it as failed, so Rust does not wait for its cap.
        Bridge.shared.emit(Event(generation: generation, kind: VB_EV_SPOKEN, value: 0))
        return
    }
    Speaker.shared.say(generation, text: t, voice: v.isEmpty ? nil : v, rate: rate)
}

@_cdecl("vb_speech_stop")
public func vb_speech_stop() {
    Speaker.shared.stop()
}

@_cdecl("vb_speech_voices")
public func vb_speech_voices(_ force: UInt32) {
    Speaker.shared.reportVoices(force: force != 0)
}
