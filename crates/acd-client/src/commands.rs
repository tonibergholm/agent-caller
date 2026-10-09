use std::collections::HashMap;

use acd_protocol::{ClientMessage, CommandResult, ErrorCode, Outcome as ProtocolOutcome};
use tokio::{sync::oneshot, time::Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Answer {
        request_id: String,
        revision: u64,
        option_id: String,
    },
    Send {
        agent_id: String,
        text: String,
    },
    MarkSeen {
        agent_id: String,
    },
}

impl Command {
    pub fn capability(&self) -> &'static str {
        match self {
            Self::Answer { .. } => "answer",
            Self::Send { .. } => "send",
            Self::MarkSeen { .. } => "mark_seen",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Delivered,
    Done,
    Failed { code: ErrorCode, message: String },
    Uncertain,
    NotSent(NotSent),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotSent {
    Offline,
    NotAdvertised,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    Unknown,
    NotDelivered,
}

pub(crate) struct Inflight {
    pub cmd: Command,
    pub msg: ClientMessage,
    pub first_sent: Instant,
    pub retry_at: Option<Instant>,
    pub delivery: Delivery,
    pub cred: [u8; 32],
    pub reply: oneshot::Sender<Outcome>,
}

#[derive(Default)]
pub(crate) struct Commands {
    items: HashMap<String, Inflight>,
}

impl Commands {
    pub fn begin(
        &mut self,
        cmd: Command,
        reply: oneshot::Sender<Outcome>,
        caps: &[String],
        cred: [u8; 32],
        now: Instant,
    ) -> Option<ClientMessage> {
        if !caps.iter().any(|cap| cap == cmd.capability()) {
            let _ = reply.send(Outcome::NotSent(NotSent::NotAdvertised));
            return None;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let mut msg = message(&cmd, id.clone());
        if !caps.iter().any(|cap| cap == "answer.revision")
            && let ClientMessage::RequestAnswer { revision, .. } = &mut msg
        {
            *revision = None;
        }
        self.items.insert(
            id.clone(),
            Inflight {
                cmd,
                msg: msg.clone(),
                first_sent: now,
                retry_at: None,
                delivery: Delivery::Unknown,
                cred,
                reply,
            },
        );
        Some(msg)
    }

    pub fn on_result(&mut self, r: &CommandResult, now: Instant, t: &crate::Timing) {
        let Some(item) = self.items.get_mut(&r.id) else {
            return;
        };
        if r.ok {
            let outcome = if r.outcome == Some(ProtocolOutcome::Delivered) {
                Outcome::Delivered
            } else {
                Outcome::Done
            };
            self.finish(&r.id, outcome);
        } else {
            let code = r
                .error
                .as_ref()
                .map(|e| e.code)
                .unwrap_or(ErrorCode::BadRequest);
            let message = r
                .error
                .as_ref()
                .map(|e| e.message.clone())
                .unwrap_or_default();
            if code == ErrorCode::RequestNotReady {
                item.delivery = Delivery::NotDelivered;
                let next = now + t.not_ready_interval;
                if next <= item.first_sent + t.not_ready_window {
                    item.retry_at = Some(next);
                } else {
                    self.finish(&r.id, Outcome::Failed { code, message });
                }
            } else {
                self.finish(&r.id, Outcome::Failed { code, message });
            }
        }
    }

    pub fn due(&mut self, now: Instant, caps: &[String], t: &crate::Timing) -> Vec<ClientMessage> {
        let due: Vec<_> = self
            .items
            .iter()
            .filter(|(_, i)| i.retry_at.is_some_and(|at| at <= now))
            .map(|(id, _)| id.clone())
            .collect();
        let mut messages = Vec::new();
        for id in due {
            let expired = now > self.items[&id].first_sent + t.not_ready_window;
            let missing = !caps
                .iter()
                .any(|cap| cap == self.items[&id].cmd.capability());
            let missing_revision = frame_requires_revision(&self.items[&id].msg)
                && !caps.iter().any(|cap| cap == "answer.revision");
            if expired {
                self.finish(
                    &id,
                    Outcome::Failed {
                        code: ErrorCode::RequestNotReady,
                        message: ErrorCode::RequestNotReady.describe().into(),
                    },
                );
            } else if missing || missing_revision {
                self.finish(&id, Outcome::Uncertain);
            } else {
                let i = self.items.get_mut(&id).unwrap();
                i.retry_at = None;
                i.delivery = Delivery::Unknown;
                messages.push(i.msg.clone());
            }
        }
        messages
    }

    pub fn next_due(&self) -> Option<Instant> {
        self.items.values().filter_map(|i| i.retry_at).min()
    }

    pub fn on_welcome(
        &mut self,
        change: crate::session::EpochChange,
        caps: &[String],
        now: Instant,
        t: &crate::Timing,
    ) -> Vec<ClientMessage> {
        if change == crate::session::EpochChange::New {
            self.revoke_all();
            return Vec::new();
        }
        let ids: Vec<_> = self.items.keys().cloned().collect();
        let mut resend = Vec::new();
        for id in ids {
            let Some(i) = self.items.get(&id) else {
                continue;
            };
            if i.delivery == Delivery::NotDelivered {
                continue;
            }
            if now.saturating_duration_since(i.first_sent) > t.stale_inflight
                || !caps.iter().any(|cap| cap == i.cmd.capability())
                || (frame_requires_revision(&i.msg)
                    && !caps.iter().any(|cap| cap == "answer.revision"))
            {
                self.finish(&id, Outcome::Uncertain);
            } else {
                resend.push(i.msg.clone());
            }
        }
        let retry_ids: Vec<_> = self
            .items
            .iter()
            .filter(|(_, i)| i.delivery == Delivery::NotDelivered)
            .map(|(id, _)| id.clone())
            .collect();
        for id in retry_ids {
            let item = self.items.get(&id).unwrap();
            if now > item.first_sent + t.not_ready_window {
                self.finish(
                    &id,
                    Outcome::Failed {
                        code: ErrorCode::RequestNotReady,
                        message: ErrorCode::RequestNotReady.describe().into(),
                    },
                );
            } else if item.retry_at.is_none_or(|at| at <= now) {
                if !caps.iter().any(|cap| cap == item.cmd.capability())
                    || (frame_requires_revision(&item.msg)
                        && !caps.iter().any(|cap| cap == "answer.revision"))
                {
                    self.finish(&id, Outcome::Uncertain);
                } else {
                    let item = self.items.get_mut(&id).unwrap();
                    item.retry_at = None;
                    item.delivery = Delivery::Unknown;
                    resend.push(item.msg.clone());
                }
            }
        }
        resend
    }

    pub fn revoke_all(&mut self) {
        let ids: Vec<_> = self.items.keys().cloned().collect();
        for id in ids {
            self.finish(&id, Outcome::Uncertain);
        }
    }

    pub fn revoke_other_creds(&mut self, cred: [u8; 32]) {
        let ids: Vec<_> = self
            .items
            .iter()
            .filter(|(_, i)| i.cred != cred)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.finish(&id, Outcome::Uncertain);
        }
    }

    fn finish(&mut self, id: &str, outcome: Outcome) {
        if let Some(item) = self.items.remove(id) {
            let _ = item.reply.send(outcome);
        }
    }
}

fn message(cmd: &Command, id: String) -> ClientMessage {
    match cmd {
        Command::Answer {
            request_id,
            option_id,
            revision,
        } => ClientMessage::RequestAnswer {
            id,
            request_id: request_id.clone(),
            revision: Some(*revision),
            option_id: Some(option_id.clone()),
            text: None,
        },
        Command::Send { agent_id, text } => ClientMessage::AgentSend {
            id,
            agent_id: agent_id.clone(),
            text: text.clone(),
        },
        Command::MarkSeen { agent_id } => ClientMessage::AgentMarkSeen {
            id,
            agent_id: agent_id.clone(),
        },
    }
}

fn frame_requires_revision(msg: &ClientMessage) -> bool {
    matches!(
        msg,
        ClientMessage::RequestAnswer {
            revision: Some(_),
            ..
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn caps() -> Vec<String> {
        vec!["answer".into(), "send".into(), "mark_seen".into()]
    }
    fn t() -> crate::Timing {
        crate::Timing::default()
    }
    fn answer() -> Command {
        Command::Answer {
            request_id: "r".into(),
            revision: 7,
            option_id: "o".into(),
        }
    }
    fn not_ready(id: &str) -> CommandResult {
        CommandResult::err(id, ErrorCode::RequestNotReady, "m")
    }
    fn id_of(m: &ClientMessage) -> String {
        m.command_id().unwrap().to_owned()
    }
    const C1: [u8; 32] = [1; 32];
    const C2: [u8; 32] = [2; 32];

    #[test]
    fn a_sent_retry_is_unknown_again_and_never_expires_as_not_ready() {
        let t0 = Instant::now();
        let mut c = Commands::default();
        let (tx, mut rx) = oneshot::channel();
        let m = c.begin(answer(), tx, &caps(), C1, t0).unwrap();
        c.on_result(
            &not_ready(&id_of(&m)),
            t0 + Duration::from_millis(100),
            &t(),
        );
        assert_eq!(
            c.due(t0 + Duration::from_millis(350), &caps(), &t()),
            vec![m.clone()]
        );
        assert_eq!(
            c.on_welcome(
                crate::session::EpochChange::Same,
                &caps(),
                t0 + Duration::from_secs(10),
                &t()
            ),
            vec![m]
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn answer_frame_includes_revision_only_for_advertising_servers() {
        let t0 = Instant::now();
        let mut c = Commands::default();
        let (tx, _rx) = oneshot::channel();
        let frame = c
            .begin(
                answer(),
                tx,
                &["answer".into(), "answer.revision".into()],
                C1,
                t0,
            )
            .unwrap();
        assert!(matches!(
            frame,
            ClientMessage::RequestAnswer {
                revision: Some(7),
                ..
            }
        ));
        let (tx, _rx) = oneshot::channel();
        let legacy = c.begin(answer(), tx, &["answer".into()], C1, t0).unwrap();
        assert!(matches!(
            legacy,
            ClientMessage::RequestAnswer { revision: None, .. }
        ));
    }

    #[test]
    fn request_changed_finishes_without_retry() {
        let t0 = Instant::now();
        let mut c = Commands::default();
        let (tx, mut rx) = oneshot::channel();
        let msg = c
            .begin(
                answer(),
                tx,
                &["answer".into(), "answer.revision".into()],
                C1,
                t0,
            )
            .unwrap();
        c.on_result(
            &CommandResult::err(
                &id_of(&msg),
                ErrorCode::RequestChanged,
                ErrorCode::RequestChanged.describe(),
            ),
            t0,
            &t(),
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Outcome::Failed {
                code: ErrorCode::RequestChanged,
                message: ErrorCode::RequestChanged.describe().into()
            }
        );
        assert!(
            c.due(
                t0 + Duration::from_secs(1),
                &["answer".into(), "answer.revision".into()],
                &t()
            )
            .is_empty()
        );
    }

    #[test]
    fn on_welcome_sends_due_not_delivered_retries_and_keeps_future_retries_waiting() {
        let t0 = Instant::now();
        let mut c = Commands::default();
        let (tx, _rx) = oneshot::channel();
        let m = c.begin(answer(), tx, &caps(), C1, t0).unwrap();
        c.on_result(&not_ready(&id_of(&m)), t0, &t());
        assert!(
            c.on_welcome(
                crate::session::EpochChange::Same,
                &caps(),
                t0 + Duration::from_millis(100),
                &t()
            )
            .is_empty()
        );
        assert_eq!(
            c.on_welcome(
                crate::session::EpochChange::Same,
                &caps(),
                t0 + Duration::from_millis(250),
                &t()
            ),
            vec![m]
        );
    }

    #[test]
    fn a_changed_credential_revokes_older_commands() {
        let t0 = Instant::now();
        let mut c = Commands::default();
        let (tx1, mut rx1) = oneshot::channel();
        let (tx2, mut rx2) = oneshot::channel();
        c.begin(answer(), tx1, &caps(), C1, t0).unwrap();
        c.begin(answer(), tx2, &caps(), C2, t0).unwrap();
        c.revoke_other_creds(C2);
        assert_eq!(rx1.try_recv().unwrap(), Outcome::Uncertain);
        assert!(rx2.try_recv().is_err());
        assert_eq!(
            c.on_welcome(crate::session::EpochChange::Same, &caps(), t0, &t())
                .len(),
            1
        );
    }

    #[test]
    fn overdue_retry_after_disconnect_fails_without_sending() {
        let t0 = Instant::now();
        let mut c = Commands::default();
        let (tx, mut rx) = oneshot::channel();
        let m = c.begin(answer(), tx, &caps(), C1, t0).unwrap();
        c.on_result(
            &not_ready(&id_of(&m)),
            t0 + Duration::from_millis(100),
            &t(),
        );
        assert!(
            c.due(t0 + Duration::from_secs(10), &caps(), &t())
                .is_empty()
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            Outcome::Failed {
                code: ErrorCode::RequestNotReady,
                ..
            }
        ));
    }

    #[test]
    fn retry_inside_the_window_reuses_the_message() {
        let t0 = Instant::now();
        let mut c = Commands::default();
        let (tx, _rx) = oneshot::channel();
        let m = c.begin(answer(), tx, &caps(), C1, t0).unwrap();
        c.on_result(
            &not_ready(&id_of(&m)),
            t0 + Duration::from_millis(100),
            &t(),
        );
        assert!(
            c.due(t0 + Duration::from_millis(300), &caps(), &t())
                .is_empty()
        );
        assert_eq!(
            c.due(t0 + Duration::from_millis(350), &caps(), &t()),
            vec![m]
        );
    }

    #[test]
    fn reconnect_after_the_window_fails_known_not_delivered() {
        let t0 = Instant::now();
        let mut c = Commands::default();
        let (tx, mut rx) = oneshot::channel();
        let m = c.begin(answer(), tx, &caps(), C1, t0).unwrap();
        c.on_result(&not_ready(&id_of(&m)), t0, &t());
        assert!(
            c.on_welcome(
                crate::session::EpochChange::Same,
                &caps(),
                t0 + Duration::from_secs(5),
                &t()
            )
            .is_empty()
        );
        assert!(matches!(
            rx.try_recv().unwrap(),
            Outcome::Failed {
                code: ErrorCode::RequestNotReady,
                ..
            }
        ));
    }

    #[test]
    fn unknown_result_is_resent_identically_in_the_same_epoch() {
        let t0 = Instant::now();
        let mut c = Commands::default();
        let (tx, _rx) = oneshot::channel();
        let m = c.begin(answer(), tx, &caps(), C1, t0).unwrap();
        assert_eq!(
            c.on_welcome(
                crate::session::EpochChange::Same,
                &caps(),
                t0 + Duration::from_secs(1),
                &t()
            ),
            vec![m]
        );
    }

    #[test]
    fn capability_lost_before_resend_is_uncertain_and_not_sent() {
        let t0 = Instant::now();
        let mut c = Commands::default();
        let (tx, mut rx) = oneshot::channel();
        c.begin(answer(), tx, &caps(), C1, t0).unwrap();
        assert!(
            c.on_welcome(
                crate::session::EpochChange::Same,
                &["send".into()],
                t0,
                &t()
            )
            .is_empty()
        );
        assert_eq!(rx.try_recv().unwrap(), Outcome::Uncertain);
    }

    #[test]
    fn revision_capability_lost_before_resend_finishes_without_rewriting() {
        let t0 = Instant::now();
        let mut c = Commands::default();
        let revision_caps = ["answer".into(), "answer.revision".into()];
        let answer_only = ["answer".into()];

        let (tx, mut rx) = oneshot::channel();
        let msg = c.begin(answer(), tx, &revision_caps, C1, t0).unwrap();
        assert!(matches!(
            msg,
            ClientMessage::RequestAnswer {
                revision: Some(7),
                ..
            }
        ));
        assert!(
            c.on_welcome(crate::session::EpochChange::Same, &answer_only, t0, &t())
                .is_empty()
        );
        assert_eq!(rx.try_recv().unwrap(), Outcome::Uncertain);

        let (tx, mut rx) = oneshot::channel();
        let msg = c.begin(answer(), tx, &revision_caps, C1, t0).unwrap();
        c.on_result(&not_ready(&id_of(&msg)), t0, &t());
        assert!(
            c.on_welcome(
                crate::session::EpochChange::Same,
                &answer_only,
                t0 + Duration::from_millis(250),
                &t()
            )
            .is_empty()
        );
        assert_eq!(rx.try_recv().unwrap(), Outcome::Uncertain);

        let (tx, mut rx) = oneshot::channel();
        let msg = c.begin(answer(), tx, &revision_caps, C1, t0).unwrap();
        c.on_result(&not_ready(&id_of(&msg)), t0, &t());
        assert!(
            c.due(t0 + Duration::from_millis(250), &answer_only, &t())
                .is_empty()
        );
        assert_eq!(rx.try_recv().unwrap(), Outcome::Uncertain);
    }

    #[test]
    fn new_epoch_and_revocation_are_uncertain() {
        let t0 = Instant::now();
        let mut c = Commands::default();
        let (tx1, mut rx1) = oneshot::channel();
        let (tx2, mut rx2) = oneshot::channel();
        c.begin(answer(), tx1, &caps(), C1, t0).unwrap();
        assert!(
            c.on_welcome(crate::session::EpochChange::New, &caps(), t0, &t())
                .is_empty()
        );
        assert_eq!(rx1.try_recv().unwrap(), Outcome::Uncertain);
        c.begin(answer(), tx2, &caps(), C1, t0).unwrap();
        c.revoke_all();
        assert_eq!(rx2.try_recv().unwrap(), Outcome::Uncertain);
    }

    #[test]
    fn missing_capability_at_begin_is_not_sent() {
        let (tx, mut rx) = oneshot::channel();
        assert!(
            Commands::default()
                .begin(
                    Command::Send {
                        agent_id: "a".into(),
                        text: "x".into()
                    },
                    tx,
                    &["answer".into()],
                    C1,
                    Instant::now()
                )
                .is_none()
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            Outcome::NotSent(NotSent::NotAdvertised)
        );
    }
}
