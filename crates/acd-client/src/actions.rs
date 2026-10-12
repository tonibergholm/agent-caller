use std::fmt;
use std::time::Instant;

use acd_protocol::{AnswerPath, Effect, Request, RequestKind, RequestOption, RequestStatus};

use crate::{
    conn::{MachineState, MachineStatus},
    inbox::PickError,
    view::{ActionView, Notice, NoticeTone},
    voice::NotSentReason,
};

/// Request text is the agent's (voice spec §3.7): `Debug` shows lengths.
#[derive(Clone, PartialEq, Eq)]
pub struct Semantics {
    pub request_id: String,
    pub revision: u64,
    pub kind: RequestKind,
    pub answer_path: AnswerPath,
    pub title: String,
    pub detail: String,
    pub option: RequestOption,
    pub option_ids: Vec<String>,
}
impl Semantics {
    pub fn of(req: &Request, option_id: &str) -> Option<Self> {
        let option = req.options.iter().find(|o| o.id == option_id)?.clone();
        Some(Self {
            request_id: req.id.clone(),
            revision: req.revision,
            kind: req.kind,
            answer_path: req.answer_path,
            title: req.title.clone(),
            detail: req.detail.clone(),
            option,
            option_ids: req.options.iter().map(|o| o.id.clone()).collect(),
        })
    }
}
/// What a typed answer was made against; `check` re-reads the request and
/// refuses to send if any of it changed.
#[derive(Clone, PartialEq, Eq)]
pub struct TextSemantics {
    pub request_id: String,
    pub revision: u64,
    pub kind: RequestKind,
    pub answer_path: AnswerPath,
    pub title: String,
    pub detail: String,
    pub text: String,
}
impl TextSemantics {
    pub fn of(req: &Request, text: &str) -> Self {
        Self {
            request_id: req.id.clone(),
            revision: req.revision,
            kind: req.kind,
            answer_path: req.answer_path,
            title: req.title.clone(),
            detail: req.detail.clone(),
            text: text.to_owned(),
        }
    }
}
#[derive(Clone, PartialEq, Eq)]
pub enum ActionKind {
    Answer(Semantics),
    AnswerText(TextSemantics),
    Reply { text: String },
}
impl fmt::Debug for Semantics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Semantics")
            .field("request_id", &self.request_id)
            .field("revision", &self.revision)
            .field("kind", &self.kind)
            .field("answer_path", &self.answer_path)
            .field("title_bytes", &self.title.len())
            .field("detail_bytes", &self.detail.len())
            .field("option_id", &self.option.id)
            .field("option_effect", &self.option.effect)
            .field("option_ids", &self.option_ids)
            .finish_non_exhaustive()
    }
}
/// Typed text is the user's (voice spec §3.7): `Debug` shows its length.
impl fmt::Debug for TextSemantics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TextSemantics")
            .field("request_id", &self.request_id)
            .field("revision", &self.revision)
            .field("kind", &self.kind)
            .field("text_bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ActionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionKind::Answer(s) => f.debug_tuple("Answer").field(s).finish(),
            ActionKind::AnswerText(s) => f.debug_tuple("AnswerText").field(s).finish(),
            ActionKind::Reply { text } => f.debug_struct("Reply").field("text_bytes", &text.len()).finish(),
        }
    }
}
/// Who started an action (omarchy spec §6). The Mac app and voice use
/// `DEFAULT`; acd-inbox gives each socket connection its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ActionOwner(pub u64);

impl ActionOwner {
    pub const DEFAULT: ActionOwner = ActionOwner(0);
}

#[derive(Clone, Debug)]
pub struct Action {
    pub id: String,
    pub owner: ActionOwner,
    pub card_key: String,
    pub machine_key: String,
    pub agent_id: String,
    pub instance: u64,
    pub conn_gen: u64,
    pub kind: ActionKind,
    pub fires_at: Instant,
    pub view: ActionView,
}
#[derive(Default)]
pub struct Actions {
    items: Vec<Action>,
}
impl Actions {
    pub fn add(&mut self, a: Action) -> Result<(), PickError> {
        if self.items.iter().any(|item| item.card_key == a.card_key) {
            return Err(PickError::AlreadyCounting);
        }
        self.items.push(a);
        Ok(())
    }
    pub fn cancel(&mut self, id: &str) -> Option<Action> {
        self.items
            .iter()
            .position(|a| a.id == id)
            .map(|i| self.items.remove(i))
    }
    pub fn cancel_latest(&mut self) -> Option<Action> {
        self.items.pop()
    }
    /// The newest unsent action of `owner`.
    pub fn cancel_latest_of(&mut self, owner: ActionOwner) -> Option<Action> {
        let i = self.items.iter().rposition(|a| a.owner == owner)?;
        Some(self.items.remove(i))
    }
    /// Every unsent action of `owner`, oldest first.
    pub fn cancel_owned(&mut self, owner: ActionOwner) -> Vec<Action> {
        self.remove_where(|a| a.owner == owner)
    }
    pub fn due(&mut self, now: Instant) -> Vec<Action> {
        let mut due = Vec::new();
        let mut i = 0;
        while i < self.items.len() {
            if self.items[i].fires_at <= now {
                due.push(self.items.remove(i));
            } else {
                i += 1;
            }
        }
        due
    }
    pub fn end_generation(&mut self, machine_key: &str, generation: u64) -> Vec<Action> {
        self.remove_where(|a| a.machine_key == machine_key && a.conn_gen == generation)
    }
    pub fn end_machine(&mut self, machine_key: &str) -> Vec<Action> {
        self.remove_where(|a| a.machine_key == machine_key)
    }
    pub(crate) fn remove_where(&mut self, mut f: impl FnMut(&Action) -> bool) -> Vec<Action> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.items.len() {
            if f(&self.items[i]) {
                out.push(self.items.remove(i));
            } else {
                i += 1;
            }
        }
        out
    }
    pub(crate) fn remove_invalid(
        &mut self,
        machine_key: &str,
        mut invalid: impl FnMut(&Action) -> Option<NotSentReason>,
    ) -> Vec<(Action, NotSentReason)> {
        let mut removed = Vec::new();
        let mut i = 0;
        while i < self.items.len() {
            if self.items[i].machine_key == machine_key
                && let Some(reason) = invalid(&self.items[i])
            {
                removed.push((self.items.remove(i), reason));
                continue;
            }
            i += 1;
        }
        removed
    }

    /// Test hook: every countdown ends now.
    #[cfg(feature = "testing")]
    pub(crate) fn expire_all(&mut self, now: Instant) {
        for a in &mut self.items {
            a.fires_at = now;
        }
    }
    pub fn views(&self) -> Vec<ActionView> {
        self.items.iter().map(|a| a.view.clone()).collect()
    }
    pub fn next_due(&self) -> Option<Instant> {
        self.items.iter().map(|a| a.fires_at).min()
    }
}

fn notice(text: &str, tone: NoticeTone) -> Notice {
    Notice {
        text: text.into(),
        tone,
    }
}
/// The card notice for an action that ended without being sent.
pub fn notice_for(reason: NotSentReason) -> Notice {
    match reason {
        NotSentReason::Offline => notice("Offline. Nothing was sent.", NoticeTone::Warning),
        NotSentReason::NotSupported => notice("Not supported for this agent", NoticeTone::Info),
        NotSentReason::AgentGone => notice("The agent is gone", NoticeTone::Info),
        NotSentReason::AnsweredElsewhere => notice("Answered elsewhere", NoticeTone::Info),
        NotSentReason::Changed => notice("The request changed. Nothing was sent.", NoticeTone::Warning),
        NotSentReason::AgentBusy => notice("The agent is busy. Try again when it is idle.", NoticeTone::Warning),
        NotSentReason::Hidden => notice("Hidden. Nothing was sent.", NoticeTone::Info),
    }
}

pub fn check(a: &Action, state: &MachineState) -> Result<(), Notice> {
    check_reason(a, state).map_err(notice_for)
}

pub fn check_reason(a: &Action, state: &MachineState) -> Result<(), NotSentReason> {
    if state.instance != a.instance
        || state.conn_gen != a.conn_gen
        || state.status != MachineStatus::Online
    {
        return Err(NotSentReason::Offline);
    }
    let cap = match a.kind {
        ActionKind::Answer(_) | ActionKind::AnswerText(_) => "answer",
        ActionKind::Reply { .. } => "send",
    };
    if !state.session.has(cap) {
        return Err(NotSentReason::NotSupported);
    }
    let Some(agent) = state.session.agents.get(&a.agent_id) else {
        return Err(NotSentReason::AgentGone);
    };
    match &a.kind {
        ActionKind::Answer(sem) => {
            let Some(req) = agent.pending.as_ref() else {
                return Err(NotSentReason::AnsweredElsewhere);
            };
            if req.id != sem.request_id || req.status != RequestStatus::Open {
                return Err(NotSentReason::AnsweredElsewhere);
            }
            if !req.is_answerable()
                || Semantics::of(req, &sem.option.id).as_ref() != Some(sem)
                || sem.option.effect == Effect::Unknown
            {
                return Err(NotSentReason::Changed);
            }
        }
        ActionKind::AnswerText(sem) => {
            let Some(req) = agent.pending.as_ref() else {
                return Err(NotSentReason::AnsweredElsewhere);
            };
            if req.id != sem.request_id || req.status != RequestStatus::Open {
                return Err(NotSentReason::AnsweredElsewhere);
            }
            if !req.is_answerable() || !req.free_text || TextSemantics::of(req, &sem.text) != *sem {
                return Err(NotSentReason::Changed);
            }
        }
        ActionKind::Reply { .. } if !agent.can_send => {
            return Err(NotSentReason::AgentBusy);
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{inbox::PickError, session::Session};
    use acd_protocol::*;
    use std::time::Duration;
    fn action(id: &str, card: &str, generation: u64, fires_at: Instant) -> Action {
        Action {
            id: id.into(),
            owner: ActionOwner::DEFAULT,
            card_key: card.into(),
            machine_key: "m".into(),
            agent_id: "a".into(),
            instance: 1,
            conn_gen: generation,
            kind: ActionKind::Reply { text: "hi".into() },
            fires_at,
            view: ActionView {
                id: id.into(),
                card_key: card.into(),
                label: "Reply".into(),
                detail: None,
                fires_at_ms: 0.,
            },
        }
    }
    #[test]
    fn due_exactly_at_the_boundary() {
        let t = Instant::now();
        let mut a = Actions::default();
        a.add(action("x", "m/a", 1, t + Duration::from_secs(2)))
            .unwrap();
        assert!(a.due(t + Duration::from_millis(1999)).is_empty());
        assert_eq!(a.due(t + Duration::from_secs(2)).len(), 1);
    }
    #[test]
    fn one_per_card_and_escape_takes_the_newest() {
        let t = Instant::now();
        let mut a = Actions::default();
        a.add(action("x", "m/a", 1, t)).unwrap();
        assert_eq!(
            a.add(action("y", "m/a", 1, t)),
            Err(PickError::AlreadyCounting)
        );
        a.add(action("z", "m/b", 1, t)).unwrap();
        assert_eq!(a.cancel_latest().unwrap().id, "z");
        assert_eq!(a.cancel_latest().unwrap().id, "x");
        assert!(a.cancel_latest().is_none());
    }
    #[test]
    fn ending_a_generation_removes_only_its_actions() {
        let t = Instant::now();
        let mut a = Actions::default();
        a.add(action("x", "m/a", 1, t)).unwrap();
        a.add(action("y", "m/b", 2, t)).unwrap();
        assert_eq!(
            a.end_generation("m", 1)
                .iter()
                .map(|x| x.id.as_str())
                .collect::<Vec<_>>(),
            ["x"]
        );
        assert_eq!(a.views().len(), 1);
    }
    fn req() -> Request {
        serde_json::from_value(serde_json::json!({"id":"r","agent_id":"a","kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"npm test","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"},{"id":"always","label":"Always","description":"Bash(npm test:*)","effect":"allow_always"}],"questions":null,"free_text":false,"raw_screen":null,"opened_at":"2026-10-08T00:00:00Z"})).unwrap()
    }
    fn state() -> MachineState {
        let a:Agent=serde_json::from_value(serde_json::json!({"id":"a","machine_id":"m","source":"herdr","kind":"claude","title":"agent","cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00Z","failed":false,"last_message":null,"can_send":true,"pending":serde_json::to_value(req()).unwrap()})).unwrap();
        let mut s = Session::default();
        let machine:Machine=serde_json::from_value(serde_json::json!({"id":"m","name":"M","acd_version":"0","sources":{"hooks":{"state":"ok"},"herdr":{"state":"ok"},"t3":{"state":"ok"}}})).unwrap();
        s.welcome(
            machine,
            ["answer", "send"].into_iter().map(str::to_owned).collect(),
            "e",
            1,
        );
        s.apply(&ServerMessage::Snapshot {
            epoch: "e".into(),
            seq: 1,
            agents: vec![a],
        });
        MachineState {
            instance: 1,
            status: MachineStatus::Online,
            session: s,
            conn_gen: 1,
        }
    }
    fn answer_action() -> Action {
        let mut a = action("x", "m/a", 1, Instant::now());
        a.kind = ActionKind::Answer(Semantics::of(&req(), "always").unwrap());
        a
    }
    #[test]
    fn check_rejects_every_semantic_change() {
        let a = answer_action();
        let base = state();
        assert_eq!(check(&a, &base), Ok(()));
        let mut s = base.clone();
        s.conn_gen = 2;
        assert_eq!(
            check(&a, &s).unwrap_err().text,
            "Offline. Nothing was sent."
        );
        let mut s = base.clone();
        s.instance = 2;
        assert_eq!(
            check(&a, &s).unwrap_err().text,
            "Offline. Nothing was sent."
        );
        let mut s = base.clone();
        s.status = MachineStatus::Syncing;
        assert_eq!(
            check(&a, &s).unwrap_err().text,
            "Offline. Nothing was sent."
        );
        let mut s = base.clone();
        s.session.capabilities.clear();
        assert_eq!(
            check(&a, &s).unwrap_err().text,
            "Not supported for this agent"
        );
        let mut s = base.clone();
        s.session
            .agents
            .get_mut("a")
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .status = RequestStatus::Answering;
        assert_eq!(check(&a, &s).unwrap_err().text, "Answered elsewhere");
        let mut s = base.clone();
        s.session
            .agents
            .get_mut("a")
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .id = "other".into();
        assert_eq!(check(&a, &s).unwrap_err().text, "Answered elsewhere");
        for change in 0..4 {
            let mut s = base.clone();
            let r = s
                .session
                .agents
                .get_mut("a")
                .unwrap()
                .pending
                .as_mut()
                .unwrap();
            match change {
                0 => r.options[1].description = Some("Bash(rm:*)".into()),
                1 => r.options[1].effect = Effect::Unknown,
                2 => r.answer_path = AnswerPath::None,
                _ => r.detail = "other".into(),
            }
            assert_eq!(
                check(&a, &s).unwrap_err().text,
                "The request changed. Nothing was sent."
            );
        }
    }
    #[test]
    fn owners_cancel_only_their_own() {
        let t = Instant::now();
        let mut a = Actions::default();
        let mut x = action("x", "m/a", 1, t);
        x.owner = ActionOwner(1);
        let mut y = action("y", "m/b", 1, t);
        y.owner = ActionOwner(2);
        let mut z = action("z", "m/c", 1, t);
        z.owner = ActionOwner(1);
        a.add(x).unwrap();
        a.add(y).unwrap();
        a.add(z).unwrap();
        assert_eq!(a.cancel_latest_of(ActionOwner(2)).unwrap().id, "y");
        assert!(a.cancel_latest_of(ActionOwner(2)).is_none());
        let ids: Vec<String> = a.cancel_owned(ActionOwner(1)).into_iter().map(|x| x.id).collect();
        assert_eq!(ids, ["x", "z"]);
        assert!(a.views().is_empty());
    }
}
