import Foundation
#if compiler(>=6.2)
import Speech

@available(macOS 26.0, *)
enum Locales {
    /// English only in step 2 (plan S13): the system locale if it is an
    /// English locale SpeechTranscriber supports, else en-US.
    static func resolve() async -> Locale? {
        let current = Locale.current
        if current.language.languageCode?.identifier == "en",
           let locale = await SpeechTranscriber.supportedLocale(equivalentTo: current) {
            return locale
        }
        return await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: "en_US"))
    }

    static func isInstalled(_ locale: Locale) async -> Bool {
        let installed = await SpeechTranscriber.installedLocales
        return installed.contains { $0.identifier(.bcp47) == locale.identifier(.bcp47) }
    }
}
#endif
