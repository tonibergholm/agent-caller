//! Spoken output (voice spec §7, plan V10 to V13).
use std::sync::Arc;
use std::time::{Duration, Instant};

use acd_voice::hud::Hud;
use acd_voice::plan::Reaction;
use acd_voice::speak::{self, Due, Speaker, SpeechSettings};
use acd_voice::text::VoiceText;

use super::{Controller, Core};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Speaking {
    pub generation: u64,
    pub seq: Option<u64>,
}

impl Controller {
    /// Attaches the platform's speech output (spec §4.1).
    pub fn attach_speaker(&self, speaker: Arc<dyn Speaker>) {
        *self.speaker.lock().unwrap() = Some(speaker);
        // Alerts could not take their baseline without a speaker: do it now,
        // or the first request after startup would become the baseline.
        self.alert_tick_soon();
    }

    /// Spec §8: alerts, voice and rate.
    pub fn set_speech(&self, settings: SpeechSettings) {
        let mut core = self.core.lock().unwrap();
        core.speech = settings;
        // Turning alerts off stops one that is playing (it has no HUD state
        // to hide, so only this does it).
        if !core.speech.alerts && core.speaking.is_some_and(|s| s.seq.is_none()) {
            self.stop_speech(&mut core);
        }
        drop(core);
        self.alert_tick_soon();
    }

    /// Test hook: a shorter cap than `speak::speech_cap`.
    #[doc(hidden)]
    pub fn set_speech_cap_for_test(&self, cap: Duration) {
        self.core.lock().unwrap().speech_cap = Some(cap);
    }

    /// The platform finished speaking `generation`. Any other generation
    /// is stale and ignored (spec §4.2).
    pub fn spoken(&self, generation: u64) {
        let mut core = self.core.lock().unwrap();
        let Some(s) = core.speaking.filter(|s| s.generation == generation) else {
            return;
        };
        core.speaking = None;
        if let Some(seq) = s.seq {
            self.sink.speech_ended(seq);
        }
        drop(core);
        // A waiting alert may go now that the speaker is free.
        self.alert_tick_soon();
    }

    /// The HUD's Stop button or Esc on it (`Some(seq)`: the state it
    /// showed), or Esc in the inbox panel (`None`). True if speech stopped.
    pub fn stop_speaking(&self, seq: Option<u64>) -> bool {
        let mut core = self.core.lock().unwrap();
        let Some(s) = core.speaking else {
            return false;
        };
        if seq.is_some() && s.seq != seq {
            return false;
        }
        self.stop_speech(&mut core);
        if s.seq.is_some() {
            self.emit(&mut core, Reaction { hud: Hud::Hidden, select: None });
        }
        true
    }

    pub(super) fn speaker(&self) -> Option<Arc<dyn Speaker>> {
        self.speaker.lock().unwrap().clone()
    }

    /// Says a reply; false without a speaker (the HUD then uses D18's timer).
    pub(super) fn say_reply(&self, core: &mut Core, text: &VoiceText) -> bool {
        let Some(speaker) = self.speaker() else {
            return false;
        };
        let generation = self.start_speech(core, &speaker, text);
        core.speaking = Some(Speaking { generation, seq: None });
        tracing::info!(target: "acd_voice", chars = text.char_len(), "speaking");
        true
    }

    pub(super) fn start_speech(&self, core: &mut Core, speaker: &Arc<dyn Speaker>, text: &VoiceText) -> u64 {
        core.next_speech += 1;
        let generation = core.next_speech;
        speaker.say(generation, text, core.speech.voice.as_deref(), core.speech.rate);
        let cap = core.speech_cap.unwrap_or_else(|| speak::speech_cap(text));
        let me = self.me.clone();
        self.runtime.spawn(async move {
            tokio::time::sleep(cap).await;
            if let Some(this) = me.upgrade() {
                this.cap_reached(generation);
            }
        });
        generation
    }

    /// No end arrived in time (Review Focus 5): stop, and hide a reply.
    fn cap_reached(&self, generation: u64) {
        let mut core = self.core.lock().unwrap();
        let Some(s) = core.speaking.filter(|s| s.generation == generation) else {
            return;
        };
        tracing::info!(target: "acd_voice", "speech cut at its cap");
        self.stop_speech(&mut core);
        if s.seq.is_some() {
            self.emit(&mut core, Reaction { hud: Hud::Hidden, select: None });
        }
    }

    /// Stops what is speaking, if anything.
    pub(super) fn stop_speech(&self, core: &mut Core) {
        if core.speaking.take().is_some()
            && let Some(speaker) = self.speaker()
        {
            speaker.stop();
        }
    }

    /// Stops the speaker whatever Rust believes is speaking: the press path,
    /// where the microphone opens next.
    pub(super) fn silence(&self, core: &mut Core) {
        core.speaking = None;
        if let Some(speaker) = self.speaker() {
            speaker.stop();
        }
    }
}

impl Controller {
    /// Runs `alert_tick` on the runtime, off any lock the caller holds.
    pub(super) fn alert_tick_soon(&self) {
        let me = self.me.clone();
        self.runtime.spawn(async move {
            if let Some(this) = me.upgrade() {
                this.alert_tick();
            }
        });
    }

    /// Looks for requests to announce (plan V14). The snapshot is frozen
    /// before the core lock is taken, as `hold` does.
    pub(super) fn alert_tick(&self) {
        let snapshot = self.freeze(false);
        let mut core = self.core.lock().unwrap();
        let speaker = self.speaker();
        let Some(speaker) = speaker.filter(|_| core.enabled && core.speech.alerts) else {
            core.alerts.reset();
            return;
        };
        // Locked or asleep: nothing is observed either, so what arrives meanwhile
        // is news when the screen is back (spec §6.2).
        if core.locked {
            return;
        }
        core.alerts.observe(&snapshot);
        let busy = core.recorder.active().is_some()
            || core.unadmitted.is_some()
            || !core.exec.is_idle()
            || core.speaking.is_some();
        match core.alerts.due(&snapshot, Instant::now(), busy) {
            Due::Speak(text) => {
                let generation = self.start_speech(&mut core, &speaker, &text);
                core.speaking = Some(Speaking { generation, seq: None });
                tracing::info!(target: "acd_voice", chars = text.char_len(), "alert spoken");
            }
            Due::Wait(after) if !core.alert_timer => {
                core.alert_timer = true;
                let me = self.me.clone();
                self.runtime.spawn(async move {
                    tokio::time::sleep(after).await;
                    if let Some(this) = me.upgrade() {
                        this.core.lock().unwrap().alert_timer = false;
                        this.alert_tick();
                    }
                });
            }
            Due::Wait(_) | Due::Nothing => {}
        }
    }
}
