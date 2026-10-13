//! The presentation snapshot and the frozen inbox context (spec §4.3).
//! These are acd-voice's own types; acd-voice-client converts
//! acd-client's view into them.
use acd_protocol::{AgentKind, AgentRole, AgentState, Effect, RequestKind};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::text::Redacted;

/// Panel state the frontend publishes (spec §4.3).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presentation {
    /// Random per panel mount, so a reloaded webview starts afresh.
    pub epoch: String,
    /// Grows with every publish within an epoch.
    pub version: u64,
    pub selected: Option<String>,
    /// Card keys on screen, in display order.
    pub visible: Vec<String>,
    pub machine_filter: Option<String>,
    pub done_collapsed: bool,
}

impl Presentation {
    /// Whether this publish replaces `current`. IPC calls can arrive out
    /// of order, so within an epoch only a higher version wins.
    pub fn supersedes(&self, current: Option<&Presentation>) -> bool {
        current.is_none_or(|c| c.epoch != self.epoch || self.version > c.version)
    }
}

/// A machine connection's identity: handle instance and connection
/// generation (spec §4.3, §5.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnStamp {
    pub instance: u64,
    pub conn_gen: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Request,
    Done,
    Working,
    Unknown,
    Idle,
}

/// `Debug` of the agent-text types below shows lengths only (spec §3.7).
#[derive(Clone, PartialEq, Eq)]
pub struct OptionSnap {
    pub id: String,
    pub label: String,
    /// For `allow_always`, the persistent rule the HUD shows in full.
    pub description: Option<String>,
    pub effect: Effect,
    /// acd-client lists it in `selectable_options`.
    pub selectable: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RequestSnap {
    pub id: String,
    pub revision: u64,
    pub kind: RequestKind,
    pub title: String,
    pub detail: String,
    /// The first question's text, for a question.
    pub question: Option<String>,
    /// acd-client's `CardView::answerable`: online, capable, open, answerable.
    pub answerable: bool,
    /// Answerable and takes a typed answer.
    pub free_text: bool,
    /// Display order: "option N" is `options[N - 1]`.
    pub options: Vec<OptionSnap>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct CardSnap {
    pub key: String,
    pub machine_key: String,
    pub machine_name: String,
    /// The machine is online and ready (`CardView::live`).
    pub online: bool,
    /// `None` when the machine has no connection handle.
    pub conn: Option<ConnStamp>,
    pub agent_id: String,
    pub kind: AgentKind,
    pub title: String,
    pub project: Option<String>,
    pub state: AgentState,
    pub role: AgentRole,
    pub group: Group,
    pub can_reply: bool,
    pub pending: Option<RequestSnap>,
    /// An idle agent the panel hides ("Show idle" off). Voice can still
    /// send to it when named (D7).
    pub idle_hidden: bool,
}

impl fmt::Debug for OptionSnap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OptionSnap")
            .field("id", &self.id)
            .field("label", &Redacted(&self.label))
            .field("description", &Redacted::opt(&self.description))
            .field("effect", &self.effect)
            .field("selectable", &self.selectable)
            .finish()
    }
}

impl fmt::Debug for RequestSnap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestSnap")
            .field("id", &self.id)
            .field("revision", &self.revision)
            .field("kind", &self.kind)
            .field("title", &Redacted(&self.title))
            .field("detail", &Redacted(&self.detail))
            .field("question", &Redacted::opt(&self.question))
            .field("answerable", &self.answerable)
            .field("free_text", &self.free_text)
            .field("options", &self.options)
            .finish()
    }
}

impl fmt::Debug for CardSnap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CardSnap")
            .field("key", &self.key)
            .field("machine_key", &self.machine_key)
            .field("machine_name", &Redacted(&self.machine_name))
            .field("online", &self.online)
            .field("conn", &self.conn)
            .field("agent_id", &self.agent_id)
            .field("kind", &self.kind)
            .field("title", &Redacted(&self.title))
            .field("project", &Redacted::opt(&self.project))
            .field("state", &self.state)
            .field("role", &self.role)
            .field("group", &self.group)
            .field("can_reply", &self.can_reply)
            .field("pending", &self.pending)
            .field("idle_hidden", &self.idle_hidden)
            .finish()
    }
}

/// A machine the inbox knows, with or without cards. Alerts baseline a
/// machine the first time it is seen online (plan V14).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineSnap {
    pub key: String,
    /// Connected and its snapshot applied (`MachineStatus::Online`).
    pub online: bool,
}

/// Everything voice reads, frozen when recording starts (spec §4.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub presentation: Presentation,
    /// Whether the inbox panel was open when the snapshot froze. A
    /// selection counts only while the panel shows it (D6).
    pub panel_open: bool,
    pub show_subagents: bool,
    pub cards: Vec<CardSnap>,
    /// Every machine, whether or not it has cards. Empty in fixtures that do
    /// not care: alerts then treat every machine as already known.
    pub machines: Vec<MachineSnap>,
}

/// Why a card can never be a target (spec §5.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hidden {
    Subagent,
    /// Neither on screen nor hidden by the machine filter (for example a
    /// card that arrived after the last publish).
    NotShown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Visible,
    /// Hidden only by the machine filter: a target only when named.
    FilterHidden,
    /// In a collapsed Done section, or an idle agent the panel hides: the
    /// target of a named `send` only, always confirmed (D7).
    SendOnly,
    Never(Hidden),
}

impl Snapshot {
    pub fn card(&self, key: &str) -> Option<&CardSnap> {
        self.cards.iter().find(|c| c.key == key)
    }

    pub fn visibility(&self, card: &CardSnap) -> Visibility {
        if card.role == AgentRole::Subagent && !self.show_subagents {
            return Visibility::Never(Hidden::Subagent);
        }
        // Idle agents share the Done section (panel.ts `sections`). D7: a
        // collapsed Done card, even one with a countdown on screen, and a
        // hidden idle agent take only a named send.
        if card.idle_hidden
            || (matches!(card.group, Group::Done | Group::Idle) && self.presentation.done_collapsed)
        {
            return Visibility::SendOnly;
        }
        if self.presentation.visible.contains(&card.key) {
            return Visibility::Visible;
        }
        match &self.presentation.machine_filter {
            Some(machine) if *machine != card.machine_key => Visibility::FilterHidden,
            _ => Visibility::Never(Hidden::NotShown),
        }
    }

    /// Cards on screen, in display order (D8).
    pub fn visible_cards(&self) -> Vec<&CardSnap> {
        self.presentation
            .visible
            .iter()
            .filter_map(|k| self.card(k))
            .filter(|c| self.visibility(c) == Visibility::Visible)
            .collect()
    }

    /// Visible cards, then cards hidden only by the machine filter.
    pub fn targetable(&self) -> Vec<&CardSnap> {
        let mut out = self.visible_cards();
        out.extend(
            self.cards
                .iter()
                .filter(|c| self.visibility(c) == Visibility::FilterHidden),
        );
        out
    }

    /// Targetable cards, then send-only cards: what a name can reach (D7).
    pub fn nameable(&self) -> Vec<&CardSnap> {
        let mut out = self.targetable();
        out.extend(self.cards.iter().filter(|c| self.visibility(c) == Visibility::SendOnly));
        out
    }

    /// The selected card, if the panel is open and shows it (D6).
    pub fn selected(&self) -> Option<&CardSnap> {
        if !self.panel_open {
            return None;
        }
        let card = self.card(self.presentation.selected.as_deref()?)?;
        (self.visibility(card) == Visibility::Visible).then_some(card)
    }

    /// Visible cards that need the user (spec §5.2 rule 3).
    pub fn pending_cards(&self) -> Vec<&CardSnap> {
        self.visible_cards()
            .into_iter()
            .filter(|c| c.group == Group::Request)
            .collect()
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod fixtures {
    //! Builders for tests here and in acd-voice-client.
    use super::*;

    pub fn option(id: &str, label: &str, effect: Effect) -> OptionSnap {
        OptionSnap {
            id: id.into(),
            label: label.into(),
            description: (effect == Effect::AllowAlways).then(|| "Bash(npm test:*)".to_owned()),
            effect,
            selectable: effect != Effect::Unknown,
        }
    }

    /// An open, answerable permission request: Yes / No / Yes, always.
    pub fn permission(id: &str) -> RequestSnap {
        RequestSnap {
            id: id.into(),
            revision: 1,
            kind: RequestKind::Permission,
            title: "Bash".into(),
            detail: "npm test".into(),
            question: None,
            answerable: true,
            free_text: false,
            options: vec![
                option("allow", "Yes", Effect::Allow),
                option("deny", "No", Effect::Deny),
                option("always", "Yes, always", Effect::AllowAlways),
            ],
        }
    }

    /// A needs-you Claude card `machine/agent` with request `r-<agent>`.
    /// Its title is the agent id, which is too short to name it.
    pub fn pending(machine: &str, agent: &str) -> CardSnap {
        CardSnap {
            key: format!("{machine}/{agent}"),
            machine_key: machine.into(),
            machine_name: machine.into(),
            online: true,
            conn: Some(ConnStamp { instance: 1, conn_gen: 1 }),
            agent_id: agent.into(),
            kind: AgentKind::Claude,
            title: agent.into(),
            project: None,
            state: AgentState::NeedsYou,
            role: AgentRole::Main,
            group: Group::Request,
            can_reply: false,
            pending: Some(permission(&format!("r-{agent}"))),
            idle_hidden: false,
        }
    }

    /// A Working Claude card that takes a reply. Not an idle card: idle
    /// agents sit in the Done section, which the panel collapses by default.
    pub fn working(machine: &str, agent: &str) -> CardSnap {
        CardSnap {
            state: AgentState::Working,
            group: Group::Working,
            can_reply: true,
            pending: None,
            ..pending(machine, agent)
        }
    }

    /// Every card visible in the given order; panel open; no selection.
    pub fn snapshot(cards: Vec<CardSnap>) -> Snapshot {
        Snapshot {
            presentation: Presentation {
                epoch: "e".into(),
                version: 1,
                selected: None,
                visible: cards.iter().map(|c| c.key.clone()).collect(),
                machine_filter: None,
                done_collapsed: false,
            },
            panel_open: true,
            show_subagents: false,
            cards,
            machines: Vec::new(),
        }
    }

    pub fn select(mut s: Snapshot, key: &str) -> Snapshot {
        s.presentation.selected = Some(key.into());
        s
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn subagents_are_never_targets_while_hidden() {
        let mut sub = pending("m", "s");
        sub.role = AgentRole::Subagent;
        let mut s = snapshot(vec![sub.clone()]);
        assert_eq!(s.visibility(&sub), Visibility::Never(Hidden::Subagent));
        s.show_subagents = true;
        assert_eq!(s.visibility(&sub), Visibility::Visible);
    }

    #[test]
    fn collapsed_done_and_hidden_idle_cards_are_send_only() {
        let mut done = working("m", "d");
        done.group = Group::Done;
        let mut idle = working("m", "i");
        idle.group = Group::Idle;
        idle.idle_hidden = true;
        let mut s = snapshot(vec![done.clone()]);
        s.cards.push(idle.clone());
        s.presentation.done_collapsed = true;
        assert_eq!(s.visibility(&done), Visibility::SendOnly);
        assert_eq!(s.visibility(&idle), Visibility::SendOnly);
        assert!(s.targetable().is_empty());
        assert_eq!(s.nameable().len(), 2);
        s.presentation.done_collapsed = false;
        assert_eq!(s.visibility(&done), Visibility::Visible);
        assert_eq!(s.visibility(&idle), Visibility::SendOnly, "still hidden by the idle setting");
    }

    #[test]
    fn the_machine_filter_hides_but_does_not_forbid() {
        let (a, b) = (pending("minos", "a"), pending("studio", "b"));
        let mut s = snapshot(vec![a.clone(), b.clone()]);
        s.presentation.machine_filter = Some("studio".into());
        s.presentation.visible = vec![b.key.clone()];
        assert_eq!(s.visibility(&a), Visibility::FilterHidden);
        assert_eq!(s.visibility(&b), Visibility::Visible);
        let keys: Vec<_> = s.targetable().iter().map(|c| c.key.clone()).collect();
        assert_eq!(keys, ["studio/b", "minos/a"]);
    }

    #[test]
    fn a_card_missing_from_an_unfiltered_publish_is_not_shown() {
        let a = pending("m", "a");
        let mut s = snapshot(vec![a.clone()]);
        s.presentation.visible.clear();
        assert_eq!(s.visibility(&a), Visibility::Never(Hidden::NotShown));
    }

    #[test]
    fn visible_order_is_the_published_order_not_the_card_order() {
        let (a, b) = (pending("m", "a"), pending("m", "b"));
        let mut s = snapshot(vec![a, b]);
        s.presentation.visible = vec!["m/b".into(), "m/a".into()];
        let keys: Vec<_> = s.visible_cards().iter().map(|c| c.key.clone()).collect();
        assert_eq!(keys, ["m/b", "m/a"]);
    }

    #[test]
    fn a_selection_counts_only_while_the_panel_shows_it() {
        let s = select(snapshot(vec![pending("m", "a")]), "m/a");
        assert_eq!(s.selected().map(|c| c.key.as_str()), Some("m/a"));
        let closed = Snapshot { panel_open: false, ..s.clone() };
        assert!(closed.selected().is_none());
        let stale = select(s, "m/gone");
        assert!(stale.selected().is_none());
    }

    #[test]
    fn presentations_supersede_by_epoch_then_version() {
        let p = |epoch: &str, version| Presentation { epoch: epoch.into(), version, ..Default::default() };
        assert!(p("e", 1).supersedes(None));
        assert!(p("e", 2).supersedes(Some(&p("e", 1))));
        assert!(!p("e", 1).supersedes(Some(&p("e", 2))));
        assert!(!p("e", 2).supersedes(Some(&p("e", 2))));
        assert!(p("f", 1).supersedes(Some(&p("e", 9))));
    }
}
