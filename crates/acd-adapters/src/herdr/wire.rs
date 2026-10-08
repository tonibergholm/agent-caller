//! herdr wire types (herdr 0.9.3, protocol 22). Unknown fields are ignored.
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    #[default]
    #[serde(other)]
    Unknown,
}

/// herdr's agent session handle. P3 binds sessions by `value`.
#[derive(Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentSession {
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct AgentInfo {
    pub pane_id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub state_change_seq: u64,
    #[serde(default)]
    pub agent_session: Option<AgentSession>,
}

#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Pong {
    pub version: String,
    pub protocol: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Created {
        pane_id: String,
    },
    Closed {
        pane_id: String,
    },
    AgentDetected {
        pane_id: String,
    },
    AgentReleased {
        pane_id: String,
    },
    Updated {
        pane_id: String,
    },
    StatusChanged {
        pane_id: String,
        status: AgentStatus,
    },
    Other,
}

/// Frame names differ from subscription names (gate 6): underscores for
/// most, dotted for status changes. Anything unexpected is `Other`, which
/// the reconciler treats as a resync hint.
pub fn parse_event(line: &str) -> Event {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Event::Other;
    };
    let data = &v["data"];
    let pane = |v: &Value| v.as_str().map(str::to_owned);
    match v["event"].as_str() {
        Some("pane_created") => {
            pane(&data["pane"]["pane_id"]).map(|pane_id| Event::Created { pane_id })
        }
        Some("pane_closed") => pane(&data["pane_id"]).map(|pane_id| Event::Closed { pane_id }),
        Some("pane_agent_detected") => pane(&data["pane_id"]).map(|pane_id| {
            if data["released"].as_bool() == Some(true) {
                Event::AgentReleased { pane_id }
            } else {
                Event::AgentDetected { pane_id }
            }
        }),
        Some("pane_updated") => {
            pane(&data["pane"]["pane_id"]).map(|pane_id| Event::Updated { pane_id })
        }
        Some("pane.agent_status_changed") => {
            pane(&data["pane_id"]).map(|pane_id| Event::StatusChanged {
                pane_id,
                status: serde_json::from_value(data["agent_status"].clone())
                    .unwrap_or(AgentStatus::Unknown),
            })
        }
        _ => None,
    }
    .unwrap_or(Event::Other)
}
