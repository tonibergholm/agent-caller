import CoreGraphics
import Foundation
import VoiceBridgeC

/// A created event tap, behind a protocol so the lifecycle is tested
/// without Input Monitoring.
protocol TapPort: AnyObject {
    var source: CFRunLoopSource? { get }
    func enable(_ on: Bool)
    var isEnabled: Bool { get }
    func invalidate()
}

final class MachTapPort: TapPort {
    private let port: CFMachPort
    init(_ port: CFMachPort) { self.port = port }
    var source: CFRunLoopSource? { CFMachPortCreateRunLoopSource(nil, port, 0) }
    func enable(_ on: Bool) { CGEvent.tapEnable(tap: port, enable: on) }
    var isEnabled: Bool { CGEvent.tapIsEnabled(tap: port) }
    func invalidate() { CFMachPortInvalidate(port) }
}

/// One tap's state (plan S26). The key and mask never change, so a
/// reconfiguration never changes what a running callback reads. `lock`
/// guards `port`, `loop`, `stopRequested` and `created`. `down` belongs to
/// the tap thread.
final class TapContext: @unchecked Sendable {
    let keycode: Int64
    let mask: UInt64
    let token: UInt64
    let bridge: Bridge
    let ready = DispatchSemaphore(value: 0)
    let exited = DispatchSemaphore(value: 0)
    let lock = NSLock()
    var port: TapPort?
    var loop: CFRunLoop?
    var stopRequested = false
    var created = false
    private var down = false

    init(keycode: UInt16, mask: UInt64, token: UInt64, bridge: Bridge) {
        self.keycode = Int64(keycode)
        self.mask = mask
        self.token = token
        self.bridge = bridge
    }

    var shouldStop: Bool {
        lock.lock()
        defer { lock.unlock() }
        return stopRequested
    }

    /// Forwards only the hold key's down and up, and "another key" while it
    /// is down, with no key code (S11). Never changes an event.
    func handle(_ type: CGEventType, _ event: CGEvent) {
        if shouldStop { return }
        switch type {
        case .tapDisabledByTimeout, .tapDisabledByUserInput:
            lock.lock()
            let p = port
            lock.unlock()
            p?.enable(true)
            let back = p?.isEnabled ?? false
            // A key-up may have been lost; Rust cancels a held recording.
            down = false
            bridge.emit(Event(generation: token, kind: VB_EV_TAP_DISABLED, code: type == .tapDisabledByTimeout ? 1 : 2, value: back ? 1 : 0))
        case .flagsChanged:
            if event.getIntegerValueField(.keyboardEventKeycode) == keycode {
                let now = event.flags.rawValue & mask != 0
                if now != down {
                    down = now
                    bridge.emit(Event(generation: token, kind: VB_EV_HOLD, value: now ? 1 : 0))
                }
            } else if down {
                bridge.emit(Event(generation: token, kind: VB_EV_OTHER_KEY))
            }
        case .keyDown:
            if down { bridge.emit(Event(generation: token, kind: VB_EV_OTHER_KEY)) }
        default:
            break
        }
    }
}

/// The listen-only hold-key tap (spec §4.1) with a serialized lifecycle
/// (plan S26). Start and stop run one at a time on `lifecycle`. `lock`
/// guards `token` and `current`. Lock order: a context's lock, then this
/// one; never the other way round.
final class HoldTap: @unchecked Sendable {
    static let shared = HoldTap()

    private let bridge: Bridge
    private let granted: () -> Bool
    private let readyTimeout: DispatchTimeInterval
    private let create: (TapContext) -> TapPort?
    private let lifecycle = DispatchQueue(label: "dev.agentcaller.voice.tap.lifecycle")
    private let lock = NSLock()
    private var token: UInt64 = 0
    private var current: TapContext?

    init(
        bridge: Bridge = .shared,
        granted: @escaping () -> Bool = { Permissions.status(VB_PERM_INPUT_MONITORING) == VB_PS_GRANTED },
        readyTimeout: DispatchTimeInterval = .seconds(1),
        create: @escaping (TapContext) -> TapPort? = HoldTap.realTap
    ) {
        self.bridge = bridge
        self.granted = granted
        self.readyTimeout = readyTimeout
        self.create = create
    }

    var running: Bool {
        lock.lock()
        defer { lock.unlock() }
        return current != nil
    }

    /// The code, and on success the tap's generation (S26).
    func start(keycode: UInt16, mask: UInt64) -> (code: Int32, tap: UInt64) {
        lifecycle.sync { startLocked(keycode, mask) }
    }

    func stop() {
        lifecycle.sync { stopLocked() }
    }

    private func startLocked(_ keycode: UInt16, _ mask: UInt64) -> (code: Int32, tap: UInt64) {
        stopLocked()
        guard granted() else { return (VB_ERR_INPUT_MONITORING_DENIED, 0) }
        lock.lock()
        token += 1
        let ctx = TapContext(keycode: keycode, mask: mask, token: token, bridge: bridge)
        lock.unlock()
        let thread = Thread { [self] in self.run(ctx) }
        thread.name = "dev.agentcaller.voice.tap"
        thread.start()
        let timedOut = ctx.ready.wait(timeout: .now() + readyTimeout) == .timedOut
        ctx.lock.lock()
        let ok = ctx.created && !timedOut
        if !ok { ctx.stopRequested = true }
        ctx.lock.unlock()
        guard ok else {
            // A tap created after this sees a stale token and never runs.
            lock.lock()
            token += 1
            lock.unlock()
            return (VB_ERR_TAP_FAILED, 0)
        }
        lock.lock()
        current = ctx
        lock.unlock()
        return (VB_OK, ctx.token)
    }

    private func stopLocked() {
        lock.lock()
        let ctx = current
        current = nil
        token += 1
        lock.unlock()
        guard let ctx else { return }
        ctx.lock.lock()
        ctx.stopRequested = true
        let loop = ctx.loop
        ctx.lock.unlock()
        if let loop { CFRunLoopStop(loop) }
        // The thread acknowledges its exit before anything is replaced.
        _ = ctx.exited.wait(timeout: .now() + 2)
    }

    private func isCurrent(_ t: UInt64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return token == t
    }

    /// The tap thread.
    private func run(_ ctx: TapContext) {
        defer { ctx.exited.signal() }
        guard let port = create(ctx) else {
            ctx.ready.signal()
            return
        }
        let loop = CFRunLoopGetCurrent()
        ctx.lock.lock()
        let stale = ctx.stopRequested || !isCurrent(ctx.token)
        if !stale {
            ctx.port = port
            ctx.loop = loop
            ctx.created = true
        }
        ctx.lock.unlock()
        if stale {
            port.invalidate()
            ctx.ready.signal()
            return
        }
        if let source = port.source { CFRunLoopAddSource(loop, source, .commonModes) }
        // Keeps the run loop alive between events, so the loop below waits
        // instead of spinning.
        let keepAlive = CFRunLoopTimerCreateWithHandler(nil, CFAbsoluteTimeGetCurrent() + 3600, 3600, 0, 0) { _ in }
        CFRunLoopAddTimer(loop, keepAlive, .commonModes)
        // A stop that arrived since the check above must win: enable under
        // the lock that `stopRequested` is set under.
        ctx.lock.lock()
        let late = ctx.stopRequested
        if late { ctx.created = false } else { port.enable(true) }
        ctx.lock.unlock()
        if late {
            port.invalidate()
            CFRunLoopRemoveTimer(loop, keepAlive, .commonModes)
            ctx.ready.signal()
            return
        }
        ctx.ready.signal()
        while !ctx.shouldStop {
            _ = CFRunLoopRunInMode(.defaultMode, 0.5, false)
        }
        port.enable(false)
        port.invalidate()
        CFRunLoopRemoveTimer(loop, keepAlive, .commonModes)
    }

    static func realTap(_ ctx: TapContext) -> TapPort? {
        let events = CGEventMask(1 << CGEventType.flagsChanged.rawValue) | CGEventMask(1 << CGEventType.keyDown.rawValue)
        guard let port = CGEvent.tapCreate(tap: .cgSessionEventTap, place: .headInsertEventTap, options: .listenOnly,
                                           eventsOfInterest: events, callback: holdTapCallback,
                                           userInfo: Unmanaged.passUnretained(ctx).toOpaque()) else {
            return nil
        }
        return MachTapPort(port)
    }
}

/// The thread's closure holds `ctx` for as long as the tap can call this.
private func holdTapCallback(proxy: CGEventTapProxy, type: CGEventType, event: CGEvent, info: UnsafeMutableRawPointer?) -> Unmanaged<CGEvent>? {
    if let info {
        Unmanaged<TapContext>.fromOpaque(info).takeUnretainedValue().handle(type, event)
    }
    return Unmanaged.passUnretained(event)
}

@_cdecl("vb_tap_start")
public func vb_tap_start(_ keycode: UInt16, _ mask: UInt64, _ outTap: UnsafeMutablePointer<UInt64>?) -> Int32 {
    let (code, tap) = HoldTap.shared.start(keycode: keycode, mask: mask)
    outTap?.pointee = tap
    return code
}

@_cdecl("vb_tap_stop")
public func vb_tap_stop() {
    HoldTap.shared.stop()
}
