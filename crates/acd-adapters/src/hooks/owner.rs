//! Who owns a session (spec §6.2 Ownership) and which herdr pane it is
//! bound to (§6.2 Binding; §6.3 Binding as amended by gate 4).
use std::path::{Path, PathBuf};

use acd_protocol::AgentKind;

use super::wire::{Hints, valid_id};
use crate::herdr::map::socket_hash;
use crate::herdr::reconciler::PaneMap;
use crate::t3::sessions::SessionMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneFact {
    pub agent_id: String,
    pub socket: PathBuf,
    pub pane_id: String,
    pub kind: AgentKind,
    pub session: Option<String>,
    pub cwd: Option<String>,
    /// G1: tickets and targets compare it.
    pub incarnation: u64,
}

pub fn pane_facts(panes: &PaneMap) -> Vec<PaneFact> {
    panes
        .read()
        .expect("pane map")
        .iter()
        .map(|(id, p)| PaneFact {
            agent_id: id.clone(),
            socket: p.socket.clone(),
            pane_id: p.pane_id.clone(),
            kind: p.kind,
            session: p.session.clone(),
            cwd: p.cwd.clone(),
            incarnation: p.incarnation,
        })
        .collect()
}

pub trait SessionOwners: Send + Sync {
    fn owner(&self, session_id: &str) -> Option<String>;
}

impl SessionOwners for SessionMap {
    fn owner(&self, session_id: &str) -> Option<String> {
        SessionMap::owner(self, session_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    T3(String),
    Herdr(String),
    Hooks,
    Unknown,
}

/// The pane named by the Claude headers. The socket header is only
/// compared through `socket_hash` with sockets herdr discovery already
/// knows; acd never opens a path taken from a header. A pane that reports
/// a different session is not this session's pane.
pub fn header_pane<'a>(
    hints: &Hints,
    session_id: &str,
    panes: &'a [PaneFact],
) -> Option<&'a PaneFact> {
    let pane = hints.herdr_pane.as_deref().filter(|p| valid_id(p))?;
    let hash = socket_hash(Path::new(hints.herdr_socket.as_deref()?));
    panes.iter().find(|f| {
        f.pane_id == pane
            && f.kind == AgentKind::Claude
            && socket_hash(&f.socket) == hash
            && f.session.as_deref().is_none_or(|s| s == session_id)
    })
}

/// Spec §6.2 Ownership, rules 1 to 4, in order.
pub fn claude_owner(
    session_id: &str,
    hints: &Hints,
    entrypoint: Option<&str>,
    t3: Option<&dyn SessionOwners>,
    panes: &[PaneFact],
    ignored: &[String],
) -> Owner {
    if let Some(agent) = t3.and_then(|m| m.owner(session_id)) {
        return Owner::T3(agent);
    }
    if let Some(p) = header_pane(hints, session_id, panes) {
        return Owner::Herdr(p.agent_id.clone());
    }
    if let Some(p) = panes
        .iter()
        .find(|p| p.kind == AgentKind::Claude && p.session.as_deref() == Some(session_id))
    {
        return Owner::Herdr(p.agent_id.clone());
    }
    match entrypoint {
        Some(e) if ignored.iter().any(|i| i == e) => Owner::Unknown,
        _ => Owner::Hooks,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexBind {
    Session(PaneFact),
    OnlyInCwd(PaneFact),
    Ambiguous,
    Unbound,
}

/// Gate 4 and ruling B4: the hook env is never used. Exactly one Codex
/// pane reporting this session binds; two or more are ambiguous.
/// Otherwise the cwd binds only when exactly one Codex pane is in it
/// across all panes, whatever sessions they report. The caller still
/// requires that pane's own screen to match (M2) and no competing pane
/// to show the same command (Task 17).
pub fn codex_bind(session_id: &str, cwd: Option<&str>, panes: &[PaneFact]) -> CodexBind {
    let codex: Vec<&PaneFact> = panes
        .iter()
        .filter(|p| p.kind == AgentKind::Codex)
        .collect();
    let by_session: Vec<&&PaneFact> = codex
        .iter()
        .filter(|p| p.session.as_deref() == Some(session_id))
        .collect();
    match by_session.as_slice() {
        [one] => return CodexBind::Session((**one).clone()),
        [_, _, ..] => return CodexBind::Ambiguous,
        [] => {}
    }
    let Some(cwd) = cwd else {
        return CodexBind::Unbound;
    };
    let same: Vec<&&PaneFact> = codex
        .iter()
        .filter(|p| p.cwd.as_deref().is_some_and(|c| same_dir(c, cwd)))
        .collect();
    match same.as_slice() {
        [] => CodexBind::Unbound,
        [one] => CodexBind::OnlyInCwd((**one).clone()),
        _ => CodexBind::Ambiguous,
    }
}

/// herdr reports `/tmp/...`, Codex `/private/tmp/...` (D5).
pub fn same_dir(a: &str, b: &str) -> bool {
    let canon = |s: &str| {
        std::fs::canonicalize(s).unwrap_or_else(|_| PathBuf::from(s.trim_end_matches('/')))
    };
    canon(a) == canon(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct T3(HashMap<String, String>);
    impl SessionOwners for T3 {
        fn owner(&self, s: &str) -> Option<String> {
            self.0.get(s).cloned()
        }
    }

    fn pane(id: &str, kind: AgentKind, session: Option<&str>, cwd: Option<&str>) -> PaneFact {
        PaneFact {
            agent_id: format!(
                "herdr:{}:{id}",
                socket_hash(Path::new("/h/.config/herdr/herdr.sock"))
            ),
            socket: "/h/.config/herdr/herdr.sock".into(),
            pane_id: id.into(),
            kind,
            session: session.map(Into::into),
            cwd: cwd.map(Into::into),
            incarnation: 1,
        }
    }
    fn hints(pane: &str, socket: &str) -> Hints {
        Hints {
            herdr_pane: Some(pane.into()),
            herdr_socket: Some(socket.into()),
            entrypoint: None,
        }
    }
    const SOCK: &str = "/h/.config/herdr/herdr.sock";

    #[test]
    fn t3_wins_over_every_other_rule() {
        let t3 = T3([("s".to_string(), "t3:th".to_string())].into());
        let panes = [pane("w1:p1", AgentKind::Claude, Some("s"), None)];
        assert_eq!(
            claude_owner(
                "s",
                &hints("w1:p1", SOCK),
                Some("cli"),
                Some(&t3),
                &panes,
                &[]
            ),
            Owner::T3("t3:th".into())
        );
    }

    #[test]
    fn header_binding_needs_a_known_socket_and_a_claude_pane() {
        let panes = [
            pane("w1:p1", AgentKind::Claude, None, None),
            pane("w1:p2", AgentKind::Codex, None, None),
        ];
        assert_eq!(
            claude_owner("s", &hints("w1:p1", SOCK), None, None, &panes, &[]),
            Owner::Herdr(panes[0].agent_id.clone())
        );
        assert_eq!(
            claude_owner(
                "s",
                &hints("w1:p1", "/other/herdr.sock"),
                None,
                None,
                &panes,
                &[]
            ),
            Owner::Hooks,
            "a pane id from another herdr session must not bind (ids repeat across sessions)"
        );
        assert_eq!(
            claude_owner("s", &hints("w1:p2", SOCK), None, None, &panes, &[]),
            Owner::Hooks,
            "not a claude pane"
        );
    }

    #[test]
    fn a_header_pane_reporting_another_session_does_not_bind() {
        let panes = [pane("w1:p1", AgentKind::Claude, Some("other"), None)];
        assert_eq!(
            claude_owner("s", &hints("w1:p1", SOCK), None, None, &panes, &[]),
            Owner::Hooks
        );
    }

    #[test]
    fn agent_session_binds_without_headers() {
        let panes = [pane("w1:p1", AgentKind::Claude, Some("s"), None)];
        assert_eq!(
            claude_owner("s", &Hints::default(), None, None, &panes, &[]),
            Owner::Herdr(panes[0].agent_id.clone())
        );
    }

    #[test]
    fn ignored_entrypoint_is_unknown_unless_bound() {
        let ignored = vec!["sdk-ts".to_string()];
        assert_eq!(
            claude_owner("s", &Hints::default(), Some("sdk-ts"), None, &[], &ignored),
            Owner::Unknown
        );
        assert_eq!(
            claude_owner("s", &Hints::default(), Some("cli"), None, &[], &ignored),
            Owner::Hooks
        );
        assert_eq!(
            claude_owner("s", &Hints::default(), None, None, &[], &ignored),
            Owner::Hooks,
            "an unset entrypoint is not ignored"
        );
        let panes = [pane("w1:p1", AgentKind::Claude, Some("s"), None)];
        assert!(
            matches!(
                claude_owner(
                    "s",
                    &Hints::default(),
                    Some("sdk-ts"),
                    None,
                    &panes,
                    &ignored
                ),
                Owner::Herdr(_)
            ),
            "a herdr binding keeps a session whatever its inherited entrypoint (P0 finding 2)"
        );
    }

    #[test]
    fn codex_env_is_never_used_for_binding() {
        // The signature has no hints: gate 4 showed the env names a stale or
        // other pane. Two panes in one cwd stay ambiguous whatever the env says.
        let panes = [
            pane("w1:p1", AgentKind::Codex, None, Some("/p")),
            pane("w1:p2", AgentKind::Codex, None, Some("/p")),
        ];
        assert_eq!(codex_bind("s", Some("/p"), &panes), CodexBind::Ambiguous);
    }

    #[test]
    fn codex_binds_by_session_then_by_the_only_pane_in_the_cwd() {
        let a = pane("w1:p1", AgentKind::Codex, Some("s"), Some("/p"));
        let b = pane("w1:p2", AgentKind::Codex, None, Some("/p"));
        let c = pane("w1:p3", AgentKind::Codex, Some("other"), Some("/q"));
        assert_eq!(
            codex_bind("s", Some("/p"), &[a.clone(), b.clone()]),
            CodexBind::Session(a.clone())
        );
        assert_eq!(
            codex_bind("s2", Some("/p"), &[a.clone(), b.clone()]),
            CodexBind::Ambiguous,
            "two Codex panes in the cwd, whatever sessions they report (ruling B4)"
        );
        assert_eq!(
            codex_bind("s2", Some("/q"), &[a.clone(), b.clone(), c.clone()]),
            CodexBind::OnlyInCwd(c),
            "the only pane in the cwd binds even if it reports another session; its screen must still match"
        );
        assert_eq!(codex_bind("s2", Some("/r"), &[a, b]), CodexBind::Unbound);
        assert_eq!(codex_bind("s2", None, &[]), CodexBind::Unbound);
    }

    #[test]
    fn a_session_reported_by_two_panes_is_ambiguous() {
        let a = pane("w1:p1", AgentKind::Codex, Some("s"), Some("/p"));
        let b = pane("w1:p2", AgentKind::Codex, Some("s"), Some("/q"));
        assert_eq!(codex_bind("s", Some("/p"), &[a, b]), CodexBind::Ambiguous);
    }

    #[test]
    fn same_dir_resolves_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(same_dir(link.to_str().unwrap(), real.to_str().unwrap()));
        assert!(same_dir("/nonexistent/x/", "/nonexistent/x"));
        assert!(!same_dir("/nonexistent/x", "/nonexistent/y"));
    }
}
