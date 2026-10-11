//! The deterministic fast path (spec §5.3): whole-utterance phrases,
//! resolved without a model, and option choice by effect.
use acd_protocol::Effect;

use crate::snapshot::RequestSnap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FastIntent {
    Approve,
    Deny,
    ApproveAlways,
    /// "option N", by the displayed number (1–9).
    Option(u8),
    Cancel,
    Read,
    Next,
    Previous,
}

impl FastIntent {
    /// How an answer intent picks its option; `None` for the others.
    pub fn option_by(self) -> Option<OptionBy> {
        match self {
            Self::Approve => Some(OptionBy::Effect(Effect::Allow)),
            Self::Deny => Some(OptionBy::Effect(Effect::Deny)),
            Self::ApproveAlways => Some(OptionBy::Effect(Effect::AllowAlways)),
            Self::Option(n) => Some(OptionBy::Number(n)),
            Self::Cancel | Self::Read | Self::Next | Self::Previous => None,
        }
    }
}

/// The phrase table is per locale (spec §5.3). English first.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Locale {
    #[default]
    En,
}

const EN: &[(&str, FastIntent)] = &[
    ("approve", FastIntent::Approve),
    ("approve it", FastIntent::Approve),
    ("approve this", FastIntent::Approve),
    ("approve that", FastIntent::Approve),
    ("yes", FastIntent::Approve),
    ("allow", FastIntent::Approve),
    ("allow it", FastIntent::Approve),
    ("allow this", FastIntent::Approve),
    ("deny", FastIntent::Deny),
    ("deny it", FastIntent::Deny),
    ("deny this", FastIntent::Deny),
    ("deny that", FastIntent::Deny),
    ("no", FastIntent::Deny),
    ("yes always", FastIntent::ApproveAlways),
    ("always allow", FastIntent::ApproveAlways),
    ("allow always", FastIntent::ApproveAlways),
    ("approve always", FastIntent::ApproveAlways),
    ("cancel", FastIntent::Cancel),
    ("cancel that", FastIntent::Cancel),
    ("undo", FastIntent::Cancel),
    ("undo that", FastIntent::Cancel),
    ("read it", FastIntent::Read),
    ("read this", FastIntent::Read),
    ("read the question", FastIntent::Read),
    ("next", FastIntent::Next),
    ("previous", FastIntent::Previous),
];

const EN_NUMBERS: [&str; 9] = [
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];

/// Lower case, punctuation trimmed from each word, single spaces.
pub fn normalize(s: &str) -> String {
    s.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The fast-path intent of a whole utterance. Anything else, including a
/// phrase with extra words ("approve the minos one"), is for the model.
pub fn parse(transcript: &str, locale: Locale) -> Option<FastIntent> {
    let said = normalize(transcript);
    match locale {
        Locale::En => {
            if let Some(n) = said.strip_prefix("option ") {
                return option_number(n).map(FastIntent::Option);
            }
            EN.iter().find(|(p, _)| *p == said).map(|(_, i)| *i)
        }
    }
}

/// "first" to "ninth": ordinals over the visible order, and which-one.
pub(crate) const ORDINALS: [&str; 9] = [
    "first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth",
];
pub(crate) const ORDINALS_SHORT: [&str; 9] = ["1st", "2nd", "3rd", "4th", "5th", "6th", "7th", "8th", "9th"];

/// Zero-based index of an English ordinal word or short form ("second",
/// "2nd"), or `None`. The one ordinal lookup: Task 3's which-one parsing
/// calls this instead of repeating it.
pub(crate) fn ordinal_index(word: &str) -> Option<usize> {
    ORDINALS
        .iter()
        .position(|o| *o == word)
        .or_else(|| ORDINALS_SHORT.iter().position(|o| *o == word))
}

/// An answer to a HUD that waits for input (D21).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    Confirm,
    Cancel,
    /// Zero-based candidate index.
    Choose(usize),
}

const EN_CONFIRM: &[&str] = &["confirm", "confirm it", "confirm this", "confirm that"];

/// "confirm" (spec §6.1), a cancel phrase, or an ordinal or number for
/// which-one ("the first", "number two", "2"; spec §5.2 rule 5).
pub fn parse_reply(transcript: &str, locale: Locale) -> Option<Reply> {
    let said = normalize(transcript);
    match locale {
        Locale::En => {
            if EN_CONFIRM.contains(&said.as_str()) {
                return Some(Reply::Confirm);
            }
            if parse(&said, locale) == Some(FastIntent::Cancel) {
                return Some(Reply::Cancel);
            }
            let core = said.strip_prefix("the ").unwrap_or(&said);
            let core = core
                .strip_prefix("number ")
                .or_else(|| core.strip_prefix("option "))
                .unwrap_or(core);
            let core = core.strip_suffix(" one").unwrap_or(core);
            ordinal_index(core)
                .or_else(|| option_number(core).map(|n| usize::from(n) - 1))
                .map(Reply::Choose)
        }
    }
}

fn option_number(word: &str) -> Option<u8> {
    if let Ok(n) = word.parse::<u8>() {
        return (1..=9).contains(&n).then_some(n);
    }
    EN_NUMBERS.iter().position(|w| *w == word).map(|i| i as u8 + 1)
}

/// How an answer picks its option (spec §5.3): by effect, never by label
/// or position, except "option N".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionBy {
    Effect(Effect),
    Number(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionPick {
    /// Index into `RequestSnap::options`.
    One(usize),
    Several,
    NoMatch,
}

pub fn pick_option(req: &RequestSnap, by: OptionBy) -> OptionPick {
    match by {
        OptionBy::Number(n) => {
            let i = usize::from(n).wrapping_sub(1);
            match req.options.get(i) {
                Some(o) if o.selectable => OptionPick::One(i),
                _ => OptionPick::NoMatch,
            }
        }
        OptionBy::Effect(effect) => {
            let hits: Vec<usize> = req
                .options
                .iter()
                .enumerate()
                .filter(|(_, o)| o.selectable && o.effect == effect)
                .map(|(i, _)| i)
                .collect();
            match hits.as_slice() {
                [i] => OptionPick::One(*i),
                [] => OptionPick::NoMatch,
                _ => OptionPick::Several,
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::fixtures::{option, permission};

    fn p(s: &str) -> Option<FastIntent> {
        parse(s, Locale::En)
    }

    #[test]
    fn spec_phrases_match_whole_utterances() {
        for (said, want) in [
            ("approve", FastIntent::Approve),
            ("Yes.", FastIntent::Approve),
            ("allow", FastIntent::Approve),
            ("approve this", FastIntent::Approve),
            ("deny", FastIntent::Deny),
            ("No!", FastIntent::Deny),
            ("Yes, always.", FastIntent::ApproveAlways),
            ("always allow", FastIntent::ApproveAlways),
            ("Option two", FastIntent::Option(2)),
            ("option 3", FastIntent::Option(3)),
            ("cancel", FastIntent::Cancel),
            ("undo that", FastIntent::Cancel),
            ("read it", FastIntent::Read),
            ("read the question", FastIntent::Read),
            ("next", FastIntent::Next),
            ("previous", FastIntent::Previous),
        ] {
            assert_eq!(p(said), Some(want), "{said}");
        }
    }

    #[test]
    fn plain_yes_never_means_always() {
        assert_eq!(p("yes"), Some(FastIntent::Approve));
        assert_ne!(p("yes"), Some(FastIntent::ApproveAlways));
    }

    #[test]
    fn near_misses_go_to_the_model() {
        for said in [
            "",
            "don't approve",
            "approve the minos one",
            "yes do it",
            "please approve",
            "no always",
            "approvee",
            "option ten",
            "option 0",
            "option",
        ] {
            assert_eq!(p(said), None, "{said:?}");
        }
    }

    #[test]
    fn replies_to_a_waiting_hud() {
        let r = |s: &str| parse_reply(s, Locale::En);
        assert_eq!(r("Confirm."), Some(Reply::Confirm));
        assert_eq!(r("undo that"), Some(Reply::Cancel));
        for (said, i) in [("the first", 0), ("the first one", 0), ("number two", 1), ("2", 1), ("the 3rd one", 2), ("one", 0)] {
            assert_eq!(r(said), Some(Reply::Choose(i)), "{said}");
        }
        for said in ["yes", "approve", "yes always", "the tenth", "send it"] {
            assert_eq!(r(said), None, "{said}");
        }
    }

    #[test]
    fn options_are_chosen_by_effect() {
        let r = permission("r");
        assert_eq!(pick_option(&r, OptionBy::Effect(Effect::Allow)), OptionPick::One(0));
        assert_eq!(pick_option(&r, OptionBy::Effect(Effect::Deny)), OptionPick::One(1));
        assert_eq!(pick_option(&r, OptionBy::Effect(Effect::AllowAlways)), OptionPick::One(2));
    }

    #[test]
    fn option_n_uses_the_displayed_number() {
        let mut r = permission("r");
        r.options.push(option("weird", "?", Effect::Unknown));
        assert_eq!(pick_option(&r, OptionBy::Number(3)), OptionPick::One(2));
        assert_eq!(pick_option(&r, OptionBy::Number(4)), OptionPick::NoMatch, "unknown effect");
        assert_eq!(pick_option(&r, OptionBy::Number(5)), OptionPick::NoMatch, "no such option");
    }

    #[test]
    fn approve_on_a_question_without_an_allow_option_matches_nothing() {
        let mut r = permission("r");
        r.options = vec![option("a", "Red", Effect::Choose), option("b", "Blue", Effect::Choose)];
        assert_eq!(pick_option(&r, OptionBy::Effect(Effect::Allow)), OptionPick::NoMatch);
        assert_eq!(pick_option(&r, OptionBy::Number(2)), OptionPick::One(1));
    }

    #[test]
    fn two_options_with_one_effect_are_ambiguous() {
        let mut r = permission("r");
        r.options.push(option("allow2", "Yes, this session", Effect::Allow));
        assert_eq!(pick_option(&r, OptionBy::Effect(Effect::Allow)), OptionPick::Several);
    }

    #[test]
    fn unselectable_options_are_never_picked() {
        let mut r = permission("r");
        r.options[0].selectable = false;
        assert_eq!(pick_option(&r, OptionBy::Effect(Effect::Allow)), OptionPick::NoMatch);
        assert_eq!(pick_option(&r, OptionBy::Number(1)), OptionPick::NoMatch);
    }
}
