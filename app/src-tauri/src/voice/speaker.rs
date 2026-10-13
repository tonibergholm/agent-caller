//! Speech output over the Swift bridge (voice spec §7, plan V10).
use acd_voice::speak::{Rate, Speaker};
use acd_voice::text::VoiceText;

use super::bridge::{VB_RATE_FAST, VB_RATE_NORMAL, VB_RATE_SLOW, ffi};

pub fn rate_code(rate: Rate) -> u32 {
    match rate {
        Rate::Slow => VB_RATE_SLOW,
        Rate::Normal => VB_RATE_NORMAL,
        Rate::Fast => VB_RATE_FAST,
    }
}

pub struct MacSpeaker;

impl Speaker for MacSpeaker {
    fn say(&self, generation: u64, text: &VoiceText, voice: Option<&str>, rate: Rate) {
        let voice = voice.unwrap_or("");
        let text = text.expose();
        // SAFETY: both buffers live for the call; Swift copies them (header contract).
        unsafe { ffi::vb_speech_say(generation, text.as_ptr(), text.len(), voice.as_ptr(), voice.len(), rate_code(rate)) }
    }

    fn stop(&self) {
        // SAFETY: no arguments; returns after speech stopped (header contract).
        unsafe { ffi::vb_speech_stop() }
    }
}

/// Asks Swift for the voices; a change arrives as `BridgeEvent::Voices`.
pub fn query_voices(force: bool) {
    // SAFETY: a plain integer; returns at once.
    unsafe { ffi::vb_speech_voices(u32::from(force)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_map_to_the_header_codes() {
        assert_eq!(rate_code(Rate::Slow), VB_RATE_SLOW);
        assert_eq!(rate_code(Rate::Normal), VB_RATE_NORMAL);
        assert_eq!(rate_code(Rate::Fast), VB_RATE_FAST);
    }
}
