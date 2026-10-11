//! `InboxPort` over acd-client's `Inbox`: one guarded delegate per
//! operation. acd-client checks the guard on the live machine state.
use std::sync::Arc;

use acd_client::inbox::{Inbox, PickError};
use acd_client::voice::{ActionOutcome, ConnStamp as ClientStamp, Guard as ClientGuard, NotSentReason as ClientReason};
use acd_voice::plan::{Enqueue, EnqueueError, Guard, InboxPort, NotSentReason, VoiceOutcome};

pub struct AcdPort {
    inbox: Arc<Inbox>,
}

impl AcdPort {
    pub fn new(inbox: Arc<Inbox>) -> Self {
        Self { inbox }
    }
}

impl InboxPort for AcdPort {
    fn enqueue(&self, op: Enqueue) -> Result<String, EnqueueError> {
        let result = match op {
            Enqueue::Pick { card, option_id, guard } => self.inbox.pick_guarded(&card, &option_id, &client(&guard)),
            Enqueue::AnswerText { card, text, guard } => {
                self.inbox.answer_text_guarded(&card, text.expose().to_owned(), &client(&guard))
            }
            Enqueue::Reply { card, text, guard } => {
                self.inbox.reply_guarded(&card, text.expose().to_owned(), &client(&guard))
            }
        };
        result.map_err(enqueue_error)
    }

    fn cancel(&self, action_id: &str) -> bool {
        self.inbox.cancel(action_id)
    }
}

fn client(g: &Guard) -> ClientGuard {
    ClientGuard {
        conn: ClientStamp { instance: g.conn.instance, conn_gen: g.conn.conn_gen },
        agent_id: g.agent_id.clone(),
        request: g.request.clone(),
    }
}

fn enqueue_error(e: PickError) -> EnqueueError {
    match e {
        PickError::UnknownCard => EnqueueError::UnknownCard,
        PickError::NotAnswerable => EnqueueError::NotAnswerable,
        PickError::StaleRequest | PickError::UnknownOption => EnqueueError::Stale,
        PickError::AlreadyCounting => EnqueueError::AlreadyCounting,
        PickError::BadText | PickError::BadAnswerText => EnqueueError::BadText,
        PickError::CannotReply => EnqueueError::CannotReply,
        PickError::ConnectionChanged => EnqueueError::ConnectionChanged,
    }
}

fn reason(r: ClientReason) -> NotSentReason {
    match r {
        ClientReason::Offline => NotSentReason::Offline,
        ClientReason::NotSupported => NotSentReason::NotSupported,
        ClientReason::AgentGone => NotSentReason::AgentGone,
        ClientReason::AnsweredElsewhere => NotSentReason::AnsweredElsewhere,
        ClientReason::Changed => NotSentReason::Changed,
        ClientReason::AgentBusy => NotSentReason::AgentBusy,
        ClientReason::Hidden => NotSentReason::Hidden,
    }
}

pub fn voice_outcome(o: ActionOutcome) -> VoiceOutcome {
    match o {
        ActionOutcome::Fired => VoiceOutcome::Fired,
        ActionOutcome::Delivered => VoiceOutcome::Delivered,
        ActionOutcome::Rejected { code } => VoiceOutcome::Rejected { code },
        ActionOutcome::Uncertain => VoiceOutcome::Uncertain,
        ActionOutcome::Cancelled => VoiceOutcome::Cancelled,
        ActionOutcome::NotSent { reason: r } => VoiceOutcome::NotSent { reason: reason(r) },
    }
}
