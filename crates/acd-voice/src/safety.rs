//! The confirmation table (spec §6.1). It is exhaustive: any case not
//! listed is refused.
use acd_protocol::Effect;
use serde::Serialize;

use crate::refusal::Refusal;
use crate::router::target::Provenance;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// `cancel` acts at once; `read`, `summarize` and `select` change nothing.
    None,
    /// The 2 s countdown; Esc, a click or the trigger cancels.
    Countdown,
    /// Enter, a click or "confirm", then the countdown.
    Confirm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Fast,
    Model,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    Answer,
    AnswerText,
    Send,
    Cancel,
    Read,
    Summarize,
    Select,
}

/// What Rust established about a step. The model sets none of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Facts {
    pub kind: StepKind,
    pub origin: Origin,
    /// The chosen option's effect, for an answer.
    pub effect: Option<Effect>,
    /// The option came from "option N".
    pub by_number: bool,
    pub provenance: Option<Provenance>,
    /// The target is not on screen: hidden by the machine filter, in a
    /// collapsed Done section, or a hidden idle agent (spec §5.2, D7).
    pub off_screen: bool,
}

pub fn level(f: &Facts) -> Result<Level, Refusal> {
    match f.kind {
        StepKind::Cancel | StepKind::Read | StepKind::Summarize | StepKind::Select => {
            return Ok(Level::None);
        }
        StepKind::Answer | StepKind::AnswerText | StepKind::Send => {}
    }
    // Confirm, then countdown: every model-produced mutation, any
    // allow_always option, any target not on screen (spec §5.2, D7).
    if f.origin == Origin::Model || f.effect == Some(Effect::AllowAlways) || f.off_screen {
        return Ok(Level::Confirm);
    }
    // Countdown: a fast-path answer with effect allow or deny (or "option
    // N") on a selected or only-pending target.
    let target = matches!(f.provenance, Some(Provenance::Selected | Provenance::OnlyPending));
    let effect = match f.effect {
        Some(Effect::Allow | Effect::Deny) => true,
        Some(Effect::Choose) => f.by_number,
        _ => false,
    };
    if f.kind == StepKind::Answer && target && effect {
        return Ok(Level::Countdown);
    }
    Err(Refusal::NotPermitted)
}
#[cfg(test)]
mod tests {
    use super::*;
    use Effect::*;
    use Provenance::*;

    const KINDS: [StepKind; 7] = [
        StepKind::Answer,
        StepKind::AnswerText,
        StepKind::Send,
        StepKind::Cancel,
        StepKind::Read,
        StepKind::Summarize,
        StepKind::Select,
    ];
    const EFFECTS: [Option<Effect>; 6] = [None, Some(Allow), Some(AllowAlways), Some(Deny), Some(Choose), Some(Unknown)];
    const PROVENANCES: [Option<Provenance>; 5] = [None, Some(Named), Some(Selected), Some(OnlyPending), Some(Inferred)];

    fn fast_answer(effect: Effect, provenance: Provenance) -> Facts {
        Facts {
            kind: StepKind::Answer,
            origin: Origin::Fast,
            effect: Some(effect),
            by_number: false,
            provenance: Some(provenance),
            off_screen: false,
        }
    }

    #[test]
    fn steps_that_change_nothing_need_nothing() {
        for kind in [StepKind::Cancel, StepKind::Read, StepKind::Summarize, StepKind::Select] {
            for origin in [Origin::Fast, Origin::Model] {
                let f = Facts { kind, origin, effect: None, by_number: false, provenance: None, off_screen: true };
                assert_eq!(level(&f), Ok(Level::None), "{kind:?} {origin:?}");
            }
        }
    }

    #[test]
    fn fast_allow_or_deny_on_a_selected_or_only_pending_card_counts_down() {
        for effect in [Allow, Deny] {
            for provenance in [Selected, OnlyPending] {
                assert_eq!(level(&fast_answer(effect, provenance)), Ok(Level::Countdown));
            }
        }
    }

    #[test]
    fn option_n_counts_down_unless_it_is_allow_always() {
        let mut f = fast_answer(Choose, Selected);
        f.by_number = true;
        assert_eq!(level(&f), Ok(Level::Countdown));
        f.effect = Some(AllowAlways);
        assert_eq!(level(&f), Ok(Level::Confirm));
    }

    #[test]
    fn model_mutations_always_confirm() {
        for kind in [StepKind::Answer, StepKind::AnswerText, StepKind::Send] {
            for provenance in PROVENANCES {
                let f = Facts { kind, origin: Origin::Model, effect: Some(Allow), by_number: false, provenance, off_screen: false };
                assert_eq!(level(&f), Ok(Level::Confirm), "{kind:?} {provenance:?}");
            }
        }
    }

    #[test]
    fn allow_always_and_off_screen_targets_always_confirm() {
        assert_eq!(level(&fast_answer(AllowAlways, Selected)), Ok(Level::Confirm));
        let mut f = fast_answer(Allow, Selected);
        f.off_screen = true;
        assert_eq!(level(&f), Ok(Level::Confirm));
    }

    #[test]
    fn unlisted_cases_are_refused() {
        assert_eq!(level(&fast_answer(Allow, Named)), Err(Refusal::NotPermitted));
        assert_eq!(level(&fast_answer(Allow, Inferred)), Err(Refusal::NotPermitted));
        assert_eq!(level(&fast_answer(Choose, Selected)), Err(Refusal::NotPermitted), "choose needs a number");
        assert_eq!(level(&fast_answer(Unknown, Selected)), Err(Refusal::NotPermitted));
        let mut f = fast_answer(Allow, Selected);
        f.provenance = None;
        assert_eq!(level(&f), Err(Refusal::NotPermitted));
        f = Facts { kind: StepKind::Send, origin: Origin::Fast, effect: None, by_number: false, provenance: Some(Selected), off_screen: false };
        assert_eq!(level(&f), Err(Refusal::NotPermitted));
    }

    /// The whole table: countdown happens exactly in the listed cases, and
    /// a mutation is never level None.
    #[test]
    fn countdown_happens_only_in_the_listed_cases() {
        for kind in KINDS {
            for origin in [Origin::Fast, Origin::Model] {
                for effect in EFFECTS {
                    for by_number in [false, true] {
                        for provenance in PROVENANCES {
                            for off_screen in [false, true] {
                                let f = Facts { kind, origin, effect, by_number, provenance, off_screen };
                                let got = level(&f);
                                let listed = kind == StepKind::Answer
                                    && origin == Origin::Fast
                                    && !off_screen
                                    && matches!(provenance, Some(Selected | OnlyPending))
                                    && (matches!(effect, Some(Allow | Deny)) || (by_number && effect == Some(Choose)));
                                assert_eq!(got == Ok(Level::Countdown), listed, "{f:?}");
                                let mutation = matches!(kind, StepKind::Answer | StepKind::AnswerText | StepKind::Send);
                                if mutation {
                                    assert_ne!(got, Ok(Level::None), "{f:?}");
                                }
                                if mutation && origin == Origin::Model {
                                    assert_eq!(got, Ok(Level::Confirm), "{f:?}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
