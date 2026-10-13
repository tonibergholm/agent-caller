import XCTest
import VoiceBridgeC
@testable import VoiceBridge

final class FakeSynth: SynthEngine, @unchecked Sendable {
    private let lock = NSLock()
    private var log: [String] = []
    private var pending: [@Sendable (Bool) -> Void] = []
    private var list: [VoiceInfo] = [VoiceInfo(id: "v1", name: "Ava", language: "en-US")]

    func add(_ voice: VoiceInfo) { lock.withLock { list.append(voice) } }
    var calls: [String] { lock.withLock { log } }
    func speak(_ text: String, voice: String?, rate: Float, done: @escaping @Sendable (Bool) -> Void) {
        lock.withLock {
            log.append("speak \(text) \(voice ?? "default") \(rate)")
            pending.append(done)
        }
    }
    func stop() { lock.withLock { log.append("stop") } }
    private var speaking = false
    func set(speaking: Bool) { lock.withLock { self.speaking = speaking } }
    var isSpeaking: Bool { lock.withLock { speaking } }
    /// `voices()` sleeps a second first, outside the lock (review item 5).
    var slowVoices = false
    func voices() -> [VoiceInfo] {
        if slowVoices { Thread.sleep(forTimeInterval: 1) }
        return lock.withLock { list }
    }
    /// Finishes the n-th utterance spoken.
    func finish(_ n: Int, ok: Bool = true) { let d = lock.withLock { pending[n] }; d(ok) }
}

final class SpeakerTests: XCTestCase {
    private func rig() -> (Speaker, FakeSynth, Bridge, Collected) {
        let bridge = Bridge()
        let got = Collected()
        bridge.install { got.add($0) }
        let synth = FakeSynth()
        return (Speaker(engine: synth, bridge: bridge), synth, bridge, got)
    }

    private func spoken(_ speaker: Speaker, _ bridge: Bridge, _ got: Collected) -> [Event] {
        speaker.flushForTest()
        bridge.events.sync {}
        return got.events.filter { $0.kind == VB_EV_SPOKEN }
    }

    func testAFinishedUtteranceReportsItsGeneration() {
        let (speaker, synth, bridge, got) = rig()
        speaker.say(4, text: "2 need you.", voice: nil, rate: VB_RATE_NORMAL)
        XCTAssertEqual(synth.calls, ["speak 2 need you. default 0.5"])
        synth.finish(0)
        XCTAssertEqual(spoken(speaker, bridge, got), [Event(generation: 4, kind: VB_EV_SPOKEN, value: 1)])
    }

    /// Spec §4.2: a late end from an older generation is dropped.
    func testAReplacedUtteranceNeverReports() {
        let (speaker, synth, bridge, got) = rig()
        speaker.say(1, text: "a", voice: nil, rate: VB_RATE_NORMAL)
        speaker.say(2, text: "b", voice: "v1", rate: VB_RATE_FAST)
        XCTAssertEqual(synth.calls, ["speak a default 0.5", "stop", "speak b v1 0.58"])
        synth.finish(0, ok: false)
        XCTAssertTrue(spoken(speaker, bridge, got).isEmpty)
        synth.finish(1)
        XCTAssertEqual(spoken(speaker, bridge, got).map(\.generation), [2])
    }

    func testStopSilencesAndNothingFollows() {
        let (speaker, synth, bridge, got) = rig()
        speaker.say(3, text: "a", voice: nil, rate: VB_RATE_SLOW)
        speaker.stop()
        XCTAssertEqual(synth.calls.last, "stop")
        synth.finish(0, ok: false)
        XCTAssertTrue(spoken(speaker, bridge, got).isEmpty)
    }

    /// Plan V16.
    func testVoicesAreReportedOnlyWhenTheyChange() {
        let (speaker, synth, bridge, got) = rig()
        speaker.reportVoices()
        speaker.reportVoices()
        speaker.flushForTest() // both reports ran before the list changes
        synth.add(VoiceInfo(id: "v2", name: "Zoe", language: "en-GB"))
        speaker.reportVoices()
        speaker.flushForTest()
        bridge.events.sync {}
        let voices = got.events.filter { $0.kind == VB_EV_VOICES }
        XCTAssertEqual(voices.count, 2)
        XCTAssertEqual(voices[0].text, "[{\"id\":\"v1\",\"language\":\"en-US\",\"name\":\"Ava\"}]")
        speaker.reportVoices(force: true)
        speaker.flushForTest()
        bridge.events.sync {}
        XCTAssertEqual(got.events.filter { $0.kind == VB_EV_VOICES }.count, 3, "a forced report repeats an unchanged list")
    }

    /// Review item 5: a slow voice listing never delays `stop`.
    func testListingVoicesDoesNotBlockStop() {
        let (speaker, synth, _, _) = rig()
        synth.slowVoices = true // voices() sleeps 1 s
        speaker.reportVoices()
        let started = Date()
        speaker.stop()
        XCTAssertLessThan(Date().timeIntervalSince(started), 0.2)
        speaker.flushForTest()
    }

    /// Review item 11c: a capture start waits briefly for the audio tail,
    /// and never longer than `drainMs`.
    func testSilenceForCaptureWaitsBrieflyForTheAudioToDrain() {
        let (speaker, synth, _, _) = rig()
        synth.set(speaking: false)
        var started = Date()
        speaker.silenceForCapture()
        XCTAssertLessThan(Date().timeIntervalSince(started), 0.05, "quiet: no wait")
        synth.set(speaking: true) // a synthesizer that never reports quiet
        started = Date()
        speaker.silenceForCapture()
        let waited = Date().timeIntervalSince(started)
        XCTAssertGreaterThanOrEqual(waited, 0.14)
        XCTAssertLessThan(waited, 0.5, "bounded")
        XCTAssertEqual(synth.calls.suffix(1), ["stop"])
    }
}
