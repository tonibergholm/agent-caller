//! One connection's model of a daemon (spec §7.4). Pure: no I/O, no clock.
use std::collections::BTreeMap;

use acd_protocol::text::{is_forbidden_control, is_forbidden_format};
use acd_protocol::{Agent, Effect, MAX_AGENT_BYTES, MAX_AGENTS_PER_MACHINE, Cursor, Machine, Request, RequestOption, ResolveReason, ResolvedBy, ServerMessage};

#[derive(Clone, Debug, Default)]
pub struct Session {
    pub machine: Option<Machine>,
    pub capabilities: Vec<String>,
    pub agents: BTreeMap<String, Agent>,
    cursor: Option<Cursor>,
    known_epoch: Option<String>,
    welcome_epoch: Option<String>,
    awaiting_snapshot: bool,
    watermark: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EpochChange {
    First,
    Same,
    New,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Applied {
    Ignored,
    Changed,
    Opened(Request),
    Resolved {
        request_id: String,
        agent_id: String,
        by: ResolvedBy,
        option_id: Option<String>,
        reason: Option<ResolveReason>,
        message: Option<String>,
    },
    Resync(&'static str),
}

fn agent_too_big(agent: &Agent) -> bool {
    serde_json::to_vec(agent).map_or(true, |v| v.len() > MAX_AGENT_BYTES)
}

/// Peer text is shown as sent except that bidi controls, separators and
/// other controls (not `\n`, `\t`) become a visible `\u{..}` escape.
/// Returns whether anything was escaped.
pub(crate) fn clean(s: &mut String) -> bool {
    if !s.chars().any(|c| is_forbidden_control(c) || is_forbidden_format(c)) {
        return false;
    }
    *s = s
        .chars()
        .map(|c| {
            if is_forbidden_control(c) || is_forbidden_format(c) {
                format!("\\u{{{:x}}}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect();
    true
}

fn clean_opt(s: &mut Option<String>) -> bool {
    s.as_mut().is_some_and(clean)
}

/// The one place that decides a label contradicts its effect. It reads the
/// label as sent, before cleaning, and skips leading characters that are not
/// letters or digits (spaces, escapes, emoji). An allow is kept only when the
/// first word approves, so any other label fails closed; a deny is refused
/// when the first word approves. Every allow label the daemon produces
/// starts with "Yes"; producers of free labels use `Effect::Choose`.
fn contradicts(o: &RequestOption) -> bool {
    let label = o.label.to_lowercase();
    let first = label.split(|c: char| !c.is_alphanumeric()).find(|w| !w.is_empty()).unwrap_or("");
    // Not "always" or "ok": "Always deny", "OK, deny" read as refusals.
    let approves = matches!(first, "yes" | "y" | "allow" | "approve" | "accept" | "proceed");
    match o.effect {
        Effect::Allow | Effect::AllowAlways => !approves,
        Effect::Deny => approves,
        _ => false,
    }
}

/// Returns whether a label or description had to be escaped.
fn clean_options(options: &mut [RequestOption]) -> bool {
    let mut escaped = false;
    for o in options {
        if contradicts(o) {
            o.effect = Effect::Unknown;
        }
        escaped |= clean(&mut o.label);
        escaped |= clean_opt(&mut o.description);
    }
    escaped
}

/// Whole or read-only: if text the user decides on had to be escaped, it is
/// not what the peer sent, so the request is not answerable here. The daemon's
/// own adapters escape such characters, so only a hostile peer's text hits it.
fn clean_request(r: &mut Request) {
    let mut escaped = clean(&mut r.title);
    escaped |= clean(&mut r.detail);
    clean_opt(&mut r.raw_screen);
    escaped |= clean_options(&mut r.options);
    for q in r.questions.iter_mut().flatten() {
        escaped |= clean_opt(&mut q.header);
        escaped |= clean(&mut q.text);
        escaped |= clean_options(&mut q.options);
    }
    if escaped {
        r.answer_path = acd_protocol::AnswerPath::None;
    }
}

fn clean_machine(m: &mut Machine) {
    clean(&mut m.name);
    clean(&mut m.acd_version);
    for status in [&mut m.sources.hooks, &mut m.sources.herdr, &mut m.sources.t3] {
        clean_opt(&mut status.message);
    }
}

fn clean_agent(a: &mut Agent) {
    clean(&mut a.title);
    clean_opt(&mut a.cwd);
    clean_opt(&mut a.project);
    clean_opt(&mut a.last_message);
    clean_opt(&mut a.parent_title);
    a.pending.iter_mut().for_each(clean_request);
}

fn event_pos(msg: &ServerMessage) -> Option<(&str, u64)> {
    match msg {
        ServerMessage::AgentUpdated { epoch, seq, .. }
        | ServerMessage::AgentRemoved { epoch, seq, .. }
        | ServerMessage::RequestOpened { epoch, seq, .. }
        | ServerMessage::RequestResolved { epoch, seq, .. }
        | ServerMessage::MachineUpdated { epoch, seq, .. } => Some((epoch, *seq)),
        _ => None,
    }
}

impl Session {
    pub fn cursor(&self) -> Option<&Cursor> {
        self.cursor.as_ref()
    }

    pub fn resume(&self) -> Option<Cursor> {
        self.cursor.clone()
    }

    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }

    pub fn synced(&self) -> bool {
        self.cursor.is_some() && !self.awaiting_snapshot
    }

    pub fn ready(&self) -> bool {
        self.synced()
            && self
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.seq >= self.watermark)
    }

    pub fn welcome(
        &mut self,
        machine: Machine,
        capabilities: Vec<String>,
        epoch: &str,
        seq: u64,
    ) -> EpochChange {
        self.watermark = seq;
        let change = match self.known_epoch.as_deref() {
            None => EpochChange::First,
            Some(known_epoch) if known_epoch == epoch => EpochChange::Same,
            Some(_) => EpochChange::New,
        };
        let mut machine = machine;
        clean_machine(&mut machine);
        self.machine = Some(machine);
        self.capabilities = capabilities;
        self.known_epoch = Some(epoch.to_owned());
        self.welcome_epoch = Some(epoch.to_owned());
        if self
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.epoch != epoch)
        {
            self.cursor = None;
        }
        self.awaiting_snapshot = self.cursor.is_none();
        change
    }

    pub fn force_resync(&mut self) {
        self.cursor = None;
        self.awaiting_snapshot = true;
    }

    pub fn apply(&mut self, msg: &ServerMessage) -> Applied {
        if let ServerMessage::Snapshot { epoch, seq, agents } = msg {
            if self.welcome_epoch.as_deref() != Some(epoch.as_str()) {
                return Applied::Resync("snapshot from another epoch");
            }
            if agents.len() > MAX_AGENTS_PER_MACHINE || agents.iter().any(agent_too_big) {
                return Applied::Resync("snapshot over the agent limits");
            }
            self.agents = agents
                .iter()
                .map(|agent| {
                    let mut agent = agent.clone();
                    clean_agent(&mut agent);
                    (agent.id.clone(), agent)
                })
                .collect();
            self.cursor = Some(Cursor {
                epoch: epoch.clone(),
                seq: *seq,
            });
            self.awaiting_snapshot = false;
            return Applied::Changed;
        }

        let Some((epoch, seq)) = event_pos(msg) else {
            return Applied::Ignored;
        };
        if self.awaiting_snapshot {
            return Applied::Resync("event before snapshot");
        }
        let Some(cursor) = self.cursor.as_mut() else {
            return Applied::Resync("event before snapshot");
        };
        if cursor.epoch != epoch {
            return Applied::Resync("event from another epoch");
        }
        if seq <= cursor.seq {
            return Applied::Ignored;
        }
        if cursor.seq.checked_add(1) != Some(seq) {
            return Applied::Resync("sequence gap");
        }
        cursor.seq = seq;

        match msg {
            ServerMessage::AgentUpdated { agent, .. } => {
                if agent_too_big(agent)
                    || (!self.agents.contains_key(&agent.id)
                        && self.agents.len() >= MAX_AGENTS_PER_MACHINE)
                {
                    return Applied::Resync("agent over the agent limits");
                }
                let mut agent = agent.clone();
                clean_agent(&mut agent);
                self.agents.insert(agent.id.clone(), agent);
                Applied::Changed
            }
            ServerMessage::AgentRemoved { agent_id, .. } => {
                self.agents.remove(agent_id);
                Applied::Changed
            }
            ServerMessage::MachineUpdated { machine, .. } => {
                let mut machine = machine.clone();
                clean_machine(&mut machine);
                self.machine = Some(machine);
                Applied::Changed
            }
            ServerMessage::RequestOpened { request, .. } => {
                let mut request = request.clone();
                clean_request(&mut request);
                Applied::Opened(request)
            }
            ServerMessage::RequestResolved {
                request_id,
                agent_id,
                by,
                option_id,
                reason,
                message,
                ..
            } => Applied::Resolved {
                request_id: request_id.clone(),
                agent_id: agent_id.clone(),
                by: *by,
                option_id: option_id.clone(),
                reason: *reason,
                message: message.clone().map(|mut m| {
                    clean(&mut m);
                    m
                }),
            },
            _ => Applied::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_protocol::*;

    fn machine() -> Machine {
        Machine {
            id: "m".into(),
            name: "M".into(),
            acd_version: "0".into(),
            sources: Sources {
                hooks: SourceStatus::unconfigured(),
                herdr: SourceStatus::unconfigured(),
                t3: SourceStatus::unconfigured(),
                external: false,
            },
        }
    }

    fn agent(id: &str) -> Agent {
        serde_json::from_value(serde_json::json!({"id": id, "machine_id": "m", "source": "herdr", "kind": "claude",
            "title": "t", "cwd": null, "project": null, "state": "working", "state_since": "2026-10-08T00:00:00.000Z",
            "failed": false, "last_message": null, "pending": null, "can_send": true})).unwrap()
    }

    fn snap(epoch: &str, seq: u64, ids: &[&str]) -> ServerMessage {
        ServerMessage::Snapshot {
            epoch: epoch.into(),
            seq,
            agents: ids.iter().map(|id| agent(id)).collect(),
        }
    }

    fn upd(epoch: &str, seq: u64, id: &str) -> ServerMessage {
        ServerMessage::AgentUpdated {
            epoch: epoch.into(),
            seq,
            agent: agent(id),
        }
    }

    #[test]
    fn first_connect_waits_for_snapshot_then_applies_in_order() {
        let mut s = Session::default();
        assert_eq!(
            s.welcome(machine(), vec!["answer".into()], "e1", 0),
            EpochChange::First
        );
        assert!(s.has("answer") && !s.has("send"));
        assert_eq!(
            s.apply(&upd("e1", 1, "a")),
            Applied::Resync("event before snapshot")
        );
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        assert_eq!(s.apply(&snap("e1", 5, &["a"])), Applied::Changed);
        assert!(s.synced());
        assert_eq!(s.apply(&upd("e1", 6, "b")), Applied::Changed);
        assert_eq!(
            s.apply(&upd("e1", 6, "b")),
            Applied::Ignored,
            "duplicate seq"
        );
        assert_eq!(
            s.resume(),
            Some(Cursor {
                epoch: "e1".into(),
                seq: 6
            })
        );
        assert_eq!(s.agents.len(), 2);
    }

    #[test]
    fn gaps_and_foreign_epochs_force_resync() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 5, &[]));
        assert_eq!(s.apply(&upd("e1", 7, "a")), Applied::Resync("sequence gap"));
        assert!(s.cursor().is_some_and(|c| c.seq == 5));
        assert!(s.agents.is_empty());
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 5, &[]));
        assert_eq!(
            s.apply(&upd("e2", 6, "a")),
            Applied::Resync("event from another epoch")
        );
        assert!(s.cursor().is_some_and(|c| c.epoch == "e1" && c.seq == 5));
        assert!(s.agents.is_empty());
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        assert_eq!(
            s.apply(&snap("e0", 5, &[])),
            Applied::Resync("snapshot from another epoch")
        );
        assert_eq!(s.resume(), None);
        assert!(s.agents.is_empty());
    }

    #[test]
    fn same_epoch_resume_keeps_agents_new_epoch_waits_for_snapshot() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 5, &["a"]));
        assert_eq!(s.welcome(machine(), vec![], "e1", 0), EpochChange::Same);
        assert!(s.synced(), "resume: replayed events apply directly");
        assert_eq!(s.apply(&upd("e1", 6, "b")), Applied::Changed);
        assert_eq!(s.welcome(machine(), vec![], "e2", 0), EpochChange::New);
        assert_eq!(s.resume(), None);
        assert_eq!(
            s.agents.len(),
            2,
            "stale agents stay visible until the snapshot"
        );
        assert_eq!(
            s.apply(&upd("e2", 1, "c")),
            Applied::Resync("event before snapshot")
        );
        s.welcome(machine(), vec![], "e2", 0);
        assert_eq!(s.apply(&snap("e2", 3, &["z"])), Applied::Changed);
        assert_eq!(s.agents.keys().collect::<Vec<_>>(), ["z"]);
    }

    #[test]
    fn ready_waits_for_watermark() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 7);
        assert!(!s.ready(), "authenticated is not ready");
        s.apply(&snap("e1", 7, &["a"]));
        assert!(s.ready());
        s.welcome(machine(), vec![], "e1", 9);
        assert!(!s.ready(), "cursor 7 < watermark 9");
        s.apply(&upd("e1", 8, "b"));
        assert!(!s.ready());
        s.apply(&upd("e1", 9, "c"));
        assert!(s.ready());
        s.welcome(machine(), vec![], "e1", 9);
        assert!(s.ready(), "nothing to replay");
        s.welcome(machine(), vec![], "e2", 3);
        assert!(!s.ready());
        s.apply(&snap("e2", 3, &[]));
        assert!(s.ready());
    }

    #[test]
    fn resync_in_the_same_epoch_is_not_a_new_epoch() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 5, &[]));
        s.force_resync();
        assert_eq!(s.resume(), None);
        assert_eq!(s.welcome(machine(), vec![], "e1", 0), EpochChange::Same);
        assert!(!s.synced(), "no cursor, so a snapshot is needed");
    }

    #[test]
    fn opened_resolved_removed_and_machine_updates() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 1, &["a"]));
        let req: Request = serde_json::from_value(serde_json::json!({"id":"r1","agent_id":"a","kind":"permission",
            "status":"open","answer_path":"hook","title":"Bash","detail":"ls","options":[],"questions":null,
            "free_text":false,"raw_screen":null,"opened_at":"2026-10-08T00:00:00.000Z"})).unwrap();
        assert_eq!(
            s.apply(&ServerMessage::RequestOpened {
                epoch: "e1".into(),
                seq: 2,
                request: req.clone()
            }),
            Applied::Opened(req)
        );
        assert_eq!(
            s.apply(&ServerMessage::RequestResolved {
                epoch: "e1".into(),
                seq: 3,
                request_id: "r1".into(),
                agent_id: "a".into(),
                by: ResolvedBy::Terminal,
                option_id: None,
                reason: None,
                message: None
            }),
            Applied::Resolved {
                request_id: "r1".into(),
                agent_id: "a".into(),
                by: ResolvedBy::Terminal,
                option_id: None,
                reason: None,
                message: None
            }
        );
        let mut m2 = machine();
        m2.name = "Renamed".into();
        assert_eq!(
            s.apply(&ServerMessage::MachineUpdated {
                epoch: "e1".into(),
                seq: 4,
                machine: m2
            }),
            Applied::Changed
        );
        assert_eq!(s.machine.as_ref().unwrap().name, "Renamed");
        assert_eq!(
            s.apply(&ServerMessage::AgentRemoved {
                epoch: "e1".into(),
                seq: 5,
                agent_id: "a".into()
            }),
            Applied::Changed
        );
        assert!(s.agents.is_empty());
        assert_eq!(
            s.apply(&ServerMessage::Pong { id: "p".into() }),
            Applied::Ignored
        );
    }

    // Demonstration for docs/research/2026-11-04-security-review-apps-voice.md (F10).
    // Fails today: a peer's text reaches the card model unchanged, bidi controls included.
    #[test]
    fn peer_text_with_bidi_controls_does_not_reach_the_card_model() {
        let mut hostile = agent("a");
        hostile.title = "safe\u{202E}fdp.exe".into();
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        let msg = ServerMessage::Snapshot {
            epoch: "e1".into(),
            seq: 1,
            agents: vec![hostile],
        };
        s.apply(&msg);
        let stored = &s.agents["a"].title;
        assert!(
            !acd_protocol::text::has_forbidden_chars(stored),
            "stored as {stored:?}"
        );
    }

    // Demonstration for docs/research/2026-11-04-security-review-apps-voice.md (F11).
    // Fails today: a peer's option labelled "Deny" with effect allow is stored as an allow, so the
    // client's confirmation table (which trusts the effect) and every renderer (which draws the
    // label) disagree. The minimal fix treats a contradicting option as `Effect::Unknown` here.
    #[test]
    fn peer_option_whose_label_contradicts_its_effect_is_not_kept_as_an_allow() {
        let mut hostile: Agent = serde_json::from_value(serde_json::json!({"id": "a", "machine_id": "m",
            "source": "herdr", "kind": "claude", "title": "t", "cwd": null, "project": null, "state": "blocked",
            "state_since": "2026-10-08T00:00:00.000Z", "failed": false, "last_message": null, "can_send": true,
            "pending": {"id": "r1", "agent_id": "a", "kind": "permission", "status": "open", "answer_path": "hook",
                "title": "Bash", "detail": "ls", "free_text": false, "raw_screen": null, "questions": null,
                "opened_at": "2026-10-08T00:00:00.000Z",
                "options": [{"id": "o1", "label": "Deny", "description": null, "effect": "allow", "confirm": false}]}}))
        .unwrap();
        assert_eq!(hostile.pending.as_ref().unwrap().options[0].effect, Effect::Allow);
        hostile.title = "t".into();
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&ServerMessage::Snapshot { epoch: "e1".into(), seq: 1, agents: vec![hostile] });
        let stored = &s.agents["a"].pending.as_ref().unwrap().options[0];
        assert_eq!(stored.effect, Effect::Unknown, "stored {:?} with effect {:?}", stored.label, stored.effect);
    }

    #[test]
    fn an_opened_request_is_cleaned_and_honest_options_are_kept() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 1, &[]));
        let req: Request = serde_json::from_value(serde_json::json!({"id":"r1","agent_id":"a","kind":"permission",
            "status":"open","answer_path":"hook","title":"Bash","detail":"ls\u{202E}\nok\ttab","free_text":false,
            "raw_screen":null,"questions":null,"opened_at":"2026-10-08T00:00:00.000Z","options":[
            {"id":"a","label":"Yes","description":null,"effect":"allow"},
            {"id":"b","label":"No, and tell Claude","description":null,"effect":"deny"}]})).unwrap();
        let Applied::Opened(r) = s.apply(&ServerMessage::RequestOpened { epoch: "e1".into(), seq: 2, request: req }) else { panic!() };
        assert_eq!(r.detail, "ls\\u{202e}\nok\ttab");
        assert_eq!((r.options[0].effect, r.options[1].effect), (Effect::Allow, Effect::Deny));
        // Whole or read-only: escaped text is not what was sent.
        assert_eq!(r.answer_path, acd_protocol::AnswerPath::None);
        assert!(!r.is_answerable());
    }

    #[test]
    fn a_request_whose_text_needs_no_escape_stays_answerable() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 1, &[]));
        let req: Request = serde_json::from_value(serde_json::json!({"id":"r1","agent_id":"a","kind":"permission",
            "status":"open","answer_path":"hook","title":"Bash","detail":"ls\nok\ttab","free_text":false,
            "raw_screen":"raw\u{202E}","questions":null,"opened_at":"2026-10-08T00:00:00.000Z","options":[
            {"id":"a","label":"Yes","description":null,"effect":"allow"}]})).unwrap();
        let Applied::Opened(r) = s.apply(&ServerMessage::RequestOpened { epoch: "e1".into(), seq: 2, request: req }) else { panic!() };
        assert_eq!(r.answer_path, acd_protocol::AnswerPath::Hook);
    }

    /// The question header is shown, so escaping it makes the request
    /// read-only like any other displayed field. Nothing else needs escaping.
    #[test]
    fn an_escaped_question_header_alone_makes_the_request_read_only() {
        let mut s = Session::default();
        s.welcome(machine(), vec![], "e1", 0);
        s.apply(&snap("e1", 1, &[]));
        let req: Request = serde_json::from_value(serde_json::json!({"id":"r1","agent_id":"a","kind":"question",
            "status":"open","answer_path":"hook","title":"Ask","detail":"d","free_text":false,
            "raw_screen":null,"opened_at":"2026-10-08T00:00:00.000Z","options":[],
            "questions":[{"id":"q1","header":"Scope\u{202E}","text":"Which?","multi_select":false,"options":[
            {"id":"a","label":"Yes","description":null,"effect":"choose"}]}]})).unwrap();
        let Applied::Opened(r) = s.apply(&ServerMessage::RequestOpened { epoch: "e1".into(), seq: 2, request: req }) else { panic!() };
        assert_eq!(r.questions.as_ref().unwrap()[0].header.as_deref(), Some("Scope\\u{202e}"));
        assert_eq!(r.answer_path, acd_protocol::AnswerPath::None);
        assert!(!r.is_answerable());
    }

    fn option_effect(label: &str, effect: Effect) -> Effect {
        let mut o = RequestOption { id: "o".into(), label: label.into(), description: None, effect, confirm: false };
        clean_options(std::slice::from_mut(&mut o));
        o.effect
    }

    #[test]
    fn an_allow_is_kept_only_when_its_label_approves() {
        for bad in ["Deny", "Nope", "Block", "Cancel", "Do not allow", "Always deny", "OK, deny", "\u{200b}Deny", "\u{1F6AB} Deny", "", "  "] {
            assert_eq!(option_effect(bad, Effect::Allow), Effect::Unknown, "{bad:?}");
            assert_eq!(option_effect(bad, Effect::AllowAlways), Effect::Unknown, "{bad:?}");
        }
        for good in ["Yes", "Yes, proceed", "Yes, and always allow access to /tmp from this project", "Yes, for this session"] {
            assert_eq!(option_effect(good, Effect::Allow), Effect::Allow, "{good:?}");
        }
        assert_eq!(option_effect("Yes, for this session", Effect::AllowAlways), Effect::AllowAlways);
    }

    #[test]
    fn a_deny_is_refused_when_its_label_approves_and_other_effects_are_untouched() {
        for bad in ["Yes", "\u{200b}Approve", "Allow once"] {
            assert_eq!(option_effect(bad, Effect::Deny), Effect::Unknown, "{bad:?}");
        }
        for good in ["No", "No, and tell Claude what to do differently", "Nope", "Deny"] {
            assert_eq!(option_effect(good, Effect::Deny), Effect::Deny, "{good:?}");
        }
        assert_eq!(option_effect("Anything", Effect::Choose), Effect::Choose);
    }

    #[test]
    fn every_peer_string_on_the_machine_is_cleaned() {
        let mut hostile = machine();
        hostile.name = "box\u{202E}".into();
        hostile.acd_version = "1\u{2066}".into();
        hostile.sources.hooks.message = Some("a\u{202E}b".into());
        hostile.sources.herdr.message = Some("c\u{2028}d".into());
        hostile.sources.t3.message = Some("e\u{1b}f".into());
        let all = |m: &Machine| {
            let mut v = vec![m.name.clone(), m.acd_version.clone()];
            v.extend([&m.sources.hooks, &m.sources.herdr, &m.sources.t3].map(|s| s.message.clone().unwrap()));
            v
        };
        let mut s = Session::default();
        s.welcome(hostile.clone(), vec![], "e1", 0);
        s.apply(&snap("e1", 1, &[]));
        for stored in all(s.machine.as_ref().unwrap()) {
            assert!(!acd_protocol::text::has_forbidden_chars(&stored), "welcome kept {stored:?}");
        }
        assert_eq!(s.machine.as_ref().unwrap().name, "box\\u{202e}");
        s.machine = None;
        s.apply(&ServerMessage::MachineUpdated { epoch: "e1".into(), seq: 2, machine: hostile });
        for stored in all(s.machine.as_ref().unwrap()) {
            assert!(!acd_protocol::text::has_forbidden_chars(&stored), "update kept {stored:?}");
        }
    }
}
