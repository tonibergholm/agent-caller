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

/// The one way out of Swift (plan S3, S5). Every event goes through
/// `events`, a serial queue, so the Rust callback never runs twice at once
/// and never on the audio thread.
final class Bridge: @unchecked Sendable {
    static let shared = Bridge()

    let events = DispatchQueue(label: "dev.agentcaller.voice.events")
    private let lock = NSLock()
    private var sink: ((Event) -> Void)?
    private var current: UInt64 = 0

    func install(_ sink: @escaping (Event) -> Void) {
        lock.lock()
        self.sink = sink
        lock.unlock()
    }

    /// The live session; session events of any other generation are
    /// dropped (spec §4.2). 0 means no session.
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

    /// An event of session `e.generation`: dropped unless it is the live one.
    func emitSession(_ e: Event) {
        events.async {
            guard e.generation != 0, e.generation == self.currentGeneration else { return }
            self.deliver(e)
        }
    }

    /// An event not tied to a session.
    func emit(_ e: Event) {
        events.async { self.deliver(e) }
    }

    private func deliver(_ e: Event) {
        lock.lock()
        let sink = self.sink
        lock.unlock()
        sink?(e)
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
    return VB_OK
}
