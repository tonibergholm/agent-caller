import Foundation
import VoiceBridgeC
#if compiler(>=6.2)
import Speech
#endif

/// Capabilities and asset downloads (plan S4, S21), each on its own task,
/// never on the session path. `lock` guards the cached values.
final class Assets: @unchecked Sendable {
    static let shared = Assets()

    /// Whether SpeechTranscriber streams volatile results in the bundle.
    /// From the step-2 spike record (S22): partials arrived only at release,
    /// so the HUD shows "listening" without live text (spec §4.1).
    static let partialsStream = false

    private let lock = NSLock()
    private var cached: Event?
    private var resolved: Locale?
    private var installing = false

    /// The last VB_EV_CAPS, for vb_session_start's synchronous checks.
    var caps: Event? {
        lock.lock()
        defer { lock.unlock() }
        return cached
    }

    /// The locale to transcribe, set only when speech is available.
    var locale: Locale? {
        lock.lock()
        defer { lock.unlock() }
        return resolved
    }

    func query() {
        Task.detached {
            let (event, locale) = await Assets.check()
            self.store(event, locale)
            Bridge.shared.emit(event)
        }
    }

    // Synchronous: NSLock cannot be taken from an async context.
    private func store(_ event: Event, _ locale: Locale?) {
        lock.lock()
        cached = event
        resolved = locale
        lock.unlock()
    }

    private func finishInstall() {
        lock.lock()
        installing = false
        lock.unlock()
    }

    func install() {
        lock.lock()
        let busy = installing
        installing = true
        lock.unlock()
        if busy { return }
        Task.detached {
            let code = await Assets.download()
            self.finishInstall()
            Bridge.shared.emit(Event(kind: VB_EV_ASSETS, code: UInt32(bitPattern: code)))
            self.query()
        }
    }

    private static func capsEvent(_ unavailable: UInt32, _ rate: UInt32 = 0, _ locale: Locale? = nil) -> Event {
        Event(kind: VB_EV_CAPS, code: unavailable, value: unavailable == VB_UA_NONE && partialsStream ? 1 : 0,
              value2: rate, text: locale?.identifier)
    }

    static func check() async -> (Event, Locale?) {
        #if compiler(>=6.2)
        guard #available(macOS 26.0, *) else { return (capsEvent(VB_UA_OS_TOO_OLD), nil) }
        guard SpeechTranscriber.isAvailable else { return (capsEvent(VB_UA_TRANSCRIBER_UNSUPPORTED), nil) }
        guard let locale = await Locales.resolve() else { return (capsEvent(VB_UA_LOCALE_UNSUPPORTED), nil) }
        guard await Locales.isInstalled(locale) else { return (capsEvent(VB_UA_ASSETS_MISSING, 0, locale), nil) }
        let format = await SpeechEngine(locale: locale).format()
        return (capsEvent(VB_UA_NONE, UInt32(format?.sampleRate ?? 0), locale), locale)
        #else
        return (capsEvent(VB_UA_SDK_TOO_OLD), nil)
        #endif
    }

    /// The system download for the English model (spec §9, plan S21).
    static func download() async -> Int32 {
        #if compiler(>=6.2)
        guard #available(macOS 26.0, *), let locale = await Locales.resolve() else { return VB_ERR_SPEECH_UNAVAILABLE }
        let module = SpeechTranscriber(locale: locale, transcriptionOptions: [], reportingOptions: [], attributeOptions: [])
        do {
            if let request = try await AssetInventory.assetInstallationRequest(supporting: [module]) {
                try await request.downloadAndInstall()
            }
            return VB_OK
        } catch {
            return VB_ERR_FAILED
        }
        #else
        return VB_ERR_SPEECH_UNAVAILABLE
        #endif
    }
}

@_cdecl("vb_permission_status")
public func vb_permission_status(_ permission: UInt32) -> UInt32 {
    Permissions.status(permission)
}

@_cdecl("vb_permission_request")
public func vb_permission_request(_ permission: UInt32) {
    Permissions.request(permission)
}

@_cdecl("vb_query_capabilities")
public func vb_query_capabilities() {
    Assets.shared.query()
}

@_cdecl("vb_install_assets")
public func vb_install_assets() {
    Assets.shared.install()
}
