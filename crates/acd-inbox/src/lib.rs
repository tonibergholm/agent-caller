//! acd-inbox: the agent-caller inbox as a local service for Omarchy
//! (omarchy spec §4). Views connect over a Unix socket and speak JSON lines.
pub mod protocol;
pub mod prefs;
pub mod startup;
pub mod outbox;
