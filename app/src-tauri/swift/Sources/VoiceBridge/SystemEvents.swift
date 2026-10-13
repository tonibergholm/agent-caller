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

    /// Whether alerts may speak again. Only a screen verified as not locked
    /// counts; locked and unknown both stay closed (review finding: a wake
    /// notification arrives while the lock screen is still up).
    nonisolated static func mayResume(screenLocked: Bool?) -> Bool {
        screenLocked == false
    }

    /// The session dictionary's lock flag. `nil` dictionary: unknown. A readable
    /// dictionary without the key is a screen that is not locked.
    nonisolated static func screenLocked(from session: [String: Any]?) -> Bool? {
        guard let session else { return nil }
        return (session["CGSSessionScreenIsLocked"] as? Bool) ?? false
    }

    nonisolated static func currentScreenLocked() -> Bool? {
        screenLocked(from: CGSessionCopyCurrentDictionary() as? [String: Any])
    }

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
        // A wake or a return to the session is not an unlock: the lock screen may
        // still be up. Check once the system settled, and only a screen that is
        // verified unlocked resumes; the explicit unlock notification below
        // resumes a locked one.
        let woke: @Sendable () -> Void = {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
                if SystemEvents.mayResume(screenLocked: SystemEvents.currentScreenLocked()) { resumed() }
            }
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
            woke()
        })
        tokens.append(workspace.addObserver(forName: NSWorkspace.screensDidWakeNotification, object: nil, queue: .main) { _ in
            woke()
        })
        tokens.append(workspace.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { _ in
            woke()
        })
        tokens.append(DistributedNotificationCenter.default().addObserver(
            forName: Notification.Name("com.apple.screenIsUnlocked"), object: nil, queue: .main
        ) { _ in
            resumed()
        })
    }
}
