use std::collections::{HashMap, HashSet};

use acd_protocol::{
    Agent, AgentState, Effect, ErrorCode, RequestStatus, ResolveReason, ResolvedBy,
};
use serde::Serialize;

use crate::{
    commands::{NotSent, Outcome},
    conn::{MachineState, MachineStatus},
    settings::{EndpointConfig, MachineEntry, MachineSecret, SecretStore},
};

pub const NOTICE_ANSWERED_ELSEWHERE: &str = "Answered elsewhere";
pub const NOTICE_CHECK_TERMINAL: &str = "Check the terminal";
pub const NOTICE_OFFLINE_NOT_SENT: &str = "Offline. Nothing was sent.";
pub const NOTICE_REQUEST_CHANGED: &str = "The request changed. Nothing was sent.";
pub const NOTICE_NOT_PAIRED: &str =
    "Pair this machine: run `acd pair` there, copy the line, then Add from clipboard";

#[derive(Serialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct InboxView {
    pub machines: Vec<MachineView>,
    pub cards: Vec<CardView>,
    pub hidden_idle: u32,
    pub show_idle: bool,
    pub show_subagents: bool,
    pub show_hook_only: bool,
    pub tray: TraySummary,
    pub actions: Vec<ActionView>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineView {
    pub key: String,
    pub name: String,
    pub machine_id: Option<String>,
    pub status: MachineStatus,
    pub is_local: bool,
    pub duplicate_of: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct CardView {
    pub key: String,
    pub machine_key: String,
    pub machine_name: String,
    pub agent: Agent,
    pub group: CardGroup,
    pub live: bool,
    pub offline_since_ms: Option<f64>,
    pub answerable: bool,
    /// Answerable, and the request also takes a typed answer.
    pub free_text_answerable: bool,
    pub selectable_options: Vec<String>,
    pub can_reply: bool,
    pub can_mark_seen: bool,
    pub busy: bool,
    pub notice: Option<Notice>,
    pub action: Option<ActionView>,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CardGroup {
    Request,
    Done,
    Working,
    Unknown,
    Idle,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Notice {
    pub tone: NoticeTone,
    pub text: String,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NoticeTone {
    Info,
    Warning,
}

#[derive(Serialize, Clone, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ActionView {
    pub id: String,
    pub card_key: String,
    pub label: String,
    pub detail: Option<String>,
    pub fires_at_ms: f64,
}

/// `detail` is the user's typed reply or answer (voice spec §3.7): `Debug`
/// shows its length, so no derived `Debug` above this type can print it.
impl std::fmt::Debug for ActionView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionView")
            .field("id", &self.id)
            .field("card_key", &self.card_key)
            .field("label_bytes", &self.label.len())
            .field("detail_bytes", &self.detail.as_ref().map(String::len))
            .field("fires_at_ms", &self.fires_at_ms)
            .finish()
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct TraySummary {
    pub color: TrayColor,
    pub needs_you: u32,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum TrayColor {
    Amber,
    Blue,
    Green,
    Grey,
}

#[derive(Clone, Copy, Debug)]
pub struct MachineInput<'a> {
    pub entry: &'a MachineEntry,
    pub state: Option<&'a MachineState>,
}

/// A hook-only agent (known through acd's hooks alone: no herdr pane, not
/// T3) cannot be messaged and has no pane to open, so it earns a card only
/// while it has a request. An agent waiting on the user without a held
/// request (a prompt passed through to the terminal) stays visible. Used by
/// the view and by `drop_hidden_actions`.
pub(crate) fn hook_only_hidden(agent: &Agent, show_hook_only: bool) -> bool {
    !show_hook_only
        && agent.source == acd_protocol::Source::Hooks
        && agent.pending.is_none()
        && agent.state != AgentState::NeedsYou
}

pub struct ViewInput<'a> {
    pub machines: Vec<MachineInput<'a>>,
    pub actions: &'a [ActionView],
    pub busy: &'a HashSet<String>,
    pub notices: &'a HashMap<String, Notice>,
    pub show_idle: bool,
    pub show_subagents: bool,
    pub show_hook_only: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineEntryView {
    pub key: String,
    pub name: String,
    pub endpoint: EndpointConfig,
    pub pairing: PairingState,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PairingState {
    Paired,
    NotPaired,
    Unreadable,
}

impl MachineEntryView {
    pub fn from_entry(entry: &MachineEntry, secret: Option<&MachineSecret>) -> Self {
        Self {
            key: entry.key.clone(),
            name: entry.name.clone(),
            endpoint: entry.endpoint.clone(),
            pairing: if matches!(entry.endpoint, EndpointConfig::Local) || secret.is_some() {
                PairingState::Paired
            } else {
                PairingState::NotPaired
            },
        }
    }
}

pub fn machine_entry_views(
    entries: &[MachineEntry],
    secrets: &dyn SecretStore,
) -> Result<Vec<MachineEntryView>, String> {
    entries
        .iter()
        .map(|entry| {
            let pairing = match entry.endpoint {
                EndpointConfig::Local => PairingState::Paired,
                EndpointConfig::Tcp { .. } => match secrets.get(&entry.key) {
                    Err(_) => return Err("Could not read saved machine credentials".into()),
                    Ok(None) => PairingState::NotPaired,
                    Ok(Some(raw)) => match MachineSecret::from_json(&raw) {
                        Ok(_) => PairingState::Paired,
                        Err(_) => PairingState::Unreadable,
                    },
                },
            };
            Ok(MachineEntryView {
                key: entry.key.clone(),
                name: entry.name.clone(),
                endpoint: entry.endpoint.clone(),
                pairing,
            })
        })
        .collect()
}

pub fn card_key(machine_key: &str, agent_id: &str) -> String {
    format!("{machine_key}/{agent_id}")
}

pub fn split_card_key(key: &str) -> Option<(&str, &str)> {
    key.split_once('/')
}

/// The card notice for a daemon error. Classified by code only: the
/// daemon's message text never reaches a notice (voice spec §3.7).
pub fn notice_for_error(code: ErrorCode) -> Notice {
    use ErrorCode::*;
    let (text, tone) = match code {
        RequestGone => (NOTICE_ANSWERED_ELSEWHERE.to_owned(), NoticeTone::Info),
        RequestChanged => (
            "The request changed. Nothing was sent.".into(),
            NoticeTone::Warning,
        ),
        RequestNotReady => (
            "Not ready yet. Enlarge the pane to answer here.".into(),
            NoticeTone::Warning,
        ),
        ScreenMismatch => (
            "The terminal shows a different dialog. Nothing was sent.".into(),
            NoticeTone::Warning,
        ),
        RequestBusy => ("Another answer is in progress".into(), NoticeTone::Info),
        NotAnswerable => ("Answer in terminal".into(), NoticeTone::Info),
        AgentGone => ("The agent is gone".into(), NoticeTone::Info),
        AgentBusy => (
            "The agent is busy. Try again when it is idle.".into(),
            NoticeTone::Warning,
        ),
        SourceUnavailable => (
            "The source is unavailable. Nothing was sent.".into(),
            NoticeTone::Warning,
        ),
        NotSupported => ("Not supported for this agent".into(), NoticeTone::Info),
        _ => (format!("Could not send: {}.", code.describe()), NoticeTone::Warning),
    };
    Notice { tone, text }
}

pub fn notice_for_outcome(outcome: &Outcome) -> Option<Notice> {
    match outcome {
        Outcome::Failed { code, .. } => Some(notice_for_error(*code)),
        Outcome::Uncertain => Some(Notice {
            tone: NoticeTone::Warning,
            text: NOTICE_CHECK_TERMINAL.into(),
        }),
        Outcome::NotSent(NotSent::Offline) => Some(Notice {
            tone: NoticeTone::Warning,
            text: NOTICE_OFFLINE_NOT_SENT.into(),
        }),
        Outcome::NotSent(NotSent::NotAdvertised) => {
            Some(notice_for_error(ErrorCode::NotSupported))
        }
        Outcome::Delivered | Outcome::Done => None,
    }
}

pub const NOTICE_STRAY_KEYSTROKE: &str = "A stray keystroke may be in the input box";

pub fn resolution_notice(by: ResolvedBy, reason: Option<ResolveReason>) -> Option<Notice> {
    if by == ResolvedBy::Uncertain && reason == Some(ResolveReason::StrayKeystroke) {
        return Some(Notice {
            tone: NoticeTone::Warning,
            text: NOTICE_STRAY_KEYSTROKE.into(),
        });
    }
    match by {
        ResolvedBy::Terminal => Some(Notice {
            tone: NoticeTone::Info,
            text: "Answered in the terminal".into(),
        }),
        ResolvedBy::Uncertain | ResolvedBy::Lost | ResolvedBy::Unknown => Some(Notice {
            tone: NoticeTone::Warning,
            text: NOTICE_CHECK_TERMINAL.into(),
        }),
        ResolvedBy::Inbox | ResolvedBy::Agent | ResolvedBy::Superseded => None,
    }
}

pub(crate) fn group(agent: &Agent) -> CardGroup {
    if agent.pending.is_some() || agent.state == AgentState::NeedsYou {
        CardGroup::Request
    } else {
        match agent.state {
            AgentState::Done => CardGroup::Done,
            AgentState::Working => CardGroup::Working,
            AgentState::Idle => CardGroup::Idle,
            AgentState::NeedsYou | AgentState::Unknown => CardGroup::Unknown,
        }
    }
}

pub fn build_view(input: &ViewInput<'_>) -> InboxView {
    let mut machines = Vec::new();
    let mut cards = Vec::new();
    let mut owners: HashMap<String, String> = HashMap::new();
    let mut needs_you = 0;
    let mut any_working = false;
    let mut any_done = false;
    for machine in &input.machines {
        let entry = machine.entry;
        let Some(state) = machine.state else {
            machines.push(MachineView {
                key: entry.key.clone(),
                name: entry.name.clone(),
                machine_id: None,
                status: MachineStatus::AuthFailed {
                    message: NOTICE_NOT_PAIRED.into(),
                },
                is_local: entry.endpoint == EndpointConfig::Local,
                duplicate_of: None,
            });
            continue;
        };
        let machine_id = state.session.machine.as_ref().map(|m| m.id.clone());
        let duplicate_of = machine_id.as_ref().and_then(|id| owners.get(id).cloned());
        if duplicate_of.is_none()
            && let Some(id) = &machine_id
        {
            owners.insert(id.clone(), entry.key.clone());
        }
        machines.push(MachineView {
            key: entry.key.clone(),
            name: entry.name.clone(),
            machine_id,
            status: state.status.clone(),
            is_local: entry.endpoint == EndpointConfig::Local,
            duplicate_of: duplicate_of.clone(),
        });
        if duplicate_of.is_some() {
            continue;
        }
        let live = state.status == MachineStatus::Online;
        let offline_since_ms = match state.status {
            MachineStatus::Offline { since_ms, .. } => Some(since_ms),
            _ => None,
        };
        for agent in state.session.agents.values() {
            // A hidden subagent is not counted, shown or notified anywhere.
            if !input.show_subagents && agent.role == acd_protocol::AgentRole::Subagent {
                continue;
            }
            let key = card_key(&entry.key, &agent.id);
            let action = input.actions.iter().find(|a| a.card_key == key).cloned();
            // A hook-only agent without a request is noise, unless a
            // countdown runs on it (same rule as an idle card).
            if hook_only_hidden(agent, input.show_hook_only) && action.is_none() {
                continue;
            }
            let card_group = group(agent);
            if live {
                needs_you += u32::from(agent.state == AgentState::NeedsYou);
                any_working |= agent.state == AgentState::Working;
                any_done |= agent.state == AgentState::Done;
            }
            let answerable = live
                && state.session.has("answer")
                && agent
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.is_answerable() && p.status == RequestStatus::Open);
            let free_text_answerable =
                answerable && agent.pending.as_ref().is_some_and(|p| p.free_text);
            let selectable_options = if answerable {
                agent
                    .pending
                    .as_ref()
                    .unwrap()
                    .options
                    .iter()
                    .filter(|o| o.effect != Effect::Unknown)
                    .map(|o| o.id.clone())
                    .collect()
            } else {
                Vec::new()
            };
            cards.push(CardView {
                key: key.clone(),
                machine_key: entry.key.clone(),
                machine_name: entry.name.clone(),
                agent: agent.clone(),
                group: card_group,
                live,
                offline_since_ms,
                answerable,
                free_text_answerable,
                selectable_options,
                can_reply: live && state.session.has("send") && agent.can_send,
                can_mark_seen: live
                    && state.session.has("mark_seen")
                    && agent.state == AgentState::Done,
                busy: agent
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.status == RequestStatus::Answering)
                    || input.busy.contains(&key),
                notice: input.notices.get(&key).cloned(),
                action,
            });
        }
    }
    cards.sort_by(|a, b| {
        let group_order = |g| match g {
            CardGroup::Request => 0,
            CardGroup::Done => 1,
            CardGroup::Working => 2,
            CardGroup::Unknown => 3,
            CardGroup::Idle => 4,
        };
        let ordering = group_order(a.group).cmp(&group_order(b.group));
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
        let timestamp = if a.group == CardGroup::Request {
            a.agent
                .pending
                .as_ref()
                .map(|p| p.opened_at)
                .unwrap_or(a.agent.state_since)
                .cmp(
                    &b.agent
                        .pending
                        .as_ref()
                        .map(|p| p.opened_at)
                        .unwrap_or(b.agent.state_since),
                )
        } else {
            b.agent.state_since.cmp(&a.agent.state_since)
        };
        timestamp
            .then_with(|| a.machine_name.cmp(&b.machine_name))
            .then_with(|| a.agent.id.cmp(&b.agent.id))
    });
    // An idle card with a countdown stays on screen, so the countdown and
    // the Esc that cancels it are never invisible.
    let hide = |c: &CardView| c.group == CardGroup::Idle && c.action.is_none();
    let hidden_idle = if input.show_idle {
        0
    } else {
        cards.iter().filter(|c| hide(c)).count() as u32
    };
    if !input.show_idle {
        cards.retain(|c| !hide(c));
    }
    let color = if needs_you > 0 {
        TrayColor::Amber
    } else if any_working {
        TrayColor::Blue
    } else if any_done {
        TrayColor::Green
    } else {
        TrayColor::Grey
    };
    let mut actions = input.actions.to_vec();
    actions.sort_by(|a, b| a.fires_at_ms.total_cmp(&b.fires_at_ms));
    InboxView {
        machines,
        cards,
        hidden_idle,
        show_idle: input.show_idle,
        show_subagents: input.show_subagents,
        show_hook_only: input.show_hook_only,
        tray: TraySummary { color, needs_you },
        actions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use crate::settings::{MachineSecret, MemorySecrets, SecretError};
    use acd_protocol::RequestKind;
    use acd_protocol::*;
    use std::collections::{HashMap, HashSet};

    fn agent(id: &str, state: &str, since: &str) -> Agent {
        serde_json::from_value(serde_json::json!({"id":id,"machine_id":"m","source":"herdr","kind":"claude","title":id,"cwd":null,"project":null,"state":state,"state_since":since,"failed":false,"last_message":null,"pending":null,"can_send":true})).unwrap()
    }
    fn with_request(mut a: Agent, opened_at: &str) -> Agent {
        a.pending = Some(serde_json::from_value(serde_json::json!({"id":format!("r-{}",a.id),"agent_id":a.id,"kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"ls","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"},{"id":"deny","label":"No","description":null,"effect":"deny"}],"questions":null,"free_text":false,"raw_screen":null,"opened_at":opened_at})).unwrap());
        a
    }
    fn state(online: bool, caps: &[&str], agents: Vec<Agent>) -> MachineState {
        let mut session = Session::default();
        let machine = Machine {
            id: "m".into(),
            name: "M".into(),
            acd_version: "0".into(),
            sources: Sources {
                hooks: SourceStatus::unconfigured(),
                herdr: SourceStatus::unconfigured(),
                t3: SourceStatus::unconfigured(),
            },
        };
        session.welcome(
            machine,
            caps.iter().map(|c| c.to_string()).collect(),
            "e1",
            1,
        );
        session.apply(&ServerMessage::Snapshot {
            epoch: "e1".into(),
            seq: 1,
            agents,
        });
        let status = if online {
            MachineStatus::Online
        } else {
            MachineStatus::Offline {
                since_ms: 5.0,
                error: "x".into(),
            }
        };
        assert!(session.ready());
        MachineState {
            instance: 1,
            status,
            session,
            conn_gen: 1,
        }
    }
    fn entry(key: &str) -> MachineEntry {
        MachineEntry {
            key: key.into(),
            name: key.into(),
            endpoint: EndpointConfig::Tcp {
                host: "h".into(),
                port: 1,
            },
        }
    }
    const ALL: &[&str] = &["answer", "send", "mark_seen"];
    fn view_of(states: &[(&MachineEntry, &MachineState)], show_idle: bool) -> InboxView {
        let (busy, notices) = (HashSet::new(), HashMap::new());
        build_view(&ViewInput {
            machines: states
                .iter()
                .map(|(e, s)| MachineInput {
                    entry: e,
                    state: Some(s),
                })
                .collect(),
            actions: &[],
            busy: &busy,
            notices: &notices,
            show_idle,
            show_subagents: false,
            show_hook_only: false,
        })
    }

    #[test]
    fn cards_are_ordered_requests_done_working_unknown_then_idle() {
        let t = |m: u32| format!("2026-10-08T10:{m:02}:00.000Z");
        let agents = vec![
            with_request(agent("a", "needs_you", &t(0)), &t(2)),
            with_request(agent("b", "needs_you", &t(0)), &t(1)),
            agent("c", "done", &t(5)),
            agent("d", "done", &t(6)),
            agent("e", "working", &t(0)),
            agent("f", "dreaming", &t(0)),
            agent("g", "idle", &t(0)),
        ];
        let (e, s) = (entry("m1"), state(true, ALL, agents));
        let ids = |v: &InboxView| {
            v.cards
                .iter()
                .map(|c| c.agent.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(&view_of(&[(&e, &s)], true)),
            ["b", "a", "d", "c", "e", "f", "g"]
        );
        let hidden = view_of(&[(&e, &s)], false);
        assert_eq!(ids(&hidden), ["b", "a", "d", "c", "e", "f"]);
        assert_eq!(hidden.hidden_idle, 1);
    }

    #[test]
    fn actions_are_ordered_by_fire_time() {
        let (e, s) = (entry("m"), state(true, ALL, vec![]));
        let actions = vec![
            ActionView {
                id: "later".into(),
                card_key: "m/a".into(),
                label: "later".into(),
                detail: None,
                fires_at_ms: 20.0,
            },
            ActionView {
                id: "first".into(),
                card_key: "m/b".into(),
                label: "first".into(),
                detail: None,
                fires_at_ms: 10.0,
            },
        ];
        let v = build_view(&ViewInput {
            machines: vec![MachineInput {
                entry: &e,
                state: Some(&s),
            }],
            actions: &actions,
            busy: &HashSet::new(),
            notices: &HashMap::new(),
            show_idle: false,
            show_subagents: false,
            show_hook_only: false,
        });
        assert_eq!(
            v.actions.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            ["first", "later"]
        );
    }

    #[test]
    fn cards_resolve_notice_and_action_by_card_key() {
        let t = "2026-10-08T10:00:00.000Z";
        let (e, s) = (entry("m"), state(true, ALL, vec![agent("a", "working", t)]));
        let notice = Notice {
            tone: NoticeTone::Info,
            text: "saved".into(),
        };
        let action = ActionView {
            id: "act".into(),
            card_key: "m/a".into(),
            label: "Undo".into(),
            detail: Some("allow".into()),
            fires_at_ms: 10.0,
        };
        let notices = HashMap::from([("m/a".into(), notice.clone())]);
        let v = build_view(&ViewInput {
            machines: vec![MachineInput {
                entry: &e,
                state: Some(&s),
            }],
            actions: std::slice::from_ref(&action),
            busy: &HashSet::new(),
            notices: &notices,
            show_idle: false,
            show_subagents: false,
            show_hook_only: false,
        });
        assert_eq!(v.cards[0].notice, Some(notice));
        assert_eq!(v.cards[0].action, Some(action));
    }
    #[test]
    fn tray_counts_live_needs_you_only() {
        let t = "2026-10-08T10:00:00.000Z";
        let (e1, e2) = (entry("m1"), entry("m2"));
        let live = state(
            true,
            ALL,
            vec![
                with_request(agent("a", "needs_you", t), t),
                with_request(agent("b", "needs_you", t), t),
                agent("c", "working", t),
            ],
        );
        let mut off = state(
            false,
            ALL,
            vec![
                agent("x", "needs_you", t),
                agent("y", "needs_you", t),
                agent("z", "needs_you", t),
            ],
        );
        off.session.machine.as_mut().unwrap().id = "other".into();
        assert_eq!(
            view_of(&[(&e1, &live), (&e2, &off)], false).tray,
            TraySummary {
                color: TrayColor::Amber,
                needs_you: 2
            }
        );
        let working = state(
            true,
            ALL,
            vec![agent("w", "working", t), agent("i", "idle", t)],
        );
        assert_eq!(
            view_of(&[(&e1, &working)], false).tray,
            TraySummary {
                color: TrayColor::Blue,
                needs_you: 0
            }
        );
        let done = state(true, ALL, vec![agent("d", "done", t)]);
        assert_eq!(
            view_of(&[(&e1, &done)], false).tray,
            TraySummary {
                color: TrayColor::Green,
                needs_you: 0
            }
        );
        assert_eq!(
            view_of(&[(&e2, &off)], false).tray,
            TraySummary {
                color: TrayColor::Grey,
                needs_you: 0
            }
        );
    }
    #[test]
    fn answerable_needs_online_capability_open_status_and_shape() {
        let t = "2026-10-08T10:00:00.000Z";
        let base = with_request(agent("a", "needs_you", t), t);
        let e = entry("m");
        let mut variants = vec![
            state(true, ALL, vec![base.clone()]),
            state(false, ALL, vec![base.clone()]),
            state(true, &["send"], vec![base.clone()]),
        ];
        let mut a = base.clone();
        a.pending.as_mut().unwrap().status = RequestStatus::Answering;
        variants.push(state(true, ALL, vec![a]));
        let mut a = base.clone();
        a.pending.as_mut().unwrap().answer_path = AnswerPath::None;
        variants.push(state(true, ALL, vec![a]));
        let mut a = base.clone();
        a.pending.as_mut().unwrap().kind = RequestKind::BlockedUnparsed;
        variants.push(state(true, ALL, vec![a]));
        let mut a = base.clone();
        a.pending.as_mut().unwrap().kind = RequestKind::Question;
        a.pending.as_mut().unwrap().questions = Some(vec![
            Question {
                id: "q1".into(),
                header: None,
                text: "?".into(),
                multi_select: false,
                options: vec![],
            },
            Question {
                id: "q2".into(),
                header: None,
                text: "?".into(),
                multi_select: false,
                options: vec![],
            },
        ]);
        variants.push(state(true, ALL, vec![a]));
        let mut a = base.clone();
        a.pending.as_mut().unwrap().kind = RequestKind::Question;
        a.pending.as_mut().unwrap().questions = Some(vec![Question {
            id: "q".into(),
            header: None,
            text: "?".into(),
            multi_select: true,
            options: vec![],
        }]);
        variants.push(state(true, ALL, vec![a]));
        let mut a = base.clone();
        a.pending.as_mut().unwrap().status = RequestStatus::Unknown;
        variants.push(state(true, ALL, vec![a]));
        assert!(view_of(&[(&e, &variants[0])], false).cards[0].answerable);
        for s in &variants[1..] {
            assert!(!view_of(&[(&e, s)], false).cards[0].answerable);
        }
    }
    #[test]
    fn unknown_effect_is_not_selectable() {
        let t = "2026-10-08T10:00:00.000Z";
        let mut a = with_request(agent("a", "needs_you", t), t);
        a.pending.as_mut().unwrap().options.insert(
            1,
            RequestOption {
                id: "weird".into(),
                label: "?".into(),
                description: None,
                effect: Effect::Unknown,
            },
        );
        let (e, s) = (entry("m"), state(true, ALL, vec![a]));
        assert_eq!(
            view_of(&[(&e, &s)], false).cards[0].selectable_options,
            ["allow", "deny"]
        );
    }
    #[test]
    fn reply_and_seen_follow_capabilities_and_liveness() {
        let t = "2026-10-08T10:00:00.000Z";
        let (e, all) = (
            entry("m"),
            state(
                true,
                ALL,
                vec![agent("a", "working", t), agent("d", "done", t)],
            ),
        );
        let cards = view_of(&[(&e, &all)], false).cards;
        assert!(cards[0].can_reply);
        assert!(
            cards
                .iter()
                .find(|c| c.agent.id == "d")
                .unwrap()
                .can_mark_seen
        );
        let limited = state(
            true,
            &["answer"],
            vec![agent("a", "working", t), agent("d", "done", t)],
        );
        let cards = view_of(&[(&e, &limited)], false).cards;
        assert!(!cards.iter().any(|c| c.can_reply || c.can_mark_seen));
        let off = state(false, ALL, vec![agent("d", "done", t)]);
        let card = &view_of(&[(&e, &off)], false).cards[0];
        assert!(!card.can_reply && !card.can_mark_seen);
        let mut no_send = agent("n", "working", t);
        no_send.can_send = false;
        let s = state(true, ALL, vec![no_send]);
        assert!(!view_of(&[(&e, &s)], false).cards[0].can_reply);
    }
    #[test]
    fn busy_when_answering_or_own_command_in_flight() {
        let t = "2026-10-08T10:00:00.000Z";
        let mut a = with_request(agent("a", "needs_you", t), t);
        a.pending.as_mut().unwrap().status = RequestStatus::Answering;
        let s = state(true, ALL, vec![a, agent("b", "working", t)]);
        let e = entry("m");
        let mut busy = HashSet::new();
        busy.insert("m/b".into());
        let cards = build_view(&ViewInput {
            machines: vec![MachineInput {
                entry: &e,
                state: Some(&s),
            }],
            actions: &[],
            busy: &busy,
            notices: &HashMap::new(),
            show_idle: false,
            show_subagents: false,
            show_hook_only: false,
        })
        .cards;
        assert!(cards.iter().all(|c| c.busy));
    }
    #[test]
    fn offline_cards_are_kept_greyed_with_since() {
        let t = "2026-10-08T10:00:00.000Z";
        let (e, s) = (
            entry("m"),
            state(
                false,
                ALL,
                vec![with_request(agent("a", "needs_you", t), t)],
            ),
        );
        let c = &view_of(&[(&e, &s)], false).cards[0];
        assert!(!c.live && !c.answerable);
        assert_eq!(c.offline_since_ms, Some(5.0));
    }
    #[test]
    fn duplicate_machine_is_flagged_and_its_cards_hidden() {
        let t = "2026-10-08T10:00:00.000Z";
        let (e1, e2) = (entry("m1"), entry("m2"));
        let (s1, s2) = (
            state(true, ALL, vec![agent("a", "working", t)]),
            state(true, ALL, vec![agent("b", "working", t)]),
        );
        let v = view_of(&[(&e1, &s1), (&e2, &s2)], false);
        assert_eq!(v.machines[1].duplicate_of.as_deref(), Some("m1"));
        assert_eq!(v.cards.len(), 1);
        assert_eq!(v.cards[0].agent.id, "a");
    }
    #[test]
    fn missing_state_shows_not_paired_status() {
        let e = entry("m");
        let v = build_view(&ViewInput {
            machines: vec![MachineInput {
                entry: &e,
                state: None,
            }],
            actions: &[],
            busy: &HashSet::new(),
            notices: &HashMap::new(),
            show_idle: false,
            show_subagents: false,
            show_hook_only: false,
        });
        assert_eq!(
            v.machines[0].status,
            MachineStatus::AuthFailed {
                message: NOTICE_NOT_PAIRED.into()
            }
        );
        assert!(v.cards.is_empty());
    }
    #[test]
    fn syncing_cards_are_visible_but_not_actionable() {
        let t = "2026-10-08T10:00:00.000Z";
        let (e, mut s) = (
            entry("m"),
            state(true, ALL, vec![with_request(agent("a", "needs_you", t), t)]),
        );
        s.status = MachineStatus::Syncing;
        let c = &view_of(&[(&e, &s)], false).cards[0];
        assert!(!c.live && !c.answerable && !c.can_reply);
        assert_eq!(c.offline_since_ms, None);
    }
    #[test]
    fn card_keys_split_at_the_first_slash() {
        let k = card_key("m1", "herdr:abc:w1:p2");
        assert_eq!(split_card_key(&k), Some(("m1", "herdr:abc:w1:p2")));
        assert_eq!(split_card_key("noslash"), None);
    }
    /// Final review B3: a daemon's message text never reaches a notice;
    /// the fallback is classified by code.
    #[test]
    fn a_daemon_message_never_reaches_a_notice() {
        use ErrorCode::*;
        for code in [BadRequest, DuplicateMismatch, Unauthorized, UnsupportedProtocol, Unknown] {
            let n = notice_for_outcome(&super::Outcome::Failed { code, message: "SECRET-DAEMON echo of run the tests".into() }).unwrap();
            assert!(!n.text.contains("SECRET"), "{code:?}: {}", n.text);
            assert_eq!(n.text, format!("Could not send: {}.", code.describe()));
        }
    }
    #[test]
    fn feedback_strings_match_the_table() {
        use ErrorCode::*;
        let t = |c| notice_for_error(c).text;
        assert_eq!(t(RequestGone), "Answered elsewhere");
        assert_eq!(t(RequestChanged), "The request changed. Nothing was sent.");
        assert_eq!(
            t(RequestNotReady),
            "Not ready yet. Enlarge the pane to answer here."
        );
        assert_eq!(
            t(ScreenMismatch),
            "The terminal shows a different dialog. Nothing was sent."
        );
        assert_eq!(t(RequestBusy), "Another answer is in progress");
        assert_eq!(t(NotAnswerable), "Answer in terminal");
        assert_eq!(t(AgentGone), "The agent is gone");
        assert_eq!(
            t(AgentBusy),
            "The agent is busy. Try again when it is idle."
        );
        assert_eq!(
            t(SourceUnavailable),
            "The source is unavailable. Nothing was sent."
        );
        assert_eq!(t(NotSupported), "Not supported for this agent");
        assert_eq!(t(BadRequest), "Could not send: malformed or invalid message.");
        assert_eq!(t(Unknown), "Could not send: unknown error.");
        assert_eq!(notice_for_error(RequestGone).tone, NoticeTone::Info);
        assert_eq!(
            notice_for_error(RequestChanged).tone,
            NoticeTone::Warning
        );
        assert_eq!(
            notice_for_outcome(&super::Outcome::Failed {
                code: RequestChanged,
                message: ErrorCode::RequestChanged.describe().into(),
            })
            .unwrap(),
            Notice {
                text: "The request changed. Nothing was sent.".into(),
                tone: NoticeTone::Warning
            }
        );
        assert_eq!(
            notice_for_error(RequestNotReady).tone,
            NoticeTone::Warning
        );
        assert_eq!(
            notice_for_outcome(&super::Outcome::Uncertain).unwrap().text,
            "Check the terminal"
        );
        assert_eq!(
            notice_for_outcome(&super::Outcome::NotSent(NotSent::Offline))
                .unwrap()
                .text,
            "Offline. Nothing was sent."
        );
        assert_eq!(notice_for_outcome(&super::Outcome::Delivered), None);
        assert_eq!(
            notice_for_outcome(&super::Outcome::Uncertain).unwrap().tone,
            NoticeTone::Warning
        );
        assert_eq!(
            notice_for_outcome(&super::Outcome::NotSent(NotSent::NotAdvertised))
                .unwrap()
                .tone,
            NoticeTone::Info
        );
        assert_eq!(
            resolution_notice(ResolvedBy::Uncertain, None).unwrap().text,
            "Check the terminal"
        );
        assert_eq!(
            resolution_notice(ResolvedBy::Lost, None).unwrap().text,
            "Check the terminal"
        );
        assert_eq!(
            resolution_notice(ResolvedBy::Unknown, None).unwrap().text,
            "Check the terminal"
        );
        assert_eq!(
            resolution_notice(ResolvedBy::Terminal, None).unwrap().text,
            "Answered in the terminal"
        );
        assert_eq!(resolution_notice(ResolvedBy::Inbox, None), None);
        assert_eq!(resolution_notice(ResolvedBy::Agent, None), None);
        assert_eq!(resolution_notice(ResolvedBy::Superseded, None), None);
    }

    #[test]
    fn stray_keystroke_reason_has_its_own_notice() {
        let n =
            resolution_notice(ResolvedBy::Uncertain, Some(ResolveReason::StrayKeystroke)).unwrap();
        assert_eq!(n.text, "A stray keystroke may be in the input box");
        assert_eq!(n.tone, NoticeTone::Warning);
        assert_eq!(
            resolution_notice(ResolvedBy::Uncertain, Some(ResolveReason::Unknown))
                .unwrap()
                .text,
            "Check the terminal"
        );
        assert_eq!(
            resolution_notice(ResolvedBy::Inbox, Some(ResolveReason::StrayKeystroke)),
            None
        );
    }

    #[test]
    fn settings_pairing_distinguishes_absent_and_unreadable_secrets() {
        struct FailingSecrets;
        impl crate::settings::SecretStore for FailingSecrets {
            fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
                Err(SecretError("sensitive detail".into()))
            }
            fn set(&self, _: &str, _: &str) -> Result<(), SecretError> {
                unreachable!()
            }
            fn delete(&self, _: &str) -> Result<(), SecretError> {
                unreachable!()
            }
        }
        let remote = entry("remote");
        let local = MachineEntry {
            key: "local".into(),
            name: "Local".into(),
            endpoint: EndpointConfig::Local,
        };
        let absent = MemorySecrets::default();
        let list = machine_entry_views(std::slice::from_ref(&remote), &absent).unwrap();
        assert_eq!(list[0].pairing, PairingState::NotPaired);
        assert_eq!(
            machine_entry_views(std::slice::from_ref(&local), &FailingSecrets).unwrap()[0].pairing,
            PairingState::Paired
        );
        assert_eq!(
            machine_entry_views(std::slice::from_ref(&remote), &FailingSecrets).unwrap_err(),
            "Could not read saved machine credentials"
        );

        let malformed = MemorySecrets::default();
        malformed.set(&remote.key, "not-json").unwrap();
        assert_eq!(
            machine_entry_views(std::slice::from_ref(&remote), &malformed).unwrap()[0].pairing,
            PairingState::Unreadable
        );
        let valid = MemorySecrets::default();
        valid
            .set(
                &remote.key,
                &MachineSecret {
                    token: "A".repeat(43),
                    pin: format!("sha256:{}", "A".repeat(43)),
                }
                .to_json(),
            )
            .unwrap();
        assert_eq!(
            machine_entry_views(&[remote], &valid).unwrap()[0].pairing,
            PairingState::Paired
        );
    }

    #[test]
    fn subagents_are_left_out_of_cards_counts_and_tray_unless_shown() {
        let t = "2026-10-08T10:00:00.000Z";
        let mut sub = with_request(agent("sub", "needs_you", t), t);
        sub.role = AgentRole::Subagent;
        sub.parent_title = Some("Parent".into());
        let mut sub_working = agent("subw", "working", t);
        sub_working.role = AgentRole::Subagent;
        let main = agent("main", "working", t);
        let (e, s) = (entry("m1"), state(true, ALL, vec![sub, sub_working, main]));
        let build = |show_subagents| {
            let (busy, notices) = (HashSet::new(), HashMap::new());
            build_view(&ViewInput {
                machines: vec![MachineInput {
                    entry: &e,
                    state: Some(&s),
                }],
                actions: &[],
                busy: &busy,
                notices: &notices,
                show_idle: true,
                show_subagents,
                show_hook_only: false,
            })
        };
        let hidden = build(false);
        assert_eq!(
            hidden.cards.iter().map(|c| c.agent.id.as_str()).collect::<Vec<_>>(),
            ["main"]
        );
        assert_eq!(hidden.tray.needs_you, 0);
        assert_eq!(hidden.tray.color, TrayColor::Blue);
        assert!(!hidden.show_subagents);
        let shown = build(true);
        assert_eq!(shown.cards.len(), 3);
        assert_eq!(shown.tray.needs_you, 1);
        assert_eq!(shown.tray.color, TrayColor::Amber);
        assert!(shown.show_subagents);
        let card = shown.cards.iter().find(|c| c.agent.id == "sub").unwrap();
        assert_eq!(card.agent.role, AgentRole::Subagent);
        assert_eq!(card.agent.parent_title.as_deref(), Some("Parent"));
    }

    #[test]
    fn hook_only_agents_show_only_with_a_request_or_an_action() {
        let t = "2026-10-08T00:00:00.000Z";
        let hooks = |id: &str, state: &str| {
            let mut a = agent(id, state, t);
            a.source = acd_protocol::Source::Hooks;
            a
        };
        let quiet = hooks("quiet", "working");
        let needy = with_request(hooks("needy", "needs_you"), t);
        let counting = hooks("counting", "working");
        let herdr = agent("herdr", "working", t);
        let (e, s) = (entry("m1"), state(true, ALL, vec![quiet, needy, counting, herdr]));
        let action = ActionView {
            id: "act".into(),
            card_key: "m1/counting".into(),
            label: "Reply".into(),
            detail: None,
            fires_at_ms: 10.0,
        };
        let build = |show_hook_only, actions: &[ActionView]| {
            let (busy, notices) = (HashSet::new(), HashMap::new());
            build_view(&ViewInput {
                machines: vec![MachineInput {
                    entry: &e,
                    state: Some(&s),
                }],
                actions,
                busy: &busy,
                notices: &notices,
                show_idle: true,
                show_subagents: false,
                show_hook_only,
            })
        };
        let ids = |v: &InboxView| {
            let mut ids: Vec<_> = v.cards.iter().map(|c| c.agent.id.clone()).collect();
            ids.sort();
            ids
        };
        let hidden = build(false, &[]);
        assert_eq!(ids(&hidden), ["herdr", "needy"]);
        assert_eq!(hidden.tray.needs_you, 1, "a pending hook-only request counts");
        assert!(!hidden.show_hook_only);
        // A running countdown keeps its card.
        assert_eq!(ids(&build(false, &[action])), ["counting", "herdr", "needy"]);
        let shown = build(true, &[]);
        assert_eq!(ids(&shown), ["counting", "herdr", "needy", "quiet"]);
        assert!(shown.show_hook_only);
    }

    #[test]
    fn hidden_hook_only_agents_do_not_colour_the_tray() {
        let t = "2026-10-08T00:00:00.000Z";
        let mut done = agent("done", "done", t);
        done.source = acd_protocol::Source::Hooks;
        let (e, s) = (entry("m1"), state(true, ALL, vec![done]));
        let (busy, notices) = (HashSet::new(), HashMap::new());
        let v = build_view(&ViewInput {
            machines: vec![MachineInput {
                entry: &e,
                state: Some(&s),
            }],
            actions: &[],
            busy: &busy,
            notices: &notices,
            show_idle: true,
            show_subagents: false,
            show_hook_only: false,
        });
        assert!(v.cards.is_empty());
        assert_eq!(v.tray.color, TrayColor::Grey);
    }

    #[test]
    fn hook_only_corner_cases() {
        let t = "2026-10-08T00:00:00.000Z";
        let hooks = |id: &str, state: &str| {
            let mut a = agent(id, state, t);
            a.source = acd_protocol::Source::Hooks;
            a.can_send = false;
            a
        };
        let waiting = hooks("waiting", "needs_you");
        let idle = hooks("idle", "idle");
        let mut sub = with_request(hooks("sub", "needs_you"), t);
        sub.role = AgentRole::Subagent;
        let (e, s) = (entry("m1"), state(true, ALL, vec![waiting, idle, sub]));
        let build = |show_hook_only, show_idle| {
            let (busy, notices) = (HashSet::new(), HashMap::new());
            build_view(&ViewInput {
                machines: vec![MachineInput {
                    entry: &e,
                    state: Some(&s),
                }],
                actions: &[],
                busy: &busy,
                notices: &notices,
                show_idle,
                show_subagents: false,
                show_hook_only,
            })
        };
        // Waiting on the user without a held request: visible and counted.
        let v = build(false, true);
        assert_eq!(v.cards.iter().map(|c| c.agent.id.as_str()).collect::<Vec<_>>(), ["waiting"]);
        assert_eq!(v.tray.needs_you, 1);
        assert_eq!(v.tray.color, TrayColor::Amber);
        // Subagent hiding wins over a pending hook-only request.
        assert!(v.cards.iter().all(|c| c.agent.id != "sub"));
        // Shown hook-only idle agents still fall under the idle setting.
        let v = build(true, false);
        assert_eq!(v.hidden_idle, 1);
        assert_eq!(v.cards.iter().map(|c| c.agent.id.as_str()).collect::<Vec<_>>(), ["waiting"]);
    }
}
