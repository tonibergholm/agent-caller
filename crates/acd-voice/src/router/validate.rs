//! Checks a model reply against the frozen snapshot (spec §5.5).
use acd_protocol::text::has_forbidden_chars;

use super::model::{ModelPlan, ModelStep};
use super::prompt::IdMap;
use super::target::{self, Purpose, said_for_names};
use super::{Choice, Routed, Step, Template, proposal, targeted};
use crate::fast::{OptionBy, normalize};
use crate::refusal::Refusal;
use crate::safety::Origin;
use crate::snapshot::{Snapshot, Visibility};
use crate::text::VoiceText;

/// acd-client's `ANSWER_TEXT_MAX_BYTES`.
const ANSWER_TEXT_MAX_BYTES: usize = 16 * 1024;
/// acd-client's reply limit.
const REPLY_MAX_BYTES: usize = 64 * 1024;

pub(super) fn validate(snapshot: &Snapshot, ids: &IdMap, transcript: &VoiceText, raw: &VoiceText) -> Routed {
    let Ok(plan) = serde_json::from_str::<ModelPlan>(raw.expose()) else {
        return Routed::Refused(Refusal::DidNotUnderstand);
    };
    match plan.step {
        ModelStep::None => Routed::Refused(Refusal::DidNotUnderstand),
        ModelStep::Cancel => proposal(Step::Cancel, Origin::Model, None),
        ModelStep::Summarize => match plan.machine.as_deref() {
            None => proposal(Step::Summarize { machine: None }, Origin::Model, None),
            Some(name) => match machine_key(snapshot, name) {
                Some(key) => proposal(Step::Summarize { machine: Some(key) }, Origin::Model, None),
                None => Routed::Refused(Refusal::NoSuchTarget),
            },
        },
        ModelStep::Answer => {
            let Some(n) = plan
                .option
                .as_deref()
                .and_then(|o| o.parse::<u8>().ok())
                .filter(|n| (1..=9).contains(n))
            else {
                return Routed::Refused(Refusal::DidNotUnderstand);
            };
            aim(snapshot, ids, transcript, &plan, Purpose::Answer, Template::Answer(OptionBy::Number(n)), None)
        }
        ModelStep::AnswerText => {
            let Some(text) = checked_text(plan.text.as_ref(), ANSWER_TEXT_MAX_BYTES) else {
                return Routed::Refused(Refusal::BadText);
            };
            let payload = text.expose().to_owned();
            aim(snapshot, ids, transcript, &plan, Purpose::AnswerText, Template::AnswerText(text), Some(&payload))
        }
        ModelStep::Send => {
            let Some(text) = checked_text(plan.text.as_ref(), REPLY_MAX_BYTES) else {
                return Routed::Refused(Refusal::BadText);
            };
            let payload = text.expose().to_owned();
            aim(snapshot, ids, transcript, &plan, Purpose::Send, Template::Send(text), Some(&payload))
        }
        ModelStep::Read => aim(snapshot, ids, transcript, &plan, Purpose::Read, Template::Read, None),
        ModelStep::Select => aim(snapshot, ids, transcript, &plan, Purpose::Select, Template::Select, None),
    }
}

/// Trimmed, non-empty, within `max` bytes, no forbidden characters.
fn checked_text(text: Option<&VoiceText>, max: usize) -> Option<VoiceText> {
    let text = text?.expose().replace("\r\n", "\n");
    let text = text.trim();
    (!text.is_empty() && text.len() <= max && !has_forbidden_chars(text)).then(|| VoiceText::new(text))
}

fn machine_key(snapshot: &Snapshot, name: &str) -> Option<String> {
    let wanted = normalize(name);
    snapshot
        .cards
        .iter()
        .find(|c| normalize(&c.machine_name) == wanted)
        .map(|c| c.machine_key.clone())
}

/// Maps the model's target to a card and lets Rust resolve it (D10).
fn aim(
    snapshot: &Snapshot,
    ids: &IdMap,
    transcript: &VoiceText,
    plan: &ModelPlan,
    purpose: Purpose,
    template: Template,
    payload: Option<&str>,
) -> Routed {
    let said = said_for_names(transcript.expose(), payload);
    // Removing the payload may only narrow what the transcript names: it
    // must not drop the user's names, and must not add or create one that
    // the whole transcript does not name (ordinals are read from the full
    // transcript's own words).
    let full = target::named(snapshot, &normalize(transcript.expose()));
    let stripped = target::named(snapshot, &said);
    let consistent = match (&full, &stripped) {
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
        (Some(f), Some(st)) => st.iter().all(|k| f.contains(k)),
    };
    if !consistent {
        return Routed::Refused(Refusal::TargetMismatch);
    }
    if let Some(candidates) = &plan.candidates {
        let Some(keys) = candidates
            .iter()
            .map(|id| ids.agent(id).map(str::to_owned))
            .collect::<Option<Vec<String>>>()
        else {
            return Routed::Refused(Refusal::DidNotUnderstand);
        };
        // Rust's own reading of the transcript wins over the model's list.
        if target::named(snapshot, &said).is_some() {
            return targeted(snapshot, &said, purpose, None, template, Origin::Model);
        }
        let fitting: Vec<String> = keys
            .into_iter()
            .filter(|k| {
                snapshot.card(k).is_some_and(|c| {
                    snapshot.visibility(c) == Visibility::Visible && target::fits(c, purpose).is_ok()
                })
            })
            .collect();
        if fitting.is_empty() {
            return Routed::Refused(Refusal::NoSuchTarget);
        }
        return Routed::WhichOne(Choice { candidates: fitting, template, origin: Origin::Model });
    }
    let pick = match (plan.agent.as_deref(), plan.request.as_deref()) {
        (Some(a), None) => ids.agent(a),
        (None, Some(r)) => ids.request(r),
        (Some(a), Some(r)) => match (ids.agent(a), ids.request(r)) {
            (Some(x), Some(y)) if x == y => Some(x),
            _ => None,
        },
        (None, None) => None,
    };
    let Some(pick) = pick else {
        return Routed::Refused(Refusal::DidNotUnderstand);
    };
    targeted(snapshot, &said, purpose, Some(pick), template, Origin::Model)
}
