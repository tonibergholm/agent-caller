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

fn claude_reporters<'a>(session_id: &str, panes: &'a [PaneFact]) -> Vec<&'a PaneFact> {
    panes
        .iter()
        .filter(|p| p.kind == AgentKind::Claude && p.session.as_deref() == Some(session_id))
        .collect()
}

/// herdr reports `/tmp/...` where the hook env can say `/private/tmp/...`
/// (macOS), so both sides are compared after resolving symlinks. A
/// discovered socket that no longer resolves keeps its raw path.
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The `socket_hash` of the header socket path after resolving symlinks.
/// Fail closed: a header path that does not resolve is no header.
pub fn header_socket_hash(hints: &Hints) -> Option<String> {
    Some(socket_hash(
        &std::fs::canonicalize(hints.herdr_socket.as_deref()?).ok()?,
    ))
}

/// Claude headers whose socket path was already resolved
/// (`header_socket_hash`), so the binding rules touch no filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeaderRef<'a> {
    pub pane: Option<&'a str>,
    pub socket_hash: Option<&'a str>,
}

/// The `socket_hash` of a discovered pane's socket after resolving symlinks.
pub type PaneSocketHash<'a> = &'a dyn Fn(&Path) -> String;

fn direct_pane_hash(p: &Path) -> String {
    socket_hash(&canonical(p))
}

/// The Claude pane the headers name, before any conflict check. The socket
/// header is only compared through `socket_hash` with sockets herdr
/// discovery already knows; acd never opens a path taken from a header. A
/// pane that reports a different session is not this session's pane.
fn header_candidate<'a>(
    header: HeaderRef<'_>,
    session_id: &str,
    panes: &'a [PaneFact],
    pane_hash: PaneSocketHash<'_>,
) -> Option<&'a PaneFact> {
    let pane = header.pane.filter(|p| valid_id(p))?;
    let hash = header.socket_hash?;
    panes.iter().find(|f| {
        f.pane_id == pane
            && f.kind == AgentKind::Claude
            && pane_hash(&f.socket) == hash
            && f.session.as_deref().is_none_or(|s| s == session_id)
    })
}

/// The pane named by the Claude headers, unless a different Claude pane
/// reports this session: that conflict binds neither.
pub fn header_pane<'a>(
    hints: &Hints,
    session_id: &str,
    panes: &'a [PaneFact],
) -> Option<&'a PaneFact> {
    let hash = header_socket_hash(hints);
    let header = HeaderRef {
        pane: hints.herdr_pane.as_deref(),
        socket_hash: hash.as_deref(),
    };
    header_pane_resolved(header, session_id, panes, &direct_pane_hash)
}

/// `header_pane` with the header socket already resolved.
pub fn header_pane_resolved<'a>(
    header: HeaderRef<'_>,
    session_id: &str,
    panes: &'a [PaneFact],
    pane_hash: PaneSocketHash<'_>,
) -> Option<&'a PaneFact> {
    let c = header_candidate(header, session_id, panes, pane_hash)?;
    let conflict = claude_reporters(session_id, panes)
        .iter()
        .any(|r| r.agent_id != c.agent_id);
    (!conflict).then_some(c)
}

/// Spec §6.2 Ownership, rules 1 to 4, in order. A header/session conflict
/// or two Claude panes reporting the session leave the session unbound
/// (it falls to rule 4), never preferring either pane.
pub fn claude_owner(
    session_id: &str,
    hints: &Hints,
    entrypoint: Option<&str>,
    t3: Option<&dyn SessionOwners>,
    panes: &[PaneFact],
    ignored: &[String],
) -> Owner {
    let hash = header_socket_hash(hints);
    let header = HeaderRef {
        pane: hints.herdr_pane.as_deref(),
        socket_hash: hash.as_deref(),
    };
    claude_owner_resolved(
        session_id,
        header,
        entrypoint,
        t3,
        panes,
        ignored,
        &direct_pane_hash,
    )
}

/// `claude_owner` with the header socket already resolved and the pane
/// sockets resolved by `pane_hash`.
pub fn claude_owner_resolved(
    session_id: &str,
    header: HeaderRef<'_>,
    entrypoint: Option<&str>,
    t3: Option<&dyn SessionOwners>,
    panes: &[PaneFact],
    ignored: &[String],
    pane_hash: PaneSocketHash<'_>,
) -> Owner {
    if let Some(agent) = t3.and_then(|m| m.owner(session_id)) {
        return Owner::T3(agent);
    }
    let reporters = claude_reporters(session_id, panes);
    let bound = match header_candidate(header, session_id, panes, pane_hash) {
        Some(c) => reporters
            .iter()
            .all(|r| r.agent_id == c.agent_id)
            .then_some(c),
        None => match reporters.as_slice() {
            [one] => Some(*one),
            _ => None,
        },
    };
    if let Some(p) = bound {
        return Owner::Herdr(p.agent_id.clone());
    }
    match entrypoint {
        Some(e) if ignored.iter().any(|i| i == e) => Owner::Unknown,
        _ => Owner::Hooks,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexBind {
    /// Carries no more trust than `OnlyInCwd`: herdr's reported session can
    /// come from a stale env (gate 4, an app-server pane reporting another
    /// pane's session). Callers must still apply the screen checks (M2, and
    /// no other Codex pane showing the same command; Task 17).
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
        [one] => {
            // A payload cwd that contradicts the pane's cwd is not a bind.
            return match (cwd, one.cwd.as_deref()) {
                (Some(c), Some(pc)) if !same_dir(pc, c) => CodexBind::Ambiguous,
                _ => CodexBind::Session((**one).clone()),
            };
        }
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
/// Relative paths never match: they would canonicalize against acd's own cwd.
pub fn same_dir(a: &str, b: &str) -> bool {
    if !Path::new(a).is_absolute() || !Path::new(b).is_absolute() {
        return false;
    }
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

    /// Real files: header paths are canonicalized and must resolve.
    fn socks() -> &'static (tempfile::TempDir, String, String) {
        static DIR: std::sync::OnceLock<(tempfile::TempDir, String, String)> =
            std::sync::OnceLock::new();
        DIR.get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let a = dir.path().join("herdr.sock");
            let b = dir.path().join("other.sock");
            std::fs::write(&a, "").unwrap();
            std::fs::write(&b, "").unwrap();
            let a = a.to_string_lossy().into_owned();
            let b = b.to_string_lossy().into_owned();
            (dir, a, b)
        })
    }
    fn sock() -> &'static str {
        &socks().1
    }

    fn pane(id: &str, kind: AgentKind, session: Option<&str>, cwd: Option<&str>) -> PaneFact {
        PaneFact {
            agent_id: format!("herdr:{}:{id}", socket_hash(Path::new(sock()))),
            socket: sock().into(),
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

    #[test]
    fn t3_wins_over_every_other_rule() {
        let t3 = T3([("s".to_string(), "t3:th".to_string())].into());
        let panes = [pane("w1:p1", AgentKind::Claude, Some("s"), None)];
        assert_eq!(
            claude_owner(
                "s",
                &hints("w1:p1", sock()),
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
            claude_owner("s", &hints("w1:p1", sock()), None, None, &panes, &[]),
            Owner::Herdr(panes[0].agent_id.clone())
        );
        assert_eq!(
            claude_owner("s", &hints("w1:p1", &socks().2), None, None, &panes, &[]),
            Owner::Hooks,
            "a pane id from another herdr session must not bind (ids repeat across sessions)"
        );
        assert_eq!(
            claude_owner("s", &hints("w1:p2", sock()), None, None, &panes, &[]),
            Owner::Hooks,
            "not a claude pane"
        );
    }

    #[test]
    fn a_symlinked_header_socket_path_binds_and_an_unresolvable_one_does_not() {
        let panes = [pane("w1:p1", AgentKind::Claude, None, None)];
        let link_dir = tempfile::tempdir().unwrap();
        let link = link_dir.path().join("link.sock");
        std::os::unix::fs::symlink(sock(), &link).unwrap();
        assert_eq!(
            claude_owner(
                "s",
                &hints("w1:p1", &link.to_string_lossy()),
                None,
                None,
                &panes,
                &[]
            ),
            Owner::Herdr(panes[0].agent_id.clone()),
            "the header path resolves to the discovered socket"
        );
        // The discovered side is a symlink too (herdr reports /tmp, the hook /private/tmp).
        let mut via_link = panes[0].clone();
        via_link.socket = link.clone();
        assert_eq!(
            claude_owner("s", &hints("w1:p1", sock()), None, None, &[via_link], &[]),
            Owner::Herdr(panes[0].agent_id.clone())
        );
        assert_eq!(
            claude_owner(
                "s",
                &hints("w1:p1", "/nonexistent/dir/herdr.sock"),
                None,
                None,
                &panes,
                &[]
            ),
            Owner::Hooks,
            "an unresolvable header path is treated as no header"
        );
    }

    #[test]
    fn a_header_pane_reporting_another_session_does_not_bind() {
        let panes = [pane("w1:p1", AgentKind::Claude, Some("other"), None)];
        assert_eq!(
            claude_owner("s", &hints("w1:p1", sock()), None, None, &panes, &[]),
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

    fn claude_hints() -> Hints {
        hints("w1:p1", sock())
    }

    #[test]
    fn two_claude_panes_reporting_the_session_do_not_bind() {
        let panes = [
            pane("w1:p1", AgentKind::Claude, Some("s"), None),
            pane("w1:p2", AgentKind::Claude, Some("s"), None),
        ];
        assert_eq!(
            claude_owner("s", &Hints::default(), None, None, &panes, &[]),
            Owner::Hooks
        );
        let ignored = vec!["sdk-ts".to_string()];
        assert_eq!(
            claude_owner(
                "s",
                &Hints::default(),
                Some("sdk-ts"),
                None,
                &panes,
                &ignored
            ),
            Owner::Unknown
        );
    }

    #[test]
    fn a_codex_pane_reporting_the_session_does_not_count_for_claude() {
        let panes = [
            pane("w1:p1", AgentKind::Claude, Some("s"), None),
            pane("w1:p2", AgentKind::Codex, Some("s"), None),
        ];
        assert!(matches!(
            claude_owner("s", &Hints::default(), None, None, &panes, &[]),
            Owner::Herdr(_)
        ));
    }

    #[test]
    fn header_conflicts_with_another_claude_pane_reporting_the_session() {
        let hdr = pane("w1:p1", AgentKind::Claude, None, None);
        let other = pane("w1:p2", AgentKind::Claude, Some("s"), None);
        for panes in [[hdr.clone(), other.clone()], [other.clone(), hdr.clone()]] {
            assert_eq!(
                claude_owner("s", &claude_hints(), None, None, &panes, &[]),
                Owner::Hooks
            );
            assert!(header_pane(&claude_hints(), "s", &panes).is_none());
        }
        // The header pane itself reporting the session is no conflict.
        let both = pane("w1:p1", AgentKind::Claude, Some("s"), None);
        assert_eq!(
            claude_owner(
                "s",
                &claude_hints(),
                None,
                None,
                std::slice::from_ref(&both),
                &[]
            ),
            Owner::Herdr(both.agent_id)
        );
    }

    #[test]
    fn a_claude_pane_does_not_take_part_in_codex_binding() {
        let claude_same = pane("w1:p1", AgentKind::Claude, Some("s"), Some("/p"));
        let claude_cwd = pane("w1:p2", AgentKind::Claude, None, Some("/p"));
        let codex = pane("w1:p3", AgentKind::Codex, None, Some("/p"));
        assert_eq!(
            codex_bind("s", Some("/p"), &[claude_same, claude_cwd, codex.clone()]),
            CodexBind::OnlyInCwd(codex)
        );
    }

    #[test]
    fn a_session_pane_in_another_cwd_is_ambiguous() {
        let a = pane("w1:p1", AgentKind::Codex, Some("s"), Some("/p"));
        assert_eq!(
            codex_bind("s", Some("/q"), std::slice::from_ref(&a)),
            CodexBind::Ambiguous
        );
        assert_eq!(
            codex_bind("s", None, std::slice::from_ref(&a)),
            CodexBind::Session(a)
        );
    }

    #[test]
    fn same_dir_rejects_relative_paths() {
        assert!(!same_dir("rel", "rel"));
        assert!(!same_dir("", ""));
        assert!(!same_dir(".", "."));
    }

    fn fixture_panes(name: &str) -> Vec<PaneFact> {
        let path = format!(
            "{}/../../tests/fixtures/herdr/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let result = &v["result"];
        let agents: Vec<&serde_json::Value> = match result.get("agents") {
            Some(list) => list.as_array().unwrap().iter().collect(),
            None => vec![&result["agent"]],
        };
        agents
            .into_iter()
            .map(|a| {
                let cwd = a["foreground_cwd"].as_str().or(a["cwd"].as_str());
                let kind = if a["agent"] == "codex" {
                    AgentKind::Codex
                } else {
                    AgentKind::Claude
                };
                pane(
                    a["pane_id"].as_str().unwrap(),
                    kind,
                    a["agent_session"]["value"].as_str(),
                    cwd,
                )
            })
            .collect()
    }

    #[test]
    fn gate_4_fixtures() {
        // Single pane reporting its session.
        let with = fixture_panes("agent-info-codex-with-agent-session.json");
        let sid = "01a11f87-cf06-79d1-a06c-ad853a2b34f1";
        assert!(matches!(
            codex_bind(sid, Some("/private/tmp/acp3/codex/d1"), &with),
            CodexBind::Session(_)
        ));
        assert_eq!(
            codex_bind(sid, Some("/nonexistent/other"), &with),
            CodexBind::Ambiguous
        );

        // No session reported: the only pane in the cwd binds.
        let shared = fixture_panes("agent-info-codex-shared-daemon-no-agent-session.json");
        assert!(
            matches!(codex_bind("any", Some("/private/tmp/acp3/codex/d1"), &shared), CodexBind::OnlyInCwd(p) if p.pane_id == "w1:p1")
        );

        // Four Codex panes in one cwd; the app-server pane (p3) reports a
        // session. By session it binds p3 (callers must screen-check); by cwd
        // alone it is ambiguous; a payload from another cwd never binds it.
        let mis = fixture_panes("agent-list-codex-fresh-app-server-misbound.json");
        let cwd = "/private/tmp/acp3/codex/d1";
        assert!(
            matches!(codex_bind("01a11f8b-4cd7-7ad0-8b21-cfd40e5ccbde", Some(cwd), &mis), CodexBind::Session(p) if p.pane_id == "w1:p3")
        );
        assert_eq!(
            codex_bind(
                "01a11f8b-4cd7-7ad0-8b21-cfd40e5ccbde",
                Some("/private/tmp/acp3/codex/other"),
                &mis
            ),
            CodexBind::Ambiguous
        );
        assert_eq!(
            codex_bind("fresh-session", Some(cwd), &mis),
            CodexBind::Ambiguous
        );
    }
}
