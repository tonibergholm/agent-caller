import AVFoundation
import CoreGraphics
import Foundation
import Speech
import VoiceBridgeC
#if canImport(FoundationModels)
import FoundationModels
#endif

/// The step-2 spike (plan Task 1A). A report carries statuses, counts,
/// timings and a `matched` flag; never a transcript (spec §3.7). One probe
/// runs at a time (S20).
enum Probe {
    private static let lock = NSLock()
    private static var running = false
    /// Microphone admission shared with voice sessions (S20). Task 5C wires
    /// these to `MicAdmission`; until then the probe is the only user.
    static var claimMic: () -> Bool = { true }
    static var releaseMic: () -> Void = {}

    static func run(_ step: UInt32) {
        lock.lock()
        let busy = running
        if !busy { running = true }
        lock.unlock()
        if busy {
            emit(["error": "busy", "step": Int(step)])
            return
        }
        Task.detached {
            var report: [String: Any]
            switch step {
            case 1: report = await status()
            case 2: report = await ask(VB_PERM_MICROPHONE)
            case 3:
                if claimMic() {
                    report = await live(seconds: 4)
                    releaseMic()
                } else {
                    report = ["error": "busy"]
                }
            case 4: report = await clip()
            case 5: report = await ask(VB_PERM_SPEECH)
            case 6: report = await inputMonitoring(seconds: 10)
            case 7: report = foundationModels()
            default: report = ["error": "unknown_step"]
            }
            report["step"] = Int(step)
            lock.withLock { running = false }
            emit(report)
        }
    }

    private static func emit(_ report: [String: Any]) {
        let data = try? JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
        let json = data.flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        Bridge.shared.emit(Event(kind: VB_EV_PROBE, text: json))
    }

    /// Spec §11 item 2: Foundation Models availability in the bundle.
    /// Reports the case name only.
    static func foundationModels() -> [String: Any] {
        #if canImport(FoundationModels)
        if #available(macOS 26.0, *) {
            switch SystemLanguageModel.default.availability {
            case .available:
                return ["foundation_models": "available"]
            case .unavailable(let reason):
                return ["foundation_models": "unavailable", "reason": String(describing: reason)]
            @unknown default:
                return ["foundation_models": "unknown"]
            }
        }
        return ["foundation_models": "os_too_old"]
        #else
        return ["foundation_models": "sdk_too_old"]
        #endif
    }

    static func ms(since start: ContinuousClock.Instant) -> Int {
        let d = start.duration(to: .now).components
        return Int(d.seconds) * 1000 + Int(d.attoseconds / 1_000_000_000_000_000)
    }

    static func permissions() -> [String: Any] {
        [
            "microphone": permissionName(Permissions.status(VB_PERM_MICROPHONE)),
            "speech": permissionName(Permissions.status(VB_PERM_SPEECH)),
            "input_monitoring": permissionName(Permissions.status(VB_PERM_INPUT_MONITORING)),
        ]
    }

    static func status() async -> [String: Any] {
        var r = permissions()
        r["bundle"] = Bundle.main.bundleIdentifier ?? "none"
        r["os"] = ProcessInfo.processInfo.operatingSystemVersionString
        r["mic_usage_string"] = Bundle.main.object(forInfoDictionaryKey: "NSMicrophoneUsageDescription") != nil
        r["speech_usage_string"] = Bundle.main.object(forInfoDictionaryKey: "NSSpeechRecognitionUsageDescription") != nil
        #if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            r["transcriber_available"] = SpeechTranscriber.isAvailable
            let english = { (l: Locale) in l.language.languageCode?.identifier == "en" }
            r["supported_count"] = await SpeechTranscriber.supportedLocales.count
            r["supported_en"] = await SpeechTranscriber.supportedLocales.filter(english).map { $0.identifier }.sorted()
            r["installed_en"] = await SpeechTranscriber.installedLocales.filter(english).map { $0.identifier }.sorted()
            r["resolved_locale"] = await Locales.resolve()?.identifier ?? "none"
        } else {
            r["reason"] = "os_too_old"
        }
        #else
        r["reason"] = "sdk_too_old"
        #endif
        return r
    }

    static func ask(_ permission: UInt32) async -> [String: Any] {
        let before = permissionName(Permissions.status(permission))
        await withCheckedContinuation { (done: CheckedContinuation<Void, Never>) in
            switch permission {
            case VB_PERM_MICROPHONE: AVCaptureDevice.requestAccess(for: .audio) { _ in done.resume() }
            case VB_PERM_SPEECH: SFSpeechRecognizer.requestAuthorization { _ in done.resume() }
            default: done.resume()
            }
        }
        return ["before": before, "after": permissionName(Permissions.status(permission))]
    }

    /// Speak "approve the request" right after starting. Measures partials
    /// and the time from stop to the final text. Speech Recognition is
    /// neither asked nor checked first: this run decides S14.
    static func live(seconds: Double) async -> [String: Any] {
        #if compiler(>=6.2)
        guard #available(macOS 26.0, *) else { return ["error": "os_too_old"] }
        var r = permissions()
        guard let locale = await Locales.resolve() else { return r.merging(["error": "no_locale"]) { $1 } }
        let transcriber = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [.volatileResults], attributeOptions: [])
        let analyzer = SpeechAnalyzer(modules: [transcriber])
        guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber]) else {
            return r.merging(["error": "no_format"]) { $1 }
        }
        let engine = AVAudioEngine()
        let input = engine.inputNode
        let micFormat = input.outputFormat(forBus: 0)
        guard micFormat.sampleRate > 0, let converter = AVAudioConverter(from: micFormat, to: format) else {
            return r.merging(["error": "no_microphone"]) { $1 }
        }
        let (inputs, feed) = AsyncStream.makeStream(of: AnalyzerInput.self, bufferingPolicy: .bufferingNewest(64))
        // The probe converts on the audio thread, as Task 5A's capture does.
        input.installTap(onBus: 0, bufferSize: 4096, format: micFormat) { buffer, _ in
            if let out = Audio.convert(buffer, with: converter, to: format) { feed.yield(AnalyzerInput(buffer: out)) }
        }
        let started = ContinuousClock.now
        let reader = Task { () -> (Int, Int, String, String) in
            var partials = 0
            var first = -1
            var text = ""
            do {
                for try await result in transcriber.results {
                    if result.isFinal {
                        text += String(result.text.characters)
                    } else {
                        partials += 1
                        if first < 0 { first = ms(since: started) }
                    }
                }
            } catch {
                return (partials, first, text, "results_failed")
            }
            return (partials, first, text, "none")
        }
        do {
            try await analyzer.start(inputSequence: inputs)
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            return r.merging(["error": "start_failed"]) { $1 }
        }
        try? await Task.sleep(for: .seconds(seconds))
        let stopped = ContinuousClock.now
        engine.stop()
        input.removeTap(onBus: 0)
        feed.finish()
        do {
            try await analyzer.finalizeAndFinishThroughEndOfInput()
        } catch {
            return r.merging(["error": "finalize_failed"]) { $1 }
        }
        let (partials, first, text, error) = await reader.value
        r["locale"] = locale.identifier
        r["partials"] = partials
        r["first_partial_ms"] = first
        r["final_latency_ms"] = ms(since: stopped)
        r["final_chars"] = text.count
        r["matched"] = text.lowercased().contains("approve")
        r["error"] = error
        r["speech_after"] = permissionName(Permissions.status(VB_PERM_SPEECH))
        return r
        #else
        return ["error": "sdk_too_old"]
        #endif
    }

    /// A clip made with `say`: a repeatable baseline next to the live run.
    static func clip() async -> [String: Any] {
        #if compiler(>=6.2)
        guard #available(macOS 26.0, *) else { return ["error": "os_too_old"] }
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("agent-caller-probe-\(UUID().uuidString).aiff")
        defer { try? FileManager.default.removeItem(at: url) }
        let say = Process()
        say.executableURL = URL(fileURLWithPath: "/usr/bin/say")
        say.arguments = ["-o", url.path, "approve the request"]
        do {
            try say.run()
            say.waitUntilExit()
        } catch {
            return ["error": "say_failed"]
        }
        guard let locale = await Locales.resolve() else { return ["error": "no_locale"] }
        let transcriber = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [], attributeOptions: [])
        let started = ContinuousClock.now
        do {
            let file = try AVAudioFile(forReading: url)
            async let text = transcriber.results.reduce("") { $0 + String($1.text.characters) }
            let analyzer = SpeechAnalyzer(modules: [transcriber])
            if let last = try await analyzer.analyzeSequence(from: file) {
                try await analyzer.finalizeAndFinish(through: last)
            } else {
                await analyzer.cancelAndFinishNow()
            }
            let t = try await text
            return ["elapsed_ms": ms(since: started), "final_chars": t.count, "matched": t.lowercased().contains("approve"),
                    "speech_after": permissionName(Permissions.status(VB_PERM_SPEECH))]
        } catch {
            return ["error": "transcription_failed", "elapsed_ms": ms(since: started),
                    "speech_after": permissionName(Permissions.status(VB_PERM_SPEECH))]
        }
        #else
        return ["error": "sdk_too_old"]
        #endif
    }

    /// Asks for Input Monitoring, then counts Right Option presses for
    /// `seconds` with a listen-only flagsChanged tap. Hold Right Option
    /// three times. Only that key is counted; nothing else is recorded.
    static func inputMonitoring(seconds: Double) async -> [String: Any] {
        let before = permissionName(Permissions.status(VB_PERM_INPUT_MONITORING))
        let requested = CGRequestListenEventAccess()
        let counts = await FlagCounter().run(seconds: seconds)
        return ["before": before, "request_returned": requested, "tap_created": counts.created,
                "right_option_down": counts.down, "right_option_up": counts.up,
                "after": permissionName(Permissions.status(VB_PERM_INPUT_MONITORING))]
    }
}

/// A probe-only tap on its own thread and run loop.
final class FlagCounter: @unchecked Sendable {
    var down = 0
    var up = 0

    func run(seconds: Double) async -> (created: Bool, down: Int, up: Int) {
        await withCheckedContinuation { done in
            let thread = Thread {
                let mask = CGEventMask(1 << CGEventType.flagsChanged.rawValue)
                let me = Unmanaged.passUnretained(self).toOpaque()
                let callback: CGEventTapCallBack = { _, type, event, info in
                    if type == .flagsChanged, event.getIntegerValueField(.keyboardEventKeycode) == 61, let info {
                        let counter = Unmanaged<FlagCounter>.fromOpaque(info).takeUnretainedValue()
                        if event.flags.rawValue & 0x40 != 0 { counter.down += 1 } else { counter.up += 1 }
                    }
                    return Unmanaged.passUnretained(event)
                }
                guard let tap = CGEvent.tapCreate(tap: .cgSessionEventTap, place: .headInsertEventTap, options: .listenOnly,
                                                  eventsOfInterest: mask, callback: callback, userInfo: me) else {
                    done.resume(returning: (false, 0, 0))
                    return
                }
                let source = CFMachPortCreateRunLoopSource(nil, tap, 0)
                CFRunLoopAddSource(CFRunLoopGetCurrent(), source, .commonModes)
                CGEvent.tapEnable(tap: tap, enable: true)
                _ = CFRunLoopRunInMode(.defaultMode, seconds, false)
                CGEvent.tapEnable(tap: tap, enable: false)
                CFMachPortInvalidate(tap)
                done.resume(returning: (true, self.down, self.up))
            }
            thread.start()
        }
    }
}

@_cdecl("vb_probe_run")
public func vb_probe_run(_ step: UInt32) {
    Probe.run(step)
}
