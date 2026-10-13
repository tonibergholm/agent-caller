//! The router (spec §5): the fast path first, else the intent model, then
//! Rust's validation, target resolution and confirmation level.
pub mod model;
pub mod prompt;
pub mod target;
mod validate;

use acd_protocol::Effect;

use crate::fast::{self, FastIntent, Locale, OptionBy, OptionPick};
use crate::refusal::Refusal;
use crate::safety::{self, Facts, Level, Origin, StepKind};
use crate::snapshot::{Snapshot, Visibility};
use crate::text::VoiceText;
use model::{IntentModel, ModelError};
use prompt::PromptOptions;
use target::{Provenance, Purpose, Resolution, Resolved};

/// One resolved step (spec §5.1). Card keys and request ids come from
/// the frozen snapshot, never from the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Answer { card: String, request_id: String, revision: u64, option_id: String },
    AnswerText { card: String, request_id: String, revision: u64, text: VoiceText },
    Send { card: String, text: VoiceText },
    Cancel,
    Read { card: String },
    /// A machine key, or every machine.
    Summarize { machine: Option<String> },
    Select { card: String },
}

impl Step {
    pub fn kind(&self) -> StepKind {
        match self {
            Step::Answer { .. } => StepKind::Answer,
            Step::AnswerText { .. } => StepKind::AnswerText,
            Step::Send { .. } => StepKind::Send,
            Step::Cancel => StepKind::Cancel,
            Step::Read { .. } => StepKind::Read,
            Step::Summarize { .. } => StepKind::Summarize,
            Step::Select { .. } => StepKind::Select,
        }
    }

    pub fn card(&self) -> Option<&str> {
        match self {
            Step::Answer { card, .. }
            | Step::AnswerText { card, .. }
            | Step::Send { card, .. }
            | Step::Read { card }
            | Step::Select { card } => Some(card),
            Step::Cancel | Step::Summarize { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub step: Step,
    pub origin: Origin,
    pub provenance: Option<Provenance>,
    pub off_screen: bool,
    pub level: Level,
}

/// What a which-one choice applies to the card the user picks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Template {
    Answer(OptionBy),
    AnswerText(VoiceText),
    Send(VoiceText),
    Read,
    Select,
}

impl Template {
    fn purpose(&self) -> Purpose {
        match self {
            Template::Answer(_) => Purpose::Answer,
            Template::AnswerText(_) => Purpose::AnswerText,
            Template::Send(_) => Purpose::Send,
            Template::Read => Purpose::Read,
            Template::Select => Purpose::Select,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub candidates: Vec<String>,
    pub template: Template,
    pub origin: Origin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Routed {
    Proposal(Proposal),
    WhichOne(Choice),
    Refused(Refusal),
}

impl Routed {
    /// A short tag for logs; never the transcript or a payload.
    pub fn tag(&self) -> &'static str {
        match self {
            Routed::Proposal(p) => match p.level {
                Level::None => "none",
                Level::Countdown => "countdown",
                Level::Confirm => "confirm",
            },
            Routed::WhichOne(_) => "which_one",
            Routed::Refused(_) => "refused",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RouteOptions {
    pub locale: Locale,
    pub prompt: PromptOptions,
}

/// Routes one utterance against a frozen snapshot.
pub async fn route(
    snapshot: &Snapshot,
    transcript: &VoiceText,
    model: Option<&dyn IntentModel>,
    opts: &RouteOptions,
) -> Routed {
    if let Some(intent) = fast::parse(transcript.expose(), opts.locale) {
        return route_fast(snapshot, intent);
    }
    let Some(model) = model else {
        return Routed::Refused(Refusal::ModelUnavailable);
    };
    let (prompt, ids) = prompt::build(snapshot, transcript, opts.prompt);
    match model.plan(&prompt).await {
        Ok(raw) => validate::validate(snapshot, &ids, transcript, &raw),
        Err(ModelError::Unavailable) => Routed::Refused(Refusal::ModelUnavailable),
        Err(ModelError::Timeout | ModelError::Failed) => Routed::Refused(Refusal::ModelFailed),
    }
}

/// Applies a which-one choice to the candidate the user picked. The pick
/// counts as a selection (D9).
pub fn bind(snapshot: &Snapshot, choice: &Choice, index: usize) -> Routed {
    let Some(card) = choice.candidates.get(index).and_then(|k| snapshot.card(k)) else {
        return Routed::Refused(Refusal::NoSuchTarget);
    };
    if let Err(r) = target::reach(snapshot, card, choice.template.purpose()) {
        return Routed::Refused(r);
    }
    let resolved = Resolved {
        card: card.key.clone(),
        provenance: Provenance::Selected,
        off_screen: snapshot.visibility(card) != Visibility::Visible,
    };
    apply(snapshot, &choice.template, &resolved, choice.origin)
}

fn route_fast(snapshot: &Snapshot, intent: FastIntent) -> Routed {
    match intent {
        FastIntent::Cancel => proposal(Step::Cancel, Origin::Fast, None),
        FastIntent::Next | FastIntent::Previous => match relative(snapshot, intent == FastIntent::Next) {
            Some(card) => proposal(Step::Select { card }, Origin::Fast, None),
            None => Routed::Refused(Refusal::NoTarget),
        },
        FastIntent::Read => targeted(snapshot, "", Purpose::Read, None, Template::Read, Origin::Fast),
        FastIntent::Approve | FastIntent::Deny | FastIntent::ApproveAlways | FastIntent::Option(_) => {
            let by = intent.option_by().expect("answer intents pick an option");
            targeted(snapshot, "", Purpose::Answer, None, Template::Answer(by), Origin::Fast)
        }
    }
}

fn targeted(
    snapshot: &Snapshot,
    said: &str,
    purpose: Purpose,
    pick: Option<&str>,
    template: Template,
    origin: Origin,
) -> Routed {
    match target::resolve(snapshot, said, purpose, pick) {
        Resolution::Refused(r) => Routed::Refused(r),
        Resolution::WhichOne(candidates) => Routed::WhichOne(Choice { candidates, template, origin }),
        Resolution::One(resolved) => apply(snapshot, &template, &resolved, origin),
    }
}

/// Builds the step for the resolved card and gives it its level.
fn apply(snapshot: &Snapshot, template: &Template, resolved: &Resolved, origin: Origin) -> Routed {
    let Some(card) = snapshot.card(&resolved.card) else {
        return Routed::Refused(Refusal::NoSuchTarget);
    };
    let (mut effect, mut by_number, mut confirm) = (None, false, false);
    let step = match template {
        Template::Answer(by) => {
            let Some(req) = card.pending.as_ref() else {
                return Routed::Refused(Refusal::NothingToAnswer);
            };
            let i = match fast::pick_option(req, *by) {
                OptionPick::One(i) => i,
                OptionPick::Several => return Routed::Refused(Refusal::AmbiguousOption),
                OptionPick::NoMatch => return Routed::Refused(Refusal::NoSuchOption),
            };
            // An option whose effect Rust cannot classify is not a valid
            // choice for a model's answer.
            if origin == Origin::Model && req.options[i].effect == Effect::Unknown {
                return Routed::Refused(Refusal::NoSuchOption);
            }
            effect = Some(req.options[i].effect);
            confirm = req.options[i].confirm;
            by_number = matches!(by, OptionBy::Number(_));
            Step::Answer {
                card: card.key.clone(),
                request_id: req.id.clone(),
                revision: req.revision,
                option_id: req.options[i].id.clone(),
            }
        }
        Template::AnswerText(text) => {
            let Some(req) = card.pending.as_ref() else {
                return Routed::Refused(Refusal::NothingToAnswer);
            };
            Step::AnswerText {
                card: card.key.clone(),
                request_id: req.id.clone(),
                revision: req.revision,
                text: text.clone(),
            }
        }
        Template::Send(text) => Step::Send { card: card.key.clone(), text: text.clone() },
        Template::Read => Step::Read { card: card.key.clone() },
        Template::Select => Step::Select { card: card.key.clone() },
    };
    let facts = Facts {
        kind: step.kind(),
        origin,
        effect,
        by_number,
        provenance: Some(resolved.provenance),
        off_screen: resolved.off_screen,
    };
    match safety::level(&facts) {
        Ok(level) => Routed::Proposal(Proposal {
            step,
            origin,
            provenance: Some(resolved.provenance),
            off_screen: resolved.off_screen,
            // An option that asks for a second tap never rides a countdown alone.
            level: if confirm { Level::Confirm } else { level },
        }),
        Err(r) => Routed::Refused(r),
    }
}

/// A step without a target card.
fn proposal(step: Step, origin: Origin, provenance: Option<Provenance>) -> Routed {
    let facts = Facts { kind: step.kind(), origin, effect: None, by_number: false, provenance, off_screen: false };
    match safety::level(&facts) {
        Ok(level) => Routed::Proposal(Proposal { step, origin, provenance, off_screen: false, level }),
        Err(r) => Routed::Refused(r),
    }
}

/// "next" and "previous" over the visible order, from the selection.
fn relative(snapshot: &Snapshot, forward: bool) -> Option<String> {
    let cards = snapshot.visible_cards();
    let at = snapshot.selected().and_then(|s| cards.iter().position(|c| c.key == s.key));
    let next = match (at, forward) {
        (None, _) => 0,
        (Some(i), true) => (i + 1).min(cards.len() - 1),
        (Some(i), false) => i.saturating_sub(1),
    };
    cards.get(next).map(|c| c.key.clone())
}
