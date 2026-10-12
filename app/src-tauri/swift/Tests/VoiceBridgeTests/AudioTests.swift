import AVFoundation
import XCTest
@testable import VoiceBridge

final class AudioTests: XCTestCase {
    private func tone(amplitude: Float, frames: AVAudioFrameCount = 4800) -> AVAudioPCMBuffer {
        let format = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 1)!
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: frames)!
        buffer.frameLength = frames
        for i in 0..<Int(frames) {
            buffer.floatChannelData![0][i] = amplitude * sin(Float(i) * 2 * .pi * 440 / 48_000)
        }
        return buffer
    }

    /// Plan S10: voice is measured on the capture format, before conversion.
    func testAChunkIsMeasuredBeforeItIsConverted() throws {
        let target = try XCTUnwrap(AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16_000, channels: 1, interleaved: true))
        let loud = tone(amplitude: 0.5)
        let converter = try XCTUnwrap(AVAudioConverter(from: loud.format, to: target))
        let chunk = try XCTUnwrap(Chunk.make(loud, converter: converter, to: target))
        XCTAssertTrue(chunk.voiced)
        XCTAssertEqual(chunk.seconds, 0.1, accuracy: 0.0001)
        XCTAssertEqual(chunk.buffer.format.sampleRate, 16_000)
        let quiet = try XCTUnwrap(Chunk.make(tone(amplitude: 0), converter: converter, to: target))
        XCTAssertFalse(quiet.voiced)
    }
}
