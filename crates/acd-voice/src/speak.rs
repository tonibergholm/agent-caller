//! Spoken output (voice spec §7): the speaker seam and the alert rules.
//! Plain Rust with the time passed in, so every rule is tested on Linux.
//! The platform sits behind `Speaker`; on macOS the app implements it over
//! the Swift bridge (plan V10).
use std::collections::HashSet;
use std::time::{Duration, Instant};

use acd_protocol::AgentRole;
use serde::{Deserialize, Serialize};

use crate::snapshot::{CardSnap, Group, Snapshot};
use crate::speech;
use crate::text::VoiceText;

/// Spec §7: at most one alert per 5 s.
pub const ALERT_GAP: Duration = Duration::from_secs(5);
/// How soon a waiting alert looks again while voice is busy.
pub const ALERT_RETRY: Duration = Duration::from_secs(1);
/// Requests remembered so a reconnect announces nothing (plan V14).
pub(crate) const SEEN_MAX: usize = 1024;

/// The speaking rate (spec §8, plan V15).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rate {
    Slow,
    #[default]
    Normal,
    Fast,
}

/// Spec §8: spoken alerts on or off, voice and rate. Settings, never content.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpeechSettings {
    pub alerts: bool,
    /// A platform voice id; `None` is the system's English voice.
    pub voice: Option<String>,
    pub rate: Rate,
}

/// Text to speech (spec §4.1). Both methods return at once, and neither
/// calls back into the controller, not even synchronously to report a
/// failure: it calls them under its core lock, so a callback would deadlock.
/// The end of speech is reported later, from the platform's own queue.
pub trait Speaker: Send + Sync {
    /// Speaks `text` as `generation`, replacing any speech. When it ends,
    /// the platform calls `Controller::spoken(generation)`.
    fn say(&self, generation: u64, text: &VoiceText, voice: Option<&str>, rate: Rate);
    /// Stops all speech before it returns, so the microphone may open next
    /// (plan V12). Nothing follows for the stopped speech.
    fn stop(&self);
}

/// How long a reply may speak before the controller cuts it (plan V11).
pub fn speech_cap(text: &VoiceText) -> Duration {
    Duration::from_millis(1500 + 100 * text.char_len() as u64).clamp(Duration::from_secs(10), Duration::from_secs(120))
}

/// Cards an alert may name (spec §7, plan V14). The rules are applied here
/// directly, not through `Snapshot::visibility`: that one treats any card
/// missing from `presentation.visible` as never visible, and a request that
/// just arrived is not in it until the panel republishes (or at all, before
/// the first publish).
pub fn alert_cards(s: &Snapshot) -> Vec<&CardSnap> {
    s.cards
        .iter()
        .filter(|c| {
            c.group == Group::Request
                && c.online
                && c.pending.is_some()
                && !c.idle_hidden
                && !(c.role == AgentRole::Subagent && !s.show_subagents)
                && s.presentation.machine_filter.as_deref().is_none_or(|m| m == c.machine_key)
        })
        .collect()
}

/// The requests an alert may announce now.
fn keys(s: &Snapshot) -> Vec<(String, String)> {
    alert_cards(s)
        .into_iter()
        .filter_map(|c| c.pending.as_ref().map(|r| (c.key.clone(), r.id.clone())))
        .collect()
}

/// Every request that is waiting, whether or not an alert may name it now.
/// The baseline and the seen set use this one, so a request that was already
/// waiting is never announced later because something made it eligible: an
/// offline machine reconnecting, the machine filter clearing, subagents shown.
fn waiting(s: &Snapshot) -> Vec<(String, String)> {
    s.cards
        .iter()
        .filter_map(|c| c.pending.as_ref().map(|r| (c.key.clone(), r.id.clone())))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Due {
    Speak(VoiceText),
    /// Look again after this long.
    Wait(Duration),
    Nothing,
}

/// Which requests were announced, and when (plan V14).
#[derive(Debug, Default)]
pub struct Alerts {
    /// `None` until the baseline is taken.
    seen: Option<HashSet<(String, String)>>,
    queue: Vec<(String, String)>,
    last: Option<Instant>,
}

impl Alerts {
    /// Alerts or voice turned off: the next look takes a new baseline.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Queues requests not seen before. The first call only takes a baseline.
    pub fn observe(&mut self, s: &Snapshot) {
        let now = waiting(s);
        let Some(seen) = &mut self.seen else {
            self.seen = Some(now.into_iter().collect());
            return;
        };
        let eligible = keys(s);
        for key in &now {
            // Remembered either way; queued only if an alert may name it now.
            if seen.insert(key.clone()) && eligible.contains(key) {
                self.queue.push(key.clone());
            }
        }
        if seen.len() > SEEN_MAX {
            seen.retain(|k| now.contains(k));
        }
        // Bounded even if `due` is not called: only live, eligible requests wait.
        self.queue.retain(|k| eligible.contains(k));
    }

    /// What to say now. Queued requests that no longer need the user are
    /// dropped first. `busy`: recording, a step on the HUD, or speech.
    pub fn due(&mut self, s: &Snapshot, now: Instant, busy: bool) -> Due {
        let live = keys(s);
        self.queue.retain(|k| live.contains(k));
        if self.queue.is_empty() {
            return Due::Nothing;
        }
        if busy {
            return Due::Wait(ALERT_RETRY);
        }
        if let Some(last) = self.last
            && now < last + ALERT_GAP
        {
            return Due::Wait(last + ALERT_GAP - now);
        }
        let queued = std::mem::take(&mut self.queue);
        let cards: Vec<&CardSnap> = queued.iter().filter_map(|(card, _)| s.card(card)).collect();
        self.last = Some(now);
        speech::alerts(&cards).map_or(Due::Nothing, Due::Speak)
    }

    #[cfg(test)]
    fn seen_len(&self) -> usize {
        self.seen.as_ref().map_or(0, HashSet::len)
    }
}

/// A `Speaker` that records calls (tests).
#[cfg(any(test, feature = "testing"))]
pub mod fake {
    use std::sync::Mutex;

    use super::{Rate, Speaker};
    use crate::text::VoiceText;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Said {
        Say { generation: u64, text: String, voice: Option<String>, rate: Rate },
        Stop,
    }

    #[derive(Default)]
    pub struct FakeSpeaker {
        pub calls: Mutex<Vec<Said>>,
    }

    impl FakeSpeaker {
        pub fn calls(&self) -> Vec<Said> {
            self.calls.lock().unwrap().clone()
        }
        pub fn last_say(&self) -> Option<(u64, String)> {
            self.calls().into_iter().rev().find_map(|c| match c {
                Said::Say { generation, text, .. } => Some((generation, text)),
                Said::Stop => None,
            })
        }
        pub fn says(&self) -> usize {
            self.calls().iter().filter(|c| matches!(c, Said::Say { .. })).count()
        }
    }

    impl Speaker for FakeSpeaker {
        fn say(&self, generation: u64, text: &VoiceText, voice: Option<&str>, rate: Rate) {
            let said = Said::Say { generation, text: text.expose().to_owned(), voice: voice.map(str::to_owned), rate };
            self.calls.lock().unwrap().push(said);
        }
        fn stop(&self) {
            self.calls.lock().unwrap().push(Said::Stop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::fixtures::*;
    use acd_protocol::AgentRole;

    const fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn with_request(machine: &str, agent: &str, request: &str) -> CardSnap {
        let mut c = pending(machine, agent);
        c.pending.as_mut().unwrap().id = request.into();
        c
    }

    /// Review Focus 4.
    #[test]
    fn startup_and_reconnect_announce_nothing() {
        let t0 = Instant::now();
        let s = snapshot(vec![pending("minos", "a")]);
        let mut a = Alerts::default();
        a.observe(&s);
        assert_eq!(a.due(&s, t0, false), Due::Nothing, "the first look is a baseline");
        a.observe(&snapshot(vec![]));
        a.observe(&s);
        assert_eq!(a.due(&s, t0, false), Due::Nothing, "a reconnect brings back a seen request");
    }

    #[test]
    fn a_new_request_is_announced_once() {
        let t0 = Instant::now();
        let mut a = Alerts::default();
        a.observe(&snapshot(vec![]));
        let s = snapshot(vec![pending("minos", "a")]);
        a.observe(&s);
        assert_eq!(a.due(&s, t0, false), Due::Speak(VoiceText::new("minos Claude needs you: bash approval")));
        a.observe(&s);
        assert_eq!(a.due(&s, t0 + ALERT_GAP, false), Due::Nothing);
    }

    /// Review Focus 4: one per 5 s, the rest counted.
    #[test]
    fn a_burst_is_one_alert_with_a_count() {
        let t0 = Instant::now();
        let mut a = Alerts::default();
        a.observe(&snapshot(vec![]));
        let one = snapshot(vec![pending("minos", "a")]);
        a.observe(&one);
        assert!(matches!(a.due(&one, t0, false), Due::Speak(_)));
        let three = snapshot(vec![pending("minos", "a"), pending("minos", "b"), pending("studio", "c")]);
        a.observe(&three);
        assert_eq!(a.due(&three, t0 + ms(1000), false), Due::Wait(ms(4000)));
        assert_eq!(
            a.due(&three, t0 + ALERT_GAP, false),
            Due::Speak(VoiceText::new("minos Claude needs you: bash approval. 1 more needs you."))
        );
    }

    #[test]
    fn a_request_answered_before_its_turn_is_not_announced() {
        let t0 = Instant::now();
        let mut a = Alerts::default();
        a.observe(&snapshot(vec![]));
        let s = snapshot(vec![pending("minos", "a")]);
        a.observe(&s);
        assert_eq!(a.due(&s, t0, true), Due::Wait(ALERT_RETRY), "busy: wait");
        let gone = snapshot(vec![]);
        a.observe(&gone);
        assert_eq!(a.due(&gone, t0 + ALERT_RETRY, false), Due::Nothing);
        let next = snapshot(vec![with_request("minos", "a", "r-new")]);
        a.observe(&next);
        assert!(matches!(a.due(&next, t0 + ALERT_RETRY, false), Due::Speak(_)), "a new request on the same card counts");
    }

    /// Review: a request that was already waiting is never announced later
    /// because it became eligible (a machine reconnecting).
    #[test]
    fn a_request_waiting_on_an_offline_machine_is_not_announced_on_reconnect() {
        let t0 = Instant::now();
        let mut offline = pending("minos", "a");
        offline.online = false;
        let mut a = Alerts::default();
        a.observe(&snapshot(vec![offline]));
        let back = snapshot(vec![pending("minos", "a")]);
        a.observe(&back);
        assert_eq!(a.due(&back, t0, false), Due::Nothing);
    }

    /// Same for a machine filter that clears, and for subagents that are shown.
    #[test]
    fn clearing_the_filter_or_showing_subagents_announces_nothing_old() {
        let t0 = Instant::now();
        let mut sub = pending("m", "sub");
        sub.role = AgentRole::Subagent;
        let mut s = snapshot(vec![pending("m", "a"), pending("other", "b"), sub]);
        s.presentation.machine_filter = Some("m".into());
        s.show_subagents = false;
        let mut a = Alerts::default();
        a.observe(&s);
        s.presentation.machine_filter = None;
        s.show_subagents = true;
        a.observe(&s);
        assert_eq!(a.due(&s, t0, false), Due::Nothing);
    }

    /// A request that arrives while its machine is filtered out is not
    /// announced when the filter clears either: alerts respect visibility
    /// when the request arrives (spec §7).
    #[test]
    fn a_request_that_arrived_while_filtered_out_stays_quiet() {
        let t0 = Instant::now();
        let mut a = Alerts::default();
        let mut s = snapshot(vec![pending("m", "a")]);
        s.presentation.machine_filter = Some("m".into());
        a.observe(&s);
        let mut with_other = snapshot(vec![pending("m", "a"), pending("other", "b")]);
        with_other.presentation.machine_filter = Some("m".into());
        a.observe(&with_other);
        with_other.presentation.machine_filter = None;
        a.observe(&with_other);
        assert_eq!(a.due(&with_other, t0, false), Due::Nothing);
    }

    #[test]
    fn the_queue_stays_bounded_without_due() {
        let mut a = Alerts::default();
        a.observe(&snapshot(vec![]));
        for i in 0..50 {
            a.observe(&snapshot(vec![with_request("m", "a", &format!("r{i}"))]));
        }
        assert!(a.queue.len() <= 1, "only the live request waits, not {}", a.queue.len());
    }

    /// The machine filter holds the machine's key, which the panel publishes
    /// (not its display name): a card whose two differ proves which is compared.
    #[test]
    fn the_filter_compares_the_machine_key_not_its_name() {
        let mut c = pending("k1", "a");
        c.machine_name = "Minos".into();
        let mut s = snapshot(vec![c]);
        s.presentation.machine_filter = Some("k1".into());
        assert_eq!(alerted(&s), ["k1/a"]);
        s.presentation.machine_filter = Some("Minos".into());
        assert!(alerted(&s).is_empty());
    }

    fn alerted(s: &Snapshot) -> Vec<&str> {
        alert_cards(s).iter().map(|c| c.key.as_str()).collect()
    }

    /// Review blocker 1: a request that arrives before the panel publishes
    /// is not in `presentation.visible`, and the controller freezes with an
    /// empty default presentation until the first publish. It still alerts.
    #[test]
    fn a_card_alerts_with_an_empty_presentation() {
        let mut s = snapshot(vec![pending("m", "a")]);
        s.presentation = Default::default();
        assert_eq!(alerted(&s), ["m/a"]);
    }

    /// Spec §7: a hidden subagent is never announced; a shown one is.
    #[test]
    fn hidden_subagents_do_not_alert() {
        let mut sub = pending("m", "sub");
        sub.role = AgentRole::Subagent;
        let mut s = snapshot(vec![sub]);
        s.show_subagents = false;
        assert!(alerted(&s).is_empty());
        s.show_subagents = true;
        assert_eq!(alerted(&s), ["m/sub"]);
    }

    /// Spec §7 "machine visibility" (owner decision 2).
    #[test]
    fn the_machine_filter_limits_alerts() {
        let mut s = snapshot(vec![pending("m", "a"), pending("other", "b")]);
        s.presentation.machine_filter = Some("m".into());
        assert_eq!(alerted(&s), ["m/a"]);
        s.presentation.machine_filter = None;
        assert_eq!(alerted(&s), ["m/a", "other/b"]);
    }

    #[test]
    fn hidden_idle_offline_and_non_request_cards_do_not_alert() {
        let mut idle = pending("m", "idle");
        idle.idle_hidden = true;
        let mut offline = pending("m", "off");
        offline.online = false;
        let working = working("m", "w");
        let mut done = pending("m", "done");
        done.group = Group::Done;
        assert!(alerted(&snapshot(vec![idle, offline, working, done])).is_empty());
    }

    #[test]
    fn reset_forgets_everything() {
        let t0 = Instant::now();
        let mut a = Alerts::default();
        a.observe(&snapshot(vec![]));
        let s = snapshot(vec![pending("minos", "a")]);
        a.observe(&s);
        assert!(matches!(a.due(&s, t0, false), Due::Speak(_)));
        a.reset();
        a.observe(&s);
        assert_eq!(a.due(&s, t0 + ms(1), false), Due::Nothing, "turning alerts on again takes a new baseline");
        // The gap was forgotten too: a request that arrives next is spoken at once.
        let more = snapshot(vec![pending("minos", "a"), pending("minos", "b")]);
        a.observe(&more);
        assert!(matches!(a.due(&more, t0 + ms(2), false), Due::Speak(_)), "reset cleared the 5 s gap");
    }

    #[test]
    fn the_seen_set_is_bounded() {
        let mut a = Alerts::default();
        a.observe(&snapshot(vec![]));
        for i in 0..(SEEN_MAX + 10) {
            a.observe(&snapshot(vec![with_request("m", "a", &format!("r{i}"))]));
        }
        assert!(a.seen_len() <= SEEN_MAX);
        // Trimming forgets what is gone, never what is still waiting: the live
        // request is not announced a second time.
        let live = snapshot(vec![with_request("m", "a", &format!("r{}", SEEN_MAX + 9))]);
        let t0 = Instant::now();
        let _ = a.due(&live, t0, false);
        a.observe(&live);
        assert_eq!(a.due(&live, t0 + ALERT_GAP + ALERT_GAP, false), Due::Nothing);
    }

    #[test]
    fn the_speech_cap_follows_the_length_within_bounds() {
        assert_eq!(speech_cap(&VoiceText::new("short")), Duration::from_secs(10));
        assert_eq!(speech_cap(&VoiceText::new("x".repeat(200))), ms(21_500));
        assert_eq!(speech_cap(&VoiceText::new("x".repeat(5000))), Duration::from_secs(120));
    }

    #[test]
    fn rates_serialize_in_snake_case_and_default_to_normal() {
        assert_eq!(serde_json::to_string(&Rate::Slow).unwrap(), "\"slow\"");
        assert_eq!(serde_json::to_string(&Rate::Fast).unwrap(), "\"fast\"");
        assert_eq!(serde_json::from_str::<Rate>("\"normal\"").unwrap(), Rate::Normal);
        assert_eq!(Rate::default(), Rate::Normal);
        assert_eq!(SpeechSettings::default(), SpeechSettings { alerts: false, voice: None, rate: Rate::Normal });
    }
}
