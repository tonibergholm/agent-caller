import Foundation
import VoiceBridgeC

/// A Swift copy of a `vb_event`. `deliver` lends it to C for one call.
struct Event: Equatable {
    var generation: UInt64 = 0
    var kind: UInt32
    var code: UInt32 = 0
    var value: UInt32 = 0
    var value2: UInt32 = 0
    var text: String? = nil
    var audioMs: UInt64 = 0
    var voicedMs: UInt64 = 0
}

/// The way out of Swift (plan S5). Bounded and coalescing:
/// - one pending capture state (latest wins) and one pending partial
///   (latest wins);
/// - one latched lock or sleep code;
/// - a FIFO of at most `capacity` other events. When it is full, the new
///   event is dropped and an overflow is latched, delivered first next time.
/// At most one drain block is queued on `events` at any time, and the Rust
/// callback runs only there: never on the audio thread, never twice at
/// once. `lock` guards every field below it.
final class Bridge: @unchecked Sendable {
    static let shared = Bridge()
    static let capacity = 64

    let events = DispatchQueue(label: "dev.agentcaller.voice.events")
    private let lock = NSLock()
    private var sink: ((Event) -> Void)?
    private var current: UInt64 = 0
    private var fifo: [Event] = []
    private var partial: Event?
    private var capture: Event?
    private var system: UInt32?
    private var overflowed = false
    private var scheduled = false

    func install(_ sink: @escaping (Event) -> Void) {
        lock.lock()
        self.sink = sink
        lock.unlock()
    }

    var isInitialized: Bool {
        lock.lock()
        defer { lock.unlock() }
        return sink != nil
    }

    /// The live session; its events are delivered, any other session's are
    /// dropped at delivery (spec §4.2). 0 means none.
    func setCurrent(_ generation: UInt64) {
        lock.lock()
        current = generation
        lock.unlock()
    }

    var currentGeneration: UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return current
    }

    var scheduledForTest: Bool {
        lock.lock()
        defer { lock.unlock() }
        return scheduled
    }

    func emit(_ e: Event) {
        push {
            if fifo.count >= Bridge.capacity {
                overflowed = true
            } else {
                fifo.append(e)
            }
        }
    }

    /// A final or error of session `e.generation`; checked at delivery.
    func emitSession(_ e: Event) {
        emit(e)
    }

    func emitPartial(_ generation: UInt64, _ text: String) {
        push { partial = Event(generation: generation, kind: VB_EV_PARTIAL, text: text) }
    }

    /// Never filtered: the indicator must learn every stop (S18).
    func emitCapture(_ generation: UInt64, on: Bool) {
        onEmitCaptureForTest?(generation, on)
        push { capture = Event(generation: generation, kind: VB_EV_CAPTURE, value: on ? 1 : 0) }
    }

    /// Test hook: runs synchronously on the emitting thread, before queueing.
    /// Set before any session starts.
    var onEmitCaptureForTest: ((UInt64, Bool) -> Void)?

    func emitSystem(_ code: UInt32) {
        push { system = code }
    }

    static func isStale(_ e: Event, current: UInt64) -> Bool {
        switch e.kind {
        case VB_EV_PARTIAL, VB_EV_FINAL, VB_EV_ERROR: return e.generation == 0 || e.generation != current
        default: return false
        }
    }

    private func push(_ change: () -> Void) {
        lock.lock()
        change()
        let schedule = !scheduled
        scheduled = true
        lock.unlock()
        if schedule {
            events.async { self.drain() }
        }
    }

    /// Delivers what is pending: overflow, lock, capture state, the FIFO in
    /// order, the latest partial.
    private func drain() {
        while true {
            lock.lock()
            var batch: [Event] = []
            if overflowed {
                batch.append(Event(kind: VB_EV_OVERFLOW))
                overflowed = false
                // Rust resets on the overflow; a press queued before it would
                // start a recording whose release may be the lost event.
                fifo.removeAll { $0.kind == VB_EV_HOLD && $0.value != 0 }
            }
            if let code = system {
                batch.append(Event(kind: VB_EV_SYSTEM, code: code))
                system = nil
                // The lock goes first, so a key press queued before it would
                // start a recording after it. A release still passes: Rust
                // needs it to forget the held key.
                fifo.removeAll { $0.kind == VB_EV_HOLD && $0.value != 0 }
            }
            if let c = capture {
                batch.append(c)
                capture = nil
            }
            batch += fifo
            fifo.removeAll(keepingCapacity: true)
            if let p = partial {
                batch.append(p)
                partial = nil
            }
            if batch.isEmpty {
                scheduled = false
                lock.unlock()
                return
            }
            let sink = self.sink
            lock.unlock()
            for e in batch {
                // Read per event: a cancel during the batch must still drop
                // the session events that follow it.
                if Bridge.isStale(e, current: currentGeneration) { continue }
                sink?(e)
            }
        }
    }
}

private let started = NSLock()
private var isStarted = false

@_cdecl("vb_init")
public func vb_init(_ ctx: UnsafeMutableRawPointer?, _ onEvent: vb_event_fn?) -> Int32 {
    guard let onEvent else { return VB_ERR_FAILED }
    started.lock()
    defer { started.unlock() }
    if isStarted { return VB_OK }
    isStarted = true
    let context = UInt(bitPattern: ctx)
    Bridge.shared.install { e in
        var ev = vb_event()
        ev.generation = e.generation
        ev.kind = e.kind
        ev.code = e.code
        ev.value = e.value
        ev.value2 = e.value2
        ev.audio_ms = e.audioMs
        ev.voiced_ms = e.voicedMs
        let ctx = UnsafeMutableRawPointer(bitPattern: context)
        guard var text = e.text, !text.isEmpty else {
            onEvent(ctx, &ev)
            return
        }
        // Borrowed for this call only (header contract).
        text.withUTF8 { bytes in
            ev.text = bytes.baseAddress
            ev.text_len = bytes.count
            onEvent(ctx, &ev)
        }
    }
    Assets.shared.query()
    Task.detached { ModelRequests.shared.query() }
    Speaker.shared.reportVoices()
    // The observers belong to the main actor (S3).
    Task { @MainActor in SystemEvents.shared.start() }
    // The probe and sessions share one microphone admission (S20).
    Probe.claimMic = { MicAdmission.shared.claimForProbe() }
    Probe.releaseMic = { MicAdmission.shared.releaseProbe() }
    return VB_OK
}
