use std::time::Instant;

use acd_protocol::{AnswerPath, Effect, Request, RequestKind, RequestOption, RequestStatus};

use crate::{
    conn::{MachineState, MachineStatus},
    inbox::PickError,
    view::{ActionView, Notice, NoticeTone},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Semantics {
    pub request_id: String,
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
            kind: req.kind,
            answer_path: req.answer_path,
            title: req.title.clone(),
            detail: req.detail.clone(),
            option,
            option_ids: req.options.iter().map(|o| o.id.clone()).collect(),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionKind {
    Answer(Semantics),
    Reply { text: String },
}
#[derive(Clone, Debug)]
pub struct Action {
    pub id: String,
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
    fn remove_where(&mut self, mut f: impl FnMut(&Action) -> bool) -> Vec<Action> {
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
        mut invalid: impl FnMut(&Action) -> Option<Notice>,
    ) -> Vec<(String, Notice)> {
        let mut removed = Vec::new();
        let mut i = 0;
        while i < self.items.len() {
            if self.items[i].machine_key == machine_key
                && let Some(n) = invalid(&self.items[i])
            {
                let a = self.items.remove(i);
                removed.push((a.card_key, n));
                continue;
            }
            i += 1;
        }
        removed
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
pub fn check(a: &Action, state: &MachineState) -> Result<(), Notice> {
    if state.instance != a.instance
        || state.conn_gen != a.conn_gen
        || state.status != MachineStatus::Online
    {
        return Err(notice("Offline. Nothing was sent.", NoticeTone::Warning));
    }
    let cap = match a.kind {
        ActionKind::Answer(_) => "answer",
        ActionKind::Reply { .. } => "send",
    };
    if !state.session.has(cap) {
        return Err(notice("Not supported for this agent", NoticeTone::Info));
    }
    let Some(agent) = state.session.agents.get(&a.agent_id) else {
        return Err(notice("The agent is gone", NoticeTone::Info));
    };
    match &a.kind {
        ActionKind::Answer(sem) => {
            let Some(req) = agent.pending.as_ref() else {
                return Err(notice("Answered elsewhere", NoticeTone::Info));
            };
            if req.id != sem.request_id || req.status != RequestStatus::Open {
                return Err(notice("Answered elsewhere", NoticeTone::Info));
            }
            if !req.is_answerable()
                || Semantics::of(req, &sem.option.id).as_ref() != Some(sem)
                || sem.option.effect == Effect::Unknown
            {
                return Err(notice(
                    "The request changed. Nothing was sent.",
                    NoticeTone::Warning,
                ));
            }
        }
        ActionKind::Reply { .. } if !agent.can_send => {
            return Err(notice(
                "The agent is busy. Try again when it is idle.",
                NoticeTone::Warning,
            ));
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
}
