//! Maps herdr wire types to registry observations (spec §6.4).
use std::path::Path;

use acd_core::AgentObs;
use acd_protocol::{AgentKind, AgentState, Source};
use sha2::{Digest, Sha256};

use super::wire;
use crate::text::{TITLE_CHARS, clean_display};

/// 12 lowercase hex chars of `sha256(path)`; namespaces agent ids per herdr socket.
pub fn socket_hash(path: &Path) -> String {
    let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// Pane ids contain colons (`w1:p7`), so ids are never parsed back; callers
/// keep a map from agent id to `(socket, pane_id)`.
pub fn agent_id(socket_hash: &str, pane_id: &str) -> String {
    format!("herdr:{socket_hash}:{pane_id}")
}

/// `None` for a pane with no detected agent.
pub fn kind(agent: Option<&str>) -> Option<AgentKind> {
    agent.map(|a| match a {
        "claude" => AgentKind::Claude,
        "codex" => AgentKind::Codex,
        _ => AgentKind::Other,
    })
}

pub fn state(s: wire::AgentStatus) -> AgentState {
    match s {
        wire::AgentStatus::Blocked => AgentState::NeedsYou,
        wire::AgentStatus::Working => AgentState::Working,
        wire::AgentStatus::Done => AgentState::Done,
        wire::AgentStatus::Idle => AgentState::Idle,
        wire::AgentStatus::Unknown => AgentState::Unknown,
    }
}

pub fn agent_obs(socket_hash: &str, info: &wire::AgentInfo) -> Option<AgentObs> {
    let kind = kind(info.agent.as_deref())?;
    let title = [&info.name, &info.title, &info.terminal_title_stripped]
        .into_iter()
        .flatten()
        .map(|s| clean_display(s, TITLE_CHARS))
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| clean_display(&info.pane_id, TITLE_CHARS));
    let cwd = info
        .cwd
        .as_deref()
        .map(|c| clean_display(c, usize::MAX))
        .filter(|c| !c.is_empty());
    let project = cwd
        .as_deref()
        .and_then(|c| Path::new(c).file_name())
        .map(|n| clean_display(&n.to_string_lossy(), TITLE_CHARS))
        .filter(|p| !p.is_empty());
    Some(AgentObs {
        id: agent_id(socket_hash, &info.pane_id),
        source: Source::Herdr,
        kind,
        title,
        cwd,
        project,
        state: state(info.agent_status),
        failed: false,
        last_message: None,
        // herdr refuses agent.prompt while blocked; the core rejects send while needs_you.
        can_send: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(v: serde_json::Value) -> wire::AgentInfo {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn ids_are_namespaced_and_never_parsed() {
        let h = socket_hash(Path::new("/Users/user/.config/herdr/herdr.sock"));
        assert_eq!(h.len(), 12);
        assert_eq!(h, "15f4d901f9d1");
        assert!(
            h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(
            h,
            socket_hash(Path::new(
                "/Users/user/.config/herdr/sessions/p0/herdr.sock"
            ))
        );
        assert_eq!(agent_id(&h, "w1:p7"), format!("herdr:{h}:w1:p7"));
    }

    #[test]
    fn maps_states_kinds_and_titles() {
        let base = serde_json::json!({
            "pane_id": "w1:p2", "terminal_id": "t1", "workspace_id": "w1", "tab_id": "w1:t1",
            "focused": false, "revision": 3, "agent_status": "blocked", "agent": "claude",
            "cwd": "/Users/user/src/app", "title": "fix tests", "some_future_field": 1
        });
        let o = agent_obs("abc", &info(base.clone())).unwrap();
        assert_eq!(o.id, "herdr:abc:w1:p2");
        assert_eq!(
            (o.kind, o.state, o.source),
            (AgentKind::Claude, AgentState::NeedsYou, Source::Herdr)
        );
        assert_eq!(o.project.as_deref(), Some("app"));
        assert_eq!(o.title, "fix tests");
        assert!(o.can_send);

        let mut v = base.clone();
        v["agent"] = "aider".into();
        v["agent_status"] = "done".into();
        let o = agent_obs("abc", &info(v)).unwrap();
        assert_eq!((o.kind, o.state), (AgentKind::Other, AgentState::Done));

        let mut v = base.clone();
        v["agent"] = serde_json::Value::Null;
        assert!(
            agent_obs("abc", &info(v)).is_none(),
            "a plain shell pane is not an agent"
        );

        let mut v = base.clone();
        v["agent_status"] = "hibernating".into();
        assert_eq!(
            agent_obs("abc", &info(v)).unwrap().state,
            AgentState::Unknown
        );

        let mut v = base;
        v["title"] = serde_json::Value::Null;
        v["cwd"] = serde_json::Value::Null;
        let o = agent_obs("abc", &info(v)).unwrap();
        assert_eq!(o.title, "w1:p2");
        assert_eq!((o.cwd, o.project), (None, None));
    }

    #[test]
    fn titles_are_sanitized() {
        let v = serde_json::json!({
            "pane_id": "w1:p2", "agent": "codex", "name": "\u{1b}\u{7}\u{202E}",
            "title": "ok\u{1b}[0m", "cwd": "/a/b\u{7}c"
        });
        let o = agent_obs("abc", &info(v)).unwrap();
        assert_eq!(
            o.title, "ok[0m",
            "a name empty after cleaning falls through"
        );
        assert_eq!(
            (o.cwd.as_deref(), o.project.as_deref()),
            (Some("/a/bc"), Some("bc"))
        );
        let v =
            serde_json::json!({ "pane_id": "w1:p2", "agent": "codex", "title": "t".repeat(300) });
        assert_eq!(
            agent_obs("abc", &info(v)).unwrap().title.chars().count(),
            200
        );
        let v = serde_json::json!({ "pane_id": "w1:p2", "agent": "codex", "name": "\u{7}" });
        assert_eq!(agent_obs("abc", &info(v)).unwrap().title, "w1:p2");
    }
}
