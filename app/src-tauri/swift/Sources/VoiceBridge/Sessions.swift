import AVFoundation
import Foundation
import VoiceBridgeC

/// Who may open the microphone: voice sessions or the probe, never both,
/// whichever comes first (plan S20). `lock` guards both flags.
final class MicAdmission: @unchecked Sendable {
    static let shared = MicAdmission()
    private let lock = NSLock()
    private var probe = false
    private var sessions = false

    func claimForSessions() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if probe { return false }
        sessions = true
        return true
    }

    func releaseSessions() {
        lock.lock()
        sessions = false
        lock.unlock()
    }

    func claimForProbe() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if probe || sessions { return false }
        probe = true
        return true
    }

    func releaseProbe() {
        lock.lock()
        probe = false
        lock.unlock()
    }
}

/// One recording (plan S4). Every capture transition (start, stop, cancel)
/// and its `VB_EV_CAPTURE` publish happen under this session's `lock`, in
/// that order, so a late `on` can never follow an `off` (S18). No queue sits
/// between `stopCapture`/`cancel` and the microphone. The work runs on
/// `task`. `lock` guards every var below it.
final class Session: @unchecked Sendable {
    let generation: UInt64
    private let capture: AudioCapture
    private let work: SpeechWork
    private let bridge: Bridge
    private let lock = NSLock()
    private var started = false // read and written only under `lock`
    private var captureEnded = false
    private var cancelled = false
    private var stoppedAt: ContinuousClock.Instant?
    private var task: Task<Void, Never>?
    private var failsafe: DispatchWorkItem?
    /// Test hook: runs under `lock`, after capture started, before `on`.
    var onCaptureStartedForTest: ((Session) -> Void)?

    init(generation: UInt64, capture: AudioCapture, work: SpeechWork, bridge: Bridge) {
        self.generation = generation
        self.capture = capture
        self.work = work
        self.bridge = bridge
    }

    /// True if `lock` is held (by anyone, including the caller).
    var lockHeldForTest: Bool {
        if lock.try() {
            lock.unlock()
            return false
        }
        return true
    }

    /// `done` runs once all of the session's work ended, cancelled or not.
    func run(maxMs: UInt32, done: @escaping @Sendable () -> Void) {
        let t = Task { [self] in
            await body(maxMs: maxMs)
            done()
        }
        lock.lock()
        task = t
        let late = cancelled
        lock.unlock()
        if late { t.cancel() }
    }

    private enum Opening {
        case opened(AsyncStream<Chunk>)
        case cancelled
        case releasedEarly
        case micFailed
    }

    /// The capture transition: start, failsafe and `on`, all under `lock`.
    /// Synchronous because an NSLock cannot be held in an async context.
    private func open(_ format: AVAudioFormat, maxMs: UInt32) -> Opening {
        lock.lock()
        defer { lock.unlock() }
        if cancelled { return .cancelled }
        // Released before the microphone opened: nothing was heard.
        if captureEnded { return .releasedEarly }
        let chunks: AsyncStream<Chunk>
        do {
            chunks = try capture.start(to: format) { [weak self] in self?.lost() }
        } catch {
            return .micFailed
        }
        started = true
        // 60 s from capture start, before the analyzer starts (S9).
        let item = DispatchWorkItem { [weak self] in self?.cancel() }
        failsafe = item
        DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(Int(maxMs)), execute: item)
        onCaptureStartedForTest?(self)
        bridge.emitCapture(generation, on: true) // inside the transition (S18)
        return .opened(chunks)
    }

    /// Whether the session was cancelled, and when capture stopped.
    private func outcome() -> (cancelled: Bool, stoppedAt: ContinuousClock.Instant?) {
        lock.lock()
        defer { lock.unlock() }
        return (cancelled, stoppedAt)
    }

    private func body(maxMs: UInt32) async {
        guard let format = await work.format() else { return fail(VB_ERR_SPEECH_UNAVAILABLE) }
        let chunks: AsyncStream<Chunk>
        switch open(format, maxMs: maxMs) {
        case .cancelled: return
        case .releasedEarly: return bridge.emitSession(Event(generation: generation, kind: VB_EV_FINAL))
        case .micFailed: return fail(VB_ERR_MIC_UNAVAILABLE)
        case .opened(let c): chunks = c
        }
        do {
            let (g, bridge) = (generation, self.bridge)
            try await work.start(chunks) { text in bridge.emitPartial(g, text) }
            let heard = try await work.finish()
            let (gone, stopped) = outcome()
            if gone { return }
            let latency = stopped.map { UInt64(Probe.ms(since: $0)) } ?? 0
            bridge.emitSession(Event(generation: generation, kind: VB_EV_FINAL, value: UInt32(clamping: latency),
                                     text: heard.text, audioMs: heard.audioMs, voicedMs: heard.voicedMs))
        } catch {
            stopCapture()
            // Never the error's text (spec §6.3).
            if !outcome().cancelled { fail(VB_ERR_SPEECH_UNAVAILABLE) }
        }
    }

    /// Ends capture under `lock` and publishes `off`, in that order.
    /// Caller holds `lock`.
    private func endCaptureLocked() {
        guard !captureEnded else { return }
        captureEnded = true
        failsafe?.cancel()
        capture.stop()
        if started { bridge.emitCapture(generation, on: false) }
    }

    /// Release or toggle: the microphone stops now; the transcript finishes
    /// and this session stays cancellable (S4).
    func stopCapture() {
        lock.lock()
        if stoppedAt == nil { stoppedAt = .now }
        endCaptureLocked()
        lock.unlock()
    }

    /// Cancel, final timeout, lock, replacement, failsafe: capture stops now
    /// and all work is cancelled, finishing or not (S4). The work's own
    /// cancel latch (S27) makes this terminal even if it lands before the
    /// work started.
    func cancel() {
        lock.lock()
        if cancelled {
            lock.unlock()
            return
        }
        cancelled = true
        endCaptureLocked()
        let t = task
        lock.unlock()
        t?.cancel()
        let work = self.work
        Task.detached { await work.cancel() }
    }

    /// The device went away: tell Rust while this is still the live
    /// session, then stop everything.
    private func lost() {
        fail(VB_ERR_MIC_UNAVAILABLE)
        cancel()
    }

    private func fail(_ code: Int32) {
        bridge.emitSession(Event(generation: generation, kind: VB_EV_ERROR, code: UInt32(code)))
    }
}

/// Every session that holds the microphone or still has work (plan S4).
/// `transition` serializes start, cancel, cancelAll and removal through
/// completed capture shutdown: a start can never open a microphone while a
/// lock or a replacement is still stopping one. `lock` guards `all` and is
/// never held while calling into a session. Lock order: `transition`, then
/// a session's lock, then `lock` or the bridge's lock.
final class Sessions: @unchecked Sendable {
    static let shared = Sessions(bridge: .shared, admission: .shared, make: Sessions.live)

    private let transition = NSLock()
    private let lock = NSLock()
    private var all: [UInt64: Session] = [:]
    private let bridge: Bridge
    private let admission: MicAdmission
    private let make: (UInt64) -> Session?
    /// Test hook: called when a session's work ended.
    var onDone: ((UInt64) -> Void)?

    init(bridge: Bridge, admission: MicAdmission, make: @escaping (UInt64) -> Session?) {
        self.bridge = bridge
        self.admission = admission
        self.make = make
    }

    var count: Int {
        lock.lock()
        defer { lock.unlock() }
        return all.count
    }

    var transitionHeldForTest: Bool {
        if transition.try() {
            transition.unlock()
            return false
        }
        return true
    }

    private let waitingLock = NSLock()
    private var startsWaiting = 0

    /// Starts blocked on `transition` right now, for barrier tests.
    var startsWaitingForTest: Int {
        waitingLock.lock()
        defer { waitingLock.unlock() }
        return startsWaiting
    }

    /// Cancels every other session first; each old capture has stopped
    /// before this returns. Refused with VB_ERR_BUSY while the probe holds
    /// the microphone (S20).
    func start(_ generation: UInt64, maxMs: UInt32) -> Int32 {
        guard generation != 0 else { return VB_ERR_FAILED }
        waitingLock.lock()
        startsWaiting += 1
        waitingLock.unlock()
        transition.lock()
        waitingLock.lock()
        startsWaiting -= 1
        waitingLock.unlock()
        defer { transition.unlock() }
        guard admission.claimForSessions() else { return VB_ERR_BUSY }
        guard let session = make(generation) else {
            releaseIfEmpty()
            return VB_ERR_SPEECH_UNAVAILABLE
        }
        lock.lock()
        let old = Array(all.values)
        all = [generation: session]
        lock.unlock()
        old.forEach { $0.cancel() }
        bridge.setCurrent(generation)
        session.run(maxMs: maxMs) { [weak self, weak session] in
            guard let self, let session else { return }
            self.remove(session)
            self.onDone?(session.generation)
        }
        return VB_OK
    }

    /// Release or toggle. Ending one capture needs no transition.
    func stop(_ generation: UInt64) {
        lock.lock()
        let s = all[generation]
        lock.unlock()
        s?.stopCapture()
    }

    func cancel(_ generation: UInt64) {
        transition.lock()
        defer { transition.unlock() }
        lock.lock()
        let s = all.removeValue(forKey: generation)
        lock.unlock()
        guard let s else { return }
        if bridge.currentGeneration == generation { bridge.setCurrent(0) }
        s.cancel()
        releaseIfEmpty()
    }

    /// Lock or sleep (spec §6.2). Returns after every capture stopped.
    func cancelAll() {
        transition.lock()
        defer { transition.unlock() }
        lock.lock()
        let ss = Array(all.values)
        all = [:]
        lock.unlock()
        bridge.setCurrent(0)
        ss.forEach { $0.cancel() }
        admission.releaseSessions()
    }

    private func remove(_ session: Session) {
        transition.lock()
        defer { transition.unlock() }
        lock.lock()
        if all[session.generation] === session { all.removeValue(forKey: session.generation) }
        lock.unlock()
        releaseIfEmpty()
    }

    /// Caller holds `transition`.
    private func releaseIfEmpty() {
        lock.lock()
        let empty = all.isEmpty
        lock.unlock()
        if empty { admission.releaseSessions() }
    }

    static func live(_ generation: UInt64) -> Session? {
        #if compiler(>=6.2)
        if #available(macOS 26.0, *), let locale = Assets.shared.locale {
            return Session(generation: generation, capture: Capture(),
                           work: Transcription(engine: SpeechEngine(locale: locale)), bridge: .shared)
        }
        #endif
        return nil
    }
}

@_cdecl("vb_session_start")
public func vb_session_start(_ generation: UInt64, _ maxMs: UInt32, _ speechRequired: UInt32) -> Int32 {
    guard Bridge.shared.isInitialized else { return VB_ERR_NOT_INITIALIZED }
    guard generation != 0 else { return VB_ERR_FAILED }
    guard Permissions.status(VB_PERM_MICROPHONE) == VB_PS_GRANTED else { return VB_ERR_MIC_DENIED }
    if speechRequired != 0, Permissions.status(VB_PERM_SPEECH) != VB_PS_GRANTED { return VB_ERR_SPEECH_DENIED }
    switch Assets.shared.caps?.code {
    case .some(VB_UA_NONE): break
    case .some(VB_UA_ASSETS_MISSING): return VB_ERR_ASSETS_MISSING
    default: return VB_ERR_SPEECH_UNAVAILABLE // includes "not checked yet"
    }
    return Sessions.shared.start(generation, maxMs: maxMs)
}

@_cdecl("vb_session_stop")
public func vb_session_stop(_ generation: UInt64) {
    Sessions.shared.stop(generation)
}

@_cdecl("vb_session_cancel")
public func vb_session_cancel(_ generation: UInt64) {
    Sessions.shared.cancel(generation)
}
