import VoiceBridgeC
import AVFoundation

enum Audio {
    /// A buffer counts as voiced at or above this RMS level (plan S10).
    static let voicedDBFS: Float = -45

    /// Converts one capture buffer to the analyzer's format; nil on failure.
    static func convert(_ buffer: AVAudioPCMBuffer, with converter: AVAudioConverter, to format: AVAudioFormat) -> AVAudioPCMBuffer? {
        let ratio = format.sampleRate / buffer.format.sampleRate
        let capacity = AVAudioFrameCount((Double(buffer.frameLength) * ratio).rounded(.up)) + 16
        guard let out = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: capacity) else { return nil }
        var fed = false
        var error: NSError?
        let status = converter.convert(to: out, error: &error) { _, inputStatus in
            if fed {
                inputStatus.pointee = .noDataNow
                return nil
            }
            fed = true
            inputStatus.pointee = .haveData
            return buffer
        }
        return status == .error || error != nil ? nil : out
    }

    /// RMS level of the first channel in dBFS; -infinity for silence or a
    /// format without float samples.
    static func levelDBFS(_ buffer: AVAudioPCMBuffer) -> Float {
        guard let data = buffer.floatChannelData, buffer.frameLength > 0 else { return -.infinity }
        let n = Int(buffer.frameLength)
        var sum: Float = 0
        for i in 0..<n {
            let s = data[0][i]
            sum += s * s
        }
        return sum > 0 ? 10 * log10(sum / Float(n)) : -.infinity
    }
}

/// One capture buffer, measured in its own float format and converted to
/// the analyzer's format.
struct Chunk: @unchecked Sendable {
    let buffer: AVAudioPCMBuffer
    let seconds: Double
    let voiced: Bool

    static func make(_ buffer: AVAudioPCMBuffer, converter: AVAudioConverter, to format: AVAudioFormat) -> Chunk? {
        let voiced = Audio.levelDBFS(buffer) >= Audio.voicedDBFS
        guard let out = Audio.convert(buffer, with: converter, to: format) else { return nil }
        return Chunk(buffer: out, seconds: Double(buffer.frameLength) / buffer.format.sampleRate, voiced: voiced)
    }
}

/// Classified only: no framework error text crosses the bridge (spec §6.3).
enum BridgeError: Error {
    case micUnavailable
    case speechUnavailable

    var code: Int32 {
        switch self {
        case .micUnavailable: return VB_ERR_MIC_UNAVAILABLE
        case .speechUnavailable: return VB_ERR_SPEECH_UNAVAILABLE
        }
    }
}
