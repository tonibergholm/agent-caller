//! acd-client's stamped view, frozen into an acd-voice snapshot (spec §4.3).
use acd_client::view::{CardGroup, CardView};
use acd_client::voice::{ConnStamp as ClientStamp, StampedView};
use acd_client::conn::MachineStatus;
use acd_voice::snapshot::{CardSnap, ConnStamp, Group, MachineSnap, OptionSnap, Presentation, RequestSnap, Snapshot};

/// The cards and their stamps come from one rebuild (D5).
pub fn freeze(stamped: &StampedView, presentation: Presentation, panel_open: bool) -> Snapshot {
    let stamp = |machine: &str| stamped.stamps.get(machine).copied().map(convert);
    let mut cards: Vec<CardSnap> = stamped
        .view
        .cards
        .iter()
        .map(|c| card(c, stamp(&c.machine_key), false))
        .collect();
    cards.extend(stamped.hidden_idle_cards.iter().map(|c| card(c, stamp(&c.machine_key), true)));
    let machines = stamped
        .view
        .machines
        .iter()
        .map(|m| MachineSnap { key: m.key.clone(), online: matches!(m.status, MachineStatus::Online) })
        .collect();
    Snapshot { presentation, panel_open, show_subagents: stamped.view.show_subagents, cards, machines }
}

pub(crate) fn convert(s: ClientStamp) -> ConnStamp {
    ConnStamp { instance: s.instance, conn_gen: s.conn_gen }
}

fn group(g: CardGroup) -> Group {
    match g {
        CardGroup::Request => Group::Request,
        CardGroup::Done => Group::Done,
        CardGroup::Working => Group::Working,
        CardGroup::Unknown => Group::Unknown,
        CardGroup::Idle => Group::Idle,
    }
}

fn card(c: &CardView, conn: Option<ConnStamp>, idle_hidden: bool) -> CardSnap {
    CardSnap {
        key: c.key.clone(),
        machine_key: c.machine_key.clone(),
        machine_name: c.machine_name.clone(),
        online: c.live,
        conn,
        agent_id: c.agent.id.clone(),
        kind: c.agent.kind,
        title: c.agent.title.clone(),
        project: c.agent.project.clone(),
        state: c.agent.state,
        role: c.agent.role,
        group: group(c.group),
        can_reply: c.can_reply,
        pending: c.agent.pending.as_ref().map(|r| RequestSnap {
            id: r.id.clone(),
            revision: r.revision,
            kind: r.kind,
            title: r.title.clone(),
            detail: r.detail.clone(),
            question: r.questions.as_ref().and_then(|q| q.first()).map(|q| q.text.clone()),
            answerable: c.answerable,
            free_text: c.free_text_answerable,
            options: r
                .options
                .iter()
                .map(|o| OptionSnap {
                    id: o.id.clone(),
                    label: o.label.clone(),
                    description: o.description.clone(),
                    effect: o.effect,
                    selectable: c.selectable_options.contains(&o.id),
                    confirm: o.confirm,
                })
                .collect(),
        }),
        idle_hidden,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use acd_client::view::InboxView;
    use acd_client::view::{TrayColor, TraySummary};
    use acd_protocol::{AgentKind, Effect};
    use serde_json::json;

    fn question_card() -> CardView {
        let agent = serde_json::from_value(json!({
            "id": "a1", "machine_id": "m", "source": "t3", "kind": "codex", "title": "Pick",
            "cwd": null, "project": "app", "state": "needs_you",
            "state_since": "2026-10-08T00:00:00.000Z", "failed": false, "last_message": null,
            "can_send": true, "role": "main",
            "pending": {
                "id": "r1", "revision": 3, "agent_id": "a1", "kind": "question", "status": "open",
                "answer_path": "t3", "title": "Colour", "detail": "Which colour?",
                "options": [
                    {"id": "red", "label": "Red", "description": null, "effect": "choose"},
                    {"id": "odd", "label": "?", "description": null, "effect": "weird"}
                ],
                "questions": [{"id": "q", "header": null, "text": "Which colour, really?", "multi_select": false, "options": []}],
                "free_text": true, "raw_screen": null, "opened_at": "2026-10-08T00:00:00.000Z"
            }
        }))
        .unwrap();
        CardView {
            key: "m1/a1".into(),
            machine_key: "m1".into(),
            machine_name: "Box".into(),
            agent,
            group: CardGroup::Request,
            live: true,
            offline_since_ms: None,
            answerable: true,
            free_text_answerable: true,
            selectable_options: vec!["red".into()],
            can_reply: false,
            can_mark_seen: false,
            busy: false,
            notice: None,
            action: None,
        }
    }

    fn stamped(cards: Vec<CardView>, hidden_idle_cards: Vec<CardView>, stamps: HashMap<String, ClientStamp>) -> StampedView {
        StampedView {
            view: InboxView {
                machines: vec![],
                cards,
                hidden_idle: hidden_idle_cards.len() as u32,
                show_idle: false,
                show_subagents: true,
                show_hook_only: false,
                tray: TraySummary { color: TrayColor::Grey, needs_you: 0 },
                actions: vec![],
            },
            hidden_idle_cards,
            stamps,
        }
    }

    #[test]
    fn freezing_copies_what_voice_needs() {
        let stamps = HashMap::from([("m1".to_owned(), ClientStamp { instance: 4, conn_gen: 7 })]);
        let s = freeze(&stamped(vec![question_card()], vec![], stamps), Presentation::default(), true);
        assert!(s.panel_open && s.show_subagents);
        let c = &s.cards[0];
        assert_eq!((c.kind, c.project.as_deref(), c.group, c.online, c.idle_hidden), (AgentKind::Codex, Some("app"), Group::Request, true, false));
        assert_eq!(c.conn, Some(ConnStamp { instance: 4, conn_gen: 7 }));
        let r = c.pending.as_ref().unwrap();
        assert_eq!((r.revision, r.question.as_deref()), (3, Some("Which colour, really?")));
        assert!(r.answerable && r.free_text);
        assert!(r.options[0].selectable);
        assert_eq!((r.options[1].effect, r.options[1].selectable), (Effect::Unknown, false));
    }

    /// Alerts baseline a machine the first time it is online (plan V14), so
    /// every machine reaches the snapshot, with or without cards.
    #[test]
    fn every_machine_is_frozen_with_its_online_state() {
        use acd_client::conn::MachineStatus;
        use acd_client::view::MachineView;
        let machine = |key: &str, status: MachineStatus| MachineView {
            key: key.into(),
            name: key.into(),
            machine_id: None,
            status,
            is_local: false,
            duplicate_of: None,
        };
        let mut s = stamped(vec![question_card()], vec![], HashMap::new());
        s.view.machines = vec![
            machine("m1", MachineStatus::Online),
            machine("quiet", MachineStatus::Online),
            machine("away", MachineStatus::Offline { since_ms: 1.0, error: "x".into() }),
        ];
        let frozen = freeze(&s, Presentation::default(), false);
        let seen: Vec<(&str, bool)> = frozen.machines.iter().map(|m| (m.key.as_str(), m.online)).collect();
        assert_eq!(seen, [("m1", true), ("quiet", true), ("away", false)]);
    }

    #[test]
    fn hidden_idle_cards_are_frozen_as_such_and_unstamped_machines_have_no_stamp() {
        let mut idle = question_card();
        idle.key = "m1/a9".into();
        idle.group = CardGroup::Idle;
        let s = freeze(&stamped(vec![question_card()], vec![idle], HashMap::new()), Presentation::default(), false);
        assert_eq!(s.cards.len(), 2);
        assert!(s.cards[1].idle_hidden);
        assert_eq!(s.cards[0].conn, None);
    }
}
