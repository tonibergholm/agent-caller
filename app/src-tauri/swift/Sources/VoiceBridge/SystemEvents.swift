import AppKit
import VoiceBridgeC

/// Locking the screen or sleep stops capture (spec §6.2). Swift cancels
/// every session itself, so the microphone stops without waiting for Rust,
/// then latches the event for Rust (never dropped, S5). Main actor: the
/// observers are installed and delivered on the main thread.
@MainActor
final class SystemEvents {
    static let shared = SystemEvents()
    private var tokens: [NSObjectProtocol] = []

    func start() {
        guard tokens.isEmpty else { return }
        let report: @Sendable (UInt32) -> Void = { code in
            // Latch for Rust first: while sessions are cancelled, a queued
            // press must already be behind the system event (it is dropped
            // with it), not free to open a new session.
            // Spec §6.2: locking the screen or sleep stops speech, first: it is
            // the quickest thing to silence.
            Speaker.shared.stop()
            Bridge.shared.emitSystem(code)
            Sessions.shared.cancelAll()
            // Spec §6.3: model work is discarded on lock and sleep too.
            ModelRequests.shared.cancelAll()
        }
        // Unlock and wake tell Rust that alerts may speak again (spec §6.2). A
        // normal queued event: the lock code above is latched and never dropped,
        // this one only lifts the hold.
        let resumed: @Sendable () -> Void = {
            Bridge.shared.emit(Event(kind: VB_EV_SYSTEM, code: VB_SYS_UNLOCKED))
        }
        let workspace = NSWorkspace.shared.notificationCenter
        tokens.append(workspace.addObserver(forName: NSWorkspace.willSleepNotification, object: nil, queue: .main) { _ in
            report(VB_SYS_WILL_SLEEP)
        })
        tokens.append(workspace.addObserver(forName: NSWorkspace.screensDidSleepNotification, object: nil, queue: .main) { _ in
            report(VB_SYS_SCREENS_SLEEP)
        })
        tokens.append(workspace.addObserver(forName: NSWorkspace.sessionDidResignActiveNotification, object: nil, queue: .main) { _ in
            report(VB_SYS_SCREEN_LOCKED)
        })
        tokens.append(DistributedNotificationCenter.default().addObserver(
            forName: Notification.Name("com.apple.screenIsLocked"), object: nil, queue: .main
        ) { _ in
            report(VB_SYS_SCREEN_LOCKED)
        })
        tokens.append(workspace.addObserver(forName: NSWorkspace.sessionDidBecomeActiveNotification, object: nil, queue: .main) { _ in
            resumed()
        })
        tokens.append(workspace.addObserver(forName: NSWorkspace.screensDidWakeNotification, object: nil, queue: .main) { _ in
            resumed()
        })
        tokens.append(workspace.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { _ in
            resumed()
        })
        tokens.append(DistributedNotificationCenter.default().addObserver(
            forName: Notification.Name("com.apple.screenIsUnlocked"), object: nil, queue: .main
        ) { _ in
            resumed()
        })
    }
}
