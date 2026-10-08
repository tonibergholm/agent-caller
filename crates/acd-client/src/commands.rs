use acd_protocol::ErrorCode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Answer {
        request_id: String,
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
