//! Target resolution with provenance (spec §5.2). Rust resolves the
//! target; a model's pick is only a proposal checked against it.
use acd_protocol::AgentKind;
use serde::Serialize;

use crate::fast::{normalize, ordinal_index};
use crate::refusal::Refusal;
use crate::snapshot::{CardSnap, Hidden, Snapshot, Visibility};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Named,
    Selected,
    OnlyPending,
    Inferred,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Answer,
    AnswerText,
    Send,
    Read,
    Select,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub card: String,
    pub provenance: Provenance,
    pub off_screen: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    One(Resolved),
    WhichOne(Vec<String>),
    Refused(Refusal),
}

/// Whether `card` can take a step for `purpose`, visibility aside.
pub fn fits(card: &CardSnap, purpose: Purpose) -> Result<(), Refusal> {
    match purpose {
        Purpose::Answer | Purpose::AnswerText => {
            let Some(req) = &card.pending else {
                return Err(Refusal::NothingToAnswer);
            };
            if !card.online {
                return Err(Refusal::Offline);
            }
            if !req.answerable || (purpose == Purpose::AnswerText && !req.free_text) {
                return Err(Refusal::ReadOnly);
            }
            Ok(())
        }
        Purpose::Send if !card.online => Err(Refusal::Offline),
        Purpose::Send if !card.can_reply => Err(Refusal::CannotReply),
        Purpose::Send | Purpose::Select => Ok(()),
        Purpose::Read => card.pending.as_ref().map(|_| ()).ok_or(Refusal::NothingToRead),
    }
}

/// Words that never count as title words.
const STOP: &[&str] = &[
    "about", "agent", "answer", "approve", "claude", "codex", "deny", "from", "have", "into",
    "option", "please", "read", "request", "select", "send", "show", "tell", "that", "them",
    "then", "there", "this", "what", "with", "your",
];

/// The transcript as matched for names, with the payload removed, so
/// that "tell studio to deploy minos" does not name minos.
pub fn said_for_names(transcript: &str, payload: Option<&str>) -> String {
    let said = normalize(transcript);
    let Some(payload) = payload.map(normalize).filter(|p| !p.is_empty()) else {
        return said;
    };
    match said.rfind(&payload) {
        Some(at) => normalize(&format!("{} {}", &said[..at], &said[at + payload.len()..])),
        None => said,
    }
}

fn words(said: &str) -> Vec<&str> {
    said.split(' ').filter(|w| !w.is_empty()).collect()
}

fn has_phrase(said: &str, phrase: &str) -> bool {
    let phrase = normalize(phrase);
    !phrase.is_empty() && format!(" {said} ").contains(&format!(" {phrase} "))
}

fn ordinal(said: &str) -> Option<usize> {
    words(said).into_iter().find_map(ordinal_index)
}

fn kinds(said: &str) -> Vec<AgentKind> {
    let mut out = Vec::new();
    for w in words(said) {
        match w {
            "claude" => out.push(AgentKind::Claude),
            "codex" => out.push(AgentKind::Codex),
            _ => {}
        }
    }
    out
}

/// Named by its project, or by two significant words of its title.
fn named_by_label(card: &CardSnap, said: &str) -> bool {
    if card
        .project
        .as_deref()
        .is_some_and(|p| normalize(p).chars().count() >= 3 && has_phrase(said, p))
    {
        return true;
    }
    let said_words = words(said);
    let title = normalize(&card.title);
    let mut hits: Vec<&str> = title
        .split(' ')
        .filter(|w| w.chars().count() >= 4 && !STOP.contains(w) && said_words.contains(w))
        .collect();
    hits.sort_unstable();
    hits.dedup();
    hits.len() >= 2
}

/// The cards the transcript names (rule 1), among the cards a name can
/// reach (`Snapshot::nameable`, which includes send-only cards; D7).
pub fn named(snapshot: &Snapshot, said: &str) -> Option<Vec<String>> {
    let pool = snapshot.nameable();
    let mut machines: Vec<&str> = snapshot
        .cards
        .iter()
        .filter(|c| has_phrase(said, &c.machine_name))
        .map(|c| c.machine_key.as_str())
        .collect();
    machines.sort_unstable();
    machines.dedup();
    let kinds = kinds(said);
    let constrained = |c: &CardSnap| {
        (machines.is_empty() || machines.contains(&c.machine_key.as_str()))
            && (kinds.is_empty() || kinds.contains(&c.kind))
    };
    if let Some(n) = ordinal(said) {
        let ranked: Vec<&CardSnap> = snapshot
            .visible_cards()
            .into_iter()
            .filter(|c| constrained(c))
            .collect();
        return Some(ranked.get(n).map(|c| vec![c.key.clone()]).unwrap_or_default());
    }
    let by_label: Vec<&str> = pool
        .iter()
        .filter(|c| named_by_label(c, said))
        .map(|c| c.key.as_str())
        .collect();
    if machines.is_empty() && kinds.is_empty() {
        return (!by_label.is_empty()).then(|| by_label.iter().map(|k| (*k).to_owned()).collect());
    }
    let mut hits: Vec<&str> = pool
        .iter()
        .filter(|c| constrained(c))
        .map(|c| c.key.as_str())
        .collect();
    if hits.len() > 1 {
        let narrowed: Vec<&str> = hits.iter().copied().filter(|k| by_label.contains(k)).collect();
        if !narrowed.is_empty() {
            hits = narrowed;
        }
    }
    Some(hits.into_iter().map(str::to_owned).collect())
}

pub fn resolve(snapshot: &Snapshot, said: &str, purpose: Purpose, pick: Option<&str>) -> Resolution {
    if let Some(named) = named(snapshot, said) {
        return resolve_named(snapshot, &named, purpose, pick);
    }
    if let Some(sel) = snapshot.selected() {
        return agree(snapshot, sel, purpose, pick, Provenance::Selected);
    }
    if matches!(purpose, Purpose::Answer | Purpose::AnswerText | Purpose::Read) {
        let pending = snapshot.pending_cards();
        if let [only] = pending.as_slice() {
            return agree(snapshot, only, purpose, pick, Provenance::OnlyPending);
        }
        if pick.is_none() {
            let fitting: Vec<String> = pending
                .iter()
                .filter(|c| fits(c, purpose).is_ok())
                .map(|c| c.key.clone())
                .collect();
            if !fitting.is_empty() {
                return Resolution::WhichOne(fitting);
            }
            return Resolution::Refused(match pending.first() {
                Some(c) => fits(c, purpose).err().unwrap_or(Refusal::NothingToAnswer),
                None if purpose == Purpose::Read => Refusal::NothingToRead,
                None => Refusal::NothingToAnswer,
            });
        }
    }
    match pick {
        Some(key) => inferred(snapshot, key, purpose),
        None => Resolution::Refused(Refusal::NoTarget),
    }
}

fn resolve_named(snapshot: &Snapshot, named: &[String], purpose: Purpose, pick: Option<&str>) -> Resolution {
    if named.is_empty() {
        return Resolution::Refused(Refusal::NoSuchTarget);
    }
    if let Some(p) = pick
        && !named.iter().any(|k| k == p)
    {
        return Resolution::Refused(Refusal::TargetMismatch);
    }
    let cards: Vec<&CardSnap> = named.iter().filter_map(|k| snapshot.card(k)).collect();
    let fitting: Vec<String> = cards
        .iter()
        .filter(|c| reach(snapshot, c, purpose).is_ok())
        .map(|c| c.key.clone())
        .collect();
    match (cards.as_slice(), fitting.is_empty()) {
        ([one], false) => one_of(snapshot, &one.key, Provenance::Named),
        (_, true) => Resolution::Refused(
            cards
                .first()
                .and_then(|c| reach(snapshot, c, purpose).err())
                .unwrap_or(Refusal::NoSuchTarget),
        ),
        _ => Resolution::WhichOne(fitting),
    }
}

/// Visibility, then `fits`: a send-only card takes only a send (D7).
fn reach(snapshot: &Snapshot, card: &CardSnap, purpose: Purpose) -> Result<(), Refusal> {
    if snapshot.visibility(card) == Visibility::SendOnly && purpose != Purpose::Send {
        return Err(Refusal::OutOfView);
    }
    fits(card, purpose)
}

/// Rules 2 and 3: Rust's card decides, and a model pick must agree.
fn agree(snapshot: &Snapshot, card: &CardSnap, purpose: Purpose, pick: Option<&str>, provenance: Provenance) -> Resolution {
    if let Err(r) = fits(card, purpose) {
        return Resolution::Refused(r);
    }
    if pick.is_some_and(|p| p != card.key) {
        return Resolution::Refused(Refusal::TargetMismatch);
    }
    one_of(snapshot, &card.key, provenance)
}

/// Rule 4: the model's pick, if the user can see it.
fn inferred(snapshot: &Snapshot, key: &str, purpose: Purpose) -> Resolution {
    let Some(card) = snapshot.card(key) else {
        return Resolution::Refused(Refusal::NoSuchTarget);
    };
    let hidden = match snapshot.visibility(card) {
        Visibility::Visible => None,
        Visibility::FilterHidden => Some(Refusal::FilterHiddenNotNamed),
        // D7: a send-only card is a target only when named.
        Visibility::SendOnly => Some(Refusal::OutOfView),
        Visibility::Never(Hidden::Subagent) => Some(Refusal::SubagentHidden),
        Visibility::Never(Hidden::NotShown) => Some(Refusal::NoSuchTarget),
    };
    if let Some(r) = hidden {
        return Resolution::Refused(r);
    }
    if let Err(r) = fits(card, purpose) {
        return Resolution::Refused(r);
    }
    one_of(snapshot, key, Provenance::Inferred)
}

fn one_of(snapshot: &Snapshot, key: &str, provenance: Provenance) -> Resolution {
    // Hidden by the filter, in collapsed Done, or a hidden idle agent:
    // always confirmed, and the HUD stresses the target (spec §5.2, D7).
    let off_screen = snapshot
        .card(key)
        .is_some_and(|c| snapshot.visibility(c) != Visibility::Visible);
    Resolution::One(Resolved {
        card: key.to_owned(),
        provenance,
        off_screen,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Group;
    use crate::snapshot::fixtures::*;

    fn one(r: Resolution) -> Resolved {
        match r {
            Resolution::One(x) => x,
            other => panic!("expected one target, got {other:?}"),
        }
    }

    fn keys(k: &[&str]) -> Vec<String> {
        k.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn machine_and_kind_name_one_card() {
        let mut codex = working("minos", "b");
        codex.kind = AgentKind::Codex;
        let s = snapshot(vec![working("minos", "a"), codex, working("studio", "c")]);
        let said = said_for_names("tell the minos claude to run the tests", Some("run the tests"));
        let r = one(resolve(&s, &said, Purpose::Send, None));
        assert_eq!((r.card.as_str(), r.provenance), ("minos/a", Provenance::Named));
    }

    #[test]
    fn two_cards_with_the_same_machine_and_kind_ask_which_one() {
        let s = snapshot(vec![working("minos", "a"), working("minos", "b")]);
        let said = said_for_names("tell the minos claude to run the tests", Some("run the tests"));
        assert_eq!(resolve(&s, &said, Purpose::Send, None), Resolution::WhichOne(keys(&["minos/a", "minos/b"])));
        assert_eq!(
            resolve(&s, &said, Purpose::Send, Some("minos/a")),
            Resolution::WhichOne(keys(&["minos/a", "minos/b"])),
            "the model's pick does not settle it"
        );
    }

    #[test]
    fn a_model_pick_outside_the_named_cards_is_refused() {
        let s = snapshot(vec![working("minos", "a"), working("studio", "b")]);
        let said = said_for_names("tell studio to continue", Some("continue"));
        assert_eq!(resolve(&s, &said, Purpose::Send, Some("minos/a")), Resolution::Refused(Refusal::TargetMismatch));
    }

    #[test]
    fn the_payload_does_not_name_a_target() {
        let s = snapshot(vec![working("minos", "a"), working("studio", "b")]);
        let said = said_for_names("tell studio to deploy minos", Some("deploy minos"));
        assert_eq!(one(resolve(&s, &said, Purpose::Send, None)).card, "studio/b");
    }

    #[test]
    fn titles_and_projects_name_cards() {
        let mut a = working("m", "a");
        a.title = "Fix the flaky build".into();
        let mut b = working("m", "b");
        b.project = Some("omarchy".into());
        let s = snapshot(vec![a, b]);
        assert_eq!(one(resolve(&s, "show the flaky build one", Purpose::Select, None)).card, "m/a");
        assert_eq!(one(resolve(&s, "show the omarchy one", Purpose::Select, None)).card, "m/b");
    }

    #[test]
    fn ordinals_use_the_published_order() {
        let mut s = snapshot(vec![pending("m", "a"), pending("m", "b")]);
        s.presentation.visible = keys(&["m/b", "m/a"]);
        let r = one(resolve(&s, "approve the first one", Purpose::Answer, None));
        assert_eq!((r.card.as_str(), r.provenance), ("m/b", Provenance::Named));
        assert_eq!(resolve(&s, "approve the third one", Purpose::Answer, None), Resolution::Refused(Refusal::NoSuchTarget));
    }

    #[test]
    fn naming_a_machine_with_no_eligible_card_is_refused() {
        let mut s = snapshot(vec![pending("studio", "a")]);
        let mut sub = pending("minos", "s");
        sub.role = acd_protocol::AgentRole::Subagent;
        s.cards.push(sub);
        s.presentation.visible.push("minos/s".into());
        assert_eq!(resolve(&s, "approve the minos one", Purpose::Answer, None), Resolution::Refused(Refusal::NoSuchTarget));
    }

    #[test]
    fn nothing_named_uses_the_selection() {
        let s = select(snapshot(vec![pending("m", "a"), pending("m", "b")]), "m/b");
        let r = one(resolve(&s, "", Purpose::Answer, None));
        assert_eq!((r.card.as_str(), r.provenance), ("m/b", Provenance::Selected));
        assert_eq!(resolve(&s, "", Purpose::Answer, Some("m/a")), Resolution::Refused(Refusal::TargetMismatch));
    }

    #[test]
    fn a_closed_panel_has_no_selection() {
        let mut s = select(snapshot(vec![pending("m", "a"), pending("m", "b")]), "m/b");
        s.panel_open = false;
        assert_eq!(resolve(&s, "", Purpose::Answer, None), Resolution::WhichOne(keys(&["m/a", "m/b"])));
    }

    #[test]
    fn a_stale_selection_is_not_a_selection() {
        let s = select(snapshot(vec![pending("m", "a")]), "m/gone");
        let r = one(resolve(&s, "", Purpose::Answer, None));
        assert_eq!((r.card.as_str(), r.provenance), ("m/a", Provenance::OnlyPending));
    }

    #[test]
    fn a_read_only_selection_is_refused_not_retargeted() {
        let mut ro = pending("m", "a");
        ro.pending.as_mut().unwrap().answerable = false;
        let s = select(snapshot(vec![ro, pending("m", "b")]), "m/a");
        assert_eq!(resolve(&s, "", Purpose::Answer, None), Resolution::Refused(Refusal::ReadOnly));
    }

    #[test]
    fn only_pending_needs_exactly_one_visible_request() {
        let s = snapshot(vec![pending("m", "a"), working("m", "w")]);
        let r = one(resolve(&s, "", Purpose::Answer, None));
        assert_eq!((r.card.as_str(), r.provenance), ("m/a", Provenance::OnlyPending));
        let none = snapshot(vec![working("m", "w")]);
        assert_eq!(resolve(&none, "", Purpose::Answer, None), Resolution::Refused(Refusal::NothingToAnswer));
    }

    #[test]
    fn an_unnamed_model_pick_is_inferred_only_when_visible() {
        let mut s = snapshot(vec![working("minos", "a"), working("studio", "b")]);
        assert_eq!(one(resolve(&s, "keep going", Purpose::Send, Some("studio/b"))).provenance, Provenance::Inferred);
        s.presentation.machine_filter = Some("studio".into());
        s.presentation.visible = keys(&["studio/b"]);
        assert_eq!(resolve(&s, "keep going", Purpose::Send, Some("minos/a")), Resolution::Refused(Refusal::FilterHiddenNotNamed));
    }

    #[test]
    fn a_filter_hidden_card_is_a_target_when_named() {
        let mut s = snapshot(vec![working("minos", "a"), working("studio", "b")]);
        s.presentation.machine_filter = Some("studio".into());
        s.presentation.visible = keys(&["studio/b"]);
        let r = one(resolve(&s, "tell minos to", Purpose::Send, None));
        assert_eq!((r.card.as_str(), r.off_screen), ("minos/a", true));
    }

    #[test]
    fn a_collapsed_done_card_takes_only_a_named_send() {
        let mut d = working("minos", "d");
        d.group = Group::Done;
        let mut s = snapshot(vec![d]);
        s.presentation.done_collapsed = true;
        let r = one(resolve(&s, "tell minos to continue", Purpose::Send, None));
        assert_eq!((r.card.as_str(), r.provenance, r.off_screen), ("minos/d", Provenance::Named, true));
        assert_eq!(resolve(&s, "keep going", Purpose::Send, Some("minos/d")), Resolution::Refused(Refusal::OutOfView), "unnamed");
        assert_eq!(resolve(&s, "read the minos one", Purpose::Read, None), Resolution::Refused(Refusal::OutOfView), "not an answer or a read");
    }

    #[test]
    fn a_hidden_idle_agent_takes_only_a_named_send() {
        // "Show idle" is off by default, so idle agents are not on screen
        // at all; D7 lets a named send reach them.
        let mut idle = working("minos", "a");
        idle.state = acd_protocol::AgentState::Idle;
        idle.group = Group::Idle;
        idle.idle_hidden = true;
        let mut s = snapshot(vec![idle]);
        s.presentation.visible.clear();
        let r = one(resolve(&s, "tell minos to continue", Purpose::Send, None));
        assert_eq!((r.card.as_str(), r.off_screen), ("minos/a", true));
        assert_eq!(resolve(&s, "keep going", Purpose::Send, Some("minos/a")), Resolution::Refused(Refusal::OutOfView));
    }

    #[test]
    fn an_idle_agent_is_not_a_target_while_done_is_collapsed() {
        // D7: unnamed, a send-only card is never a target, whether it was
        // reached by the model's pick or by the only-pending rule.
        let mut d = pending("minos", "d");
        d.group = Group::Done;
        let mut idle = working("studio", "i");
        idle.state = acd_protocol::AgentState::Idle;
        idle.group = Group::Idle;
        idle.idle_hidden = true;
        let mut s = snapshot(vec![d, idle]);
        s.presentation.done_collapsed = true;
        s.presentation.visible.clear();
        for k in ["minos/d", "studio/i"] {
            assert_eq!(resolve(&s, "keep going", Purpose::Send, Some(k)), Resolution::Refused(Refusal::OutOfView), "{k} unnamed");
        }
        assert_eq!(resolve(&s, "approve it", Purpose::Answer, None), Resolution::Refused(Refusal::NothingToAnswer));
        // Named: a send is allowed and confirmed; other steps are refused.
        let r = one(resolve(&s, "tell studio to continue", Purpose::Send, None));
        assert!(r.off_screen);
        assert_eq!(resolve(&s, "select the minos one", Purpose::Select, None), Resolution::Refused(Refusal::OutOfView));
    }

    #[test]
    fn purposes_check_the_card() {
        let mut off = pending("m", "a");
        off.online = false;
        assert_eq!(fits(&off, Purpose::Answer), Err(Refusal::Offline));
        assert_eq!(fits(&pending("m", "a"), Purpose::AnswerText), Err(Refusal::ReadOnly));
        assert_eq!(fits(&pending("m", "a"), Purpose::Send), Err(Refusal::CannotReply));
        assert_eq!(fits(&working("m", "w"), Purpose::Read), Err(Refusal::NothingToRead));
        assert_eq!(fits(&working("m", "w"), Purpose::Send), Ok(()));
    }
}
