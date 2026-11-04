//! What the Android screen shows, decided in Rust. The Java activity only
//! draws these rows and sends back the ids it was given, so the rules for
//! "answerable whole, or read-only" live here and are tested on the host.
//!
//! The rules are those of `app/src/components/Card.svelte`: options appear
//! only on a live, answerable card; an option the client cannot send is
//! disabled; every other request shows where to answer instead ("Answer in
//! terminal"); a request is shown with its whole detail text.

use acd_client::{
    conn::MachineStatus,
    view::{CardGroup, CardView, InboxView, NoticeTone},
};
use acd_protocol::{Effect, RequestKind, Source};
use serde::Serialize;

#[derive(Serialize, Debug, PartialEq)]
pub struct ScreenModel {
    pub machines: Vec<MachineRow>,
    pub cards: Vec<CardRow>,
    pub needs_you: u32,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct MachineRow {
    pub key: String,
    pub name: String,
    /// "online", "connecting", "offline", or a problem that needs the user.
    pub status: String,
    pub problem: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct CardRow {
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub last_message: Option<String>,
    /// The request text, whole, one entry per line.
    pub detail: Vec<String>,
    pub offline_note: Option<String>,
    /// Present when the card can be answered here. Empty otherwise.
    pub request_id: Option<String>,
    pub revision: u64,
    pub options: Vec<OptionRow>,
    pub free_text: bool,
    /// Where to answer when it cannot be answered here.
    pub read_only: Option<String>,
    pub can_mark_seen: bool,
    pub busy: bool,
    pub notice: Option<String>,
    pub notice_warning: bool,
    pub undo: Option<UndoRow>,
    pub group: &'static str,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct OptionRow {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    /// False when this client cannot send the option ("Not supported here").
    pub enabled: bool,
    /// Needs a second tap.
    pub confirm: bool,
    pub deny: bool,
}

#[derive(Serialize, Debug, PartialEq)]
pub struct UndoRow {
    pub id: String,
    pub label: String,
    pub detail: Option<String>,
}

pub fn screen(view: &InboxView) -> ScreenModel {
    ScreenModel {
        machines: view.machines.iter().filter(|m| m.duplicate_of.is_none()).map(machine_row).collect(),
        cards: view.cards.iter().map(card_row).collect(),
        needs_you: view.tray.needs_you,
    }
}

fn machine_row(machine: &acd_client::view::MachineView) -> MachineRow {
    let (status, problem) = match &machine.status {
        MachineStatus::Connecting | MachineStatus::Syncing => ("connecting", None),
        MachineStatus::Online => ("online", None),
        MachineStatus::Offline { error, .. } => ("offline", Some(error.clone())),
        MachineStatus::AuthFailed { message }
        | MachineStatus::IdentityChanged { message }
        | MachineStatus::Incompatible { message }
        | MachineStatus::LocalUnavailable { message } => ("attention", Some(message.clone())),
    };
    MachineRow {
        key: machine.key.clone(),
        name: machine.name.clone(),
        status: status.into(),
        problem,
    }
}

fn card_row(card: &CardView) -> CardRow {
    let agent = &card.agent;
    let pending = agent.pending.as_ref();
    let external = agent.source == Source::External;
    let source = match agent.source {
        Source::Herdr => "herdr".to_owned(),
        Source::T3 => "T3".to_owned(),
        Source::Hooks => "hooks".to_owned(),
        Source::External => agent.title.clone(),
        Source::Unknown => "unknown".to_owned(),
    };
    let place = agent
        .project
        .clone()
        .or_else(|| agent.cwd.as_deref().and_then(|c| c.split('/').rfind(|p| !p.is_empty())).map(str::to_owned));
    let mut subtitle = vec![card.machine_name.clone(), source];
    subtitle.extend(place);
    let title = match (external, pending) {
        (true, Some(p)) => p.title.clone(),
        _ => agent.title.clone(),
    };
    let read_only_label = match agent.source {
        Source::T3 => "Answer in T3".to_owned(),
        Source::External => format!("Answer in {}", agent.title),
        _ => "Answer in terminal".to_owned(),
    };
    let can_answer_here = card.live && card.answerable && pending.is_some();
    let options = match pending {
        Some(p) if can_answer_here => p
            .options
            .iter()
            .map(|o| OptionRow {
                id: o.id.clone(),
                label: o.label.clone(),
                description: o.description.clone(),
                enabled: card.selectable_options.contains(&o.id) && o.effect != Effect::Unknown,
                confirm: o.confirm,
                deny: o.effect == Effect::Deny,
            })
            .collect(),
        _ => Vec::new(),
    };
    // A needs-you card that is live, not busy and not answerable here says
    // where to answer. Other groups have nothing to answer.
    let read_only = (card.live
        && card.group == CardGroup::Request
        && !can_answer_here
        && !card.busy
        && pending.map_or(!external, |p| p.status != acd_protocol::RequestStatus::Answering))
    .then_some(read_only_label);
    let mut detail = Vec::new();
    if let Some(p) = pending {
        detail.extend(p.detail.split('\n').map(str::to_owned));
        if p.kind == RequestKind::Question
            && let Some(q) = p.questions.as_ref().and_then(|qs| qs.first())
            && !q.text.trim().is_empty()
            && q.text.trim() != p.detail.trim()
        {
            detail.push(q.text.clone());
        }
        if p.kind == RequestKind::BlockedUnparsed
            && let Some(raw) = &p.raw_screen
        {
            detail.extend(raw.split('\n').map(str::to_owned));
        }
    }
    CardRow {
        key: card.key.clone(),
        title,
        subtitle: subtitle.join(" · "),
        last_message: agent.last_message.clone(),
        detail,
        offline_note: (!card.live).then(|| {
            if card.offline_since_ms.is_some() { "offline" } else { "connecting…" }.to_owned()
        }),
        request_id: pending.filter(|_| can_answer_here).map(|p| p.id.clone()),
        revision: pending.map_or(0, |p| p.revision),
        options,
        free_text: can_answer_here && card.free_text_answerable,
        read_only,
        can_mark_seen: card.live && card.can_mark_seen,
        busy: card.busy,
        notice: card.notice.as_ref().map(|n| n.text.clone()),
        notice_warning: card.notice.as_ref().is_some_and(|n| n.tone == NoticeTone::Warning),
        undo: card.action.as_ref().map(|a| UndoRow {
            id: a.id.clone(),
            label: a.label.clone(),
            detail: a.detail.clone(),
        }),
        group: match card.group {
            CardGroup::Request => "request",
            CardGroup::Done => "done",
            CardGroup::Working => "working",
            CardGroup::Unknown => "unknown",
            CardGroup::Idle => "idle",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_client::{
        conn::MachineState,
        session::Session,
        settings::{EndpointConfig, MachineEntry},
        view::{MachineInput, ViewInput, build_view},
    };
    use acd_protocol::{Agent, Machine, ServerMessage, SourceStatus, Sources};
    use serde_json::{Value, json};
    use std::collections::{HashMap, HashSet};

    fn agent(id: &str, source: &str, state: &str, pending: Value) -> Agent {
        serde_json::from_value(json!({
            "id": id, "machine_id": "m", "source": source, "kind": "claude", "title": id,
            "cwd": "/home/dev/proj", "project": null, "state": state,
            "state_since": "2026-11-04T00:00:00Z", "failed": false, "last_message": null,
            "pending": pending, "can_send": true
        }))
        .unwrap()
    }

    fn request(id: &str, kind: &str, answer_path: &str, status: &str, options: Value) -> Value {
        json!({
            "id": id, "revision": 3, "agent_id": "a", "kind": kind, "status": status,
            "answer_path": answer_path, "title": "Bash", "detail": "$ ls\n\"cwd\": \"/x\"",
            "options": options, "questions": null, "free_text": false, "raw_screen": null,
            "opened_at": "2026-11-04T00:00:00Z"
        })
    }

    fn yes_no() -> Value {
        json!([
            {"id": "allow", "label": "Yes", "description": null, "effect": "allow"},
            {"id": "deny", "label": "No", "description": null, "effect": "deny", "confirm": true},
            {"id": "weird", "label": "Odd", "description": null, "effect": "unknown_effect"}
        ])
    }

    /// The real client view of one machine with one agent, so the answerable
    /// and selectable rules under test are the client's own.
    fn screen_of(caps: &[&str], online: bool, agent: Agent) -> ScreenModel {
        let mut session = Session::default();
        session.welcome(
            Machine {
                id: "m".into(),
                name: "devbox".into(),
                acd_version: "0".into(),
                sources: Sources {
                    hooks: SourceStatus::unconfigured(),
                    herdr: SourceStatus::unconfigured(),
                    t3: SourceStatus::unconfigured(),
                    external: false,
                },
            },
            caps.iter().map(|c| (*c).to_owned()).collect(),
            "e1",
            1,
        );
        session.apply(&ServerMessage::Snapshot { epoch: "e1".into(), seq: 1, agents: vec![agent] });
        let state = MachineState {
            instance: 1,
            status: if online {
                MachineStatus::Online
            } else {
                MachineStatus::Offline { since_ms: 5.0, error: "gone".into() }
            },
            session,
            conn_gen: 1,
        };
        let entry = MachineEntry {
            key: "k1".into(),
            name: "devbox".into(),
            endpoint: EndpointConfig::Tcp { host: "h".into(), port: 1 },
        };
        let view = build_view(&ViewInput {
            machines: vec![MachineInput { entry: &entry, state: Some(&state) }],
            actions: &[],
            busy: &HashSet::new(),
            notices: &HashMap::new(),
            show_idle: true,
            show_subagents: true,
            show_hook_only: true,
        });
        screen(&view)
    }

    fn only_card(s: &ScreenModel) -> &CardRow {
        assert_eq!(s.cards.len(), 1, "{s:?}");
        &s.cards[0]
    }

    #[test]
    fn a_live_answerable_request_offers_its_options_and_the_ids_to_send_back() {
        let a = agent("a", "hooks", "needs_you", request("r1", "permission", "hook", "open", yes_no()));
        let s = screen_of(&["answer"], true, a);
        let card = only_card(&s);
        assert_eq!(card.request_id.as_deref(), Some("r1"));
        assert_eq!(card.revision, 3);
        assert_eq!(card.read_only, None);
        assert_eq!(card.detail, vec!["$ ls", "\"cwd\": \"/x\""]);
        let ids: Vec<_> = card.options.iter().map(|o| (o.id.as_str(), o.enabled, o.confirm, o.deny)).collect();
        // An option with an effect this client does not know is shown, disabled.
        assert_eq!(ids, vec![("allow", true, false, false), ("deny", true, true, true), ("weird", false, false, false)]);
        assert_eq!(s.needs_you, 1);
        assert_eq!(card.subtitle, "devbox · hooks · proj");
    }

    #[test]
    fn without_the_answer_capability_the_card_is_read_only_with_no_options() {
        let a = agent("a", "hooks", "needs_you", request("r1", "permission", "hook", "open", yes_no()));
        let s = screen_of(&[], true, a);
        let card = only_card(&s);
        assert!(card.options.is_empty());
        assert_eq!(card.request_id, None);
        assert!(!card.free_text);
        assert_eq!(card.read_only.as_deref(), Some("Answer in terminal"));
        // The request is still shown whole.
        assert_eq!(card.detail.len(), 2);
    }

    #[test]
    fn a_request_with_no_answer_path_or_unparsed_is_read_only() {
        for (kind, path) in [("permission", "none"), ("blocked_unparsed", "hook"), ("unknown", "hook")] {
            let a = agent("a", "herdr", "needs_you", request("r1", kind, path, "open", yes_no()));
            let s = screen_of(&["answer"], true, a);
            let card = only_card(&s);
            assert!(card.options.is_empty(), "{kind}/{path}");
            assert_eq!(card.read_only.as_deref(), Some("Answer in terminal"), "{kind}/{path}");
        }
    }

    #[test]
    fn a_request_that_is_not_open_cannot_be_answered() {
        let a = agent("a", "hooks", "needs_you", request("r1", "permission", "hook", "resolved", yes_no()));
        let s = screen_of(&["answer"], true, a);
        assert!(only_card(&s).options.is_empty());
    }

    #[test]
    fn an_answering_request_is_busy_and_shows_neither_options_nor_a_hint() {
        let a = agent("a", "hooks", "needs_you", request("r1", "permission", "hook", "answering", yes_no()));
        let s = screen_of(&["answer"], true, a);
        let card = only_card(&s);
        assert!(card.busy);
        assert!(card.options.is_empty());
        assert_eq!(card.read_only, None);
    }

    #[test]
    fn an_offline_machine_offers_nothing_and_says_so() {
        let a = agent("a", "hooks", "needs_you", request("r1", "permission", "hook", "open", yes_no()));
        let s = screen_of(&["answer"], false, a);
        let card = only_card(&s);
        assert!(card.options.is_empty());
        assert_eq!(card.request_id, None);
        assert_eq!(card.offline_note.as_deref(), Some("offline"));
        assert_eq!(card.read_only, None);
        assert_eq!(s.machines[0].status, "offline");
        assert_eq!(s.machines[0].problem.as_deref(), Some("gone"));
    }

    #[test]
    fn the_hint_names_where_to_answer_by_source() {
        let req = request("r1", "permission", "none", "open", yes_no());
        let t3 = screen_of(&["answer"], true, agent("a", "t3", "needs_you", req.clone()));
        assert_eq!(only_card(&t3).read_only.as_deref(), Some("Answer in T3"));
        let ext = screen_of(&["answer"], true, agent("My Tool", "external", "needs_you", req));
        assert_eq!(only_card(&ext).read_only.as_deref(), Some("Answer in My Tool"));
        // An external card's heading is what is asked.
        assert_eq!(only_card(&ext).title, "Bash");
    }

    #[test]
    fn a_working_card_has_no_options_and_no_hint() {
        let s = screen_of(&["answer"], true, agent("a", "hooks", "working", Value::Null));
        let card = only_card(&s);
        assert!(card.options.is_empty());
        assert_eq!(card.read_only, None);
        assert_eq!(card.group, "working");
    }

    #[test]
    fn a_free_text_question_takes_typed_answers_only_when_answerable() {
        let mut req = request("r1", "question", "hook", "open", json!([{"id": "o1", "label": "A", "description": null, "effect": "choose"}]));
        req["questions"] = json!([{"id": "q", "header": null, "text": "Which branch?", "multi_select": false,
            "options": [{"id": "o1", "label": "A", "description": null, "effect": "choose"}]}]);
        req["free_text"] = json!(true);
        req["detail"] = json!("pick one");
        let live = screen_of(&["answer"], true, agent("a", "hooks", "needs_you", req.clone()));
        let card = only_card(&live);
        assert!(card.free_text);
        // The question text is added when the detail does not carry it.
        assert_eq!(card.detail, vec!["pick one", "Which branch?"]);
        let ro = screen_of(&[], true, agent("a", "hooks", "needs_you", req));
        assert!(!only_card(&ro).free_text);
    }

    #[test]
    fn an_unparsed_screen_is_shown() {
        let mut req = request("r1", "blocked_unparsed", "none", "open", json!([]));
        req["raw_screen"] = json!("line one\nline two");
        req["detail"] = json!("");
        let s = screen_of(&["answer"], true, agent("a", "herdr", "needs_you", req));
        assert_eq!(only_card(&s).detail, vec!["", "line one", "line two"]);
    }

    #[test]
    fn machine_problems_are_mapped_and_duplicates_hidden() {
        let machine = |key: &str, status: MachineStatus, dup: Option<&str>| acd_client::view::MachineView {
            key: key.into(),
            name: key.into(),
            machine_id: None,
            status,
            is_local: false,
            duplicate_of: dup.map(Into::into),
        };
        let view = InboxView {
            machines: vec![
                machine("a", MachineStatus::Syncing, None),
                machine("b", MachineStatus::IdentityChanged { message: "pin changed".into() }, None),
                machine("c", MachineStatus::Online, Some("a")),
            ],
            cards: vec![],
            hidden_idle: 0,
            show_idle: true,
            show_subagents: true,
            show_hook_only: true,
            tray: acd_client::view::TraySummary { color: acd_client::view::TrayColor::Grey, needs_you: 0 },
            actions: vec![],
        };
        let s = screen(&view);
        let rows: Vec<_> = s.machines.iter().map(|m| (m.key.as_str(), m.status.as_str(), m.problem.as_deref())).collect();
        assert_eq!(rows, vec![("a", "connecting", None), ("b", "attention", Some("pin changed"))]);
    }
}
