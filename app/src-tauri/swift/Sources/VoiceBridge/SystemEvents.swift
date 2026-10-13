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
            Bridge.shared.emitSystem(code)
            Sessions.shared.cancelAll()
            // Spec §6.3: model work is discarded on lock and sleep too.
            ModelRequests.shared.cancelAll()
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
    }
}
