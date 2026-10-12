import AVFoundation
import CoreGraphics
import IOKit.hid
import Speech
import VoiceBridgeC

/// The three permissions voice may need (spec §6.4).
enum Permissions {
    static func status(_ permission: UInt32) -> UInt32 {
        switch permission {
        case VB_PERM_MICROPHONE:
            switch AVCaptureDevice.authorizationStatus(for: .audio) {
            case .authorized: return VB_PS_GRANTED
            case .denied: return VB_PS_DENIED
            case .restricted: return VB_PS_RESTRICTED
            default: return VB_PS_NOT_DETERMINED
            }
        case VB_PERM_SPEECH:
            switch SFSpeechRecognizer.authorizationStatus() {
            case .authorized: return VB_PS_GRANTED
            case .denied: return VB_PS_DENIED
            case .restricted: return VB_PS_RESTRICTED
            default: return VB_PS_NOT_DETERMINED
            }
        case VB_PERM_INPUT_MONITORING:
            // The only call that tells "never asked" from "denied" (plan S17).
            switch IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) {
            case kIOHIDAccessTypeGranted: return VB_PS_GRANTED
            case kIOHIDAccessTypeDenied: return VB_PS_DENIED
            default: return VB_PS_NOT_DETERMINED
            }
        default:
            return VB_PS_NOT_DETERMINED
        }
    }

    /// Shows the system prompt if the user was never asked. The result
    /// arrives as VB_EV_PERMISSION.
    static func request(_ permission: UInt32) {
        let report = { Bridge.shared.emit(Event(kind: VB_EV_PERMISSION, code: permission, value: status(permission))) }
        switch permission {
        case VB_PERM_MICROPHONE:
            AVCaptureDevice.requestAccess(for: .audio) { _ in report() }
        case VB_PERM_SPEECH:
            // Needs NSSpeechRecognitionUsageDescription, or the process ends.
            SFSpeechRecognizer.requestAuthorization { _ in report() }
        case VB_PERM_INPUT_MONITORING:
            _ = CGRequestListenEventAccess()
            report()
        default:
            break
        }
    }
}

func permissionName(_ state: UInt32) -> String {
    switch state {
    case VB_PS_GRANTED: return "granted"
    case VB_PS_DENIED: return "denied"
    case VB_PS_RESTRICTED: return "restricted"
    default: return "not_determined"
    }
}
