//! Tests added from a cargo-mutants run (docs/research/2026-11-04-mutation-testing.md).
//! Each test names the mutant it kills.
use std::path::{Path, PathBuf};

use acd_adapters::hooks::owner::*;
use acd_adapters::hooks::paths::PathResolver;
use acd_adapters::hooks::wire::Hints;
use acd_adapters::t3::sessions::SessionMap;
use acd_protocol::AgentKind;

fn pane(agent: &str, pane_id: &str, kind: AgentKind, session: Option<&str>, socket: &str) -> PaneFact {
    PaneFact {
        agent_id: agent.into(),
        socket: PathBuf::from(socket),
        pane_id: pane_id.into(),
        kind,
        session: session.map(str::to_owned),
        cwd: None,
        incarnation: 1,
    }
}

/// A pane's socket "hashes" to its own path, so the test names the hash directly.
fn by_path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn header<'a>(pane: &'a str, socket: &'a str) -> HeaderRef<'a> {
    HeaderRef { pane: Some(pane), socket_hash: Some(socket) }
}

/// owner.rs:138 `header_pane_resolved -> None` and the rules it applies: the
/// pane the headers name, on the socket they name, of kind Claude, that does
/// not report another session, and that no other Claude pane contradicts.
#[test]
fn the_headers_bind_one_matching_claude_pane() {
    let panes = vec![
        pane("herdr:a", "p1", AgentKind::Claude, None, "/s/one"),
        pane("herdr:b", "p2", AgentKind::Claude, None, "/s/two"),
        pane("herdr:c", "p1", AgentKind::Codex, None, "/s/three"),
    ];
    let bound = |h: HeaderRef<'_>, session: &str, panes: &[PaneFact]| {
        header_pane_resolved(h, session, panes, &by_path).map(|p| p.agent_id.clone())
    };
    assert_eq!(bound(header("p1", "/s/one"), "s", &panes).as_deref(), Some("herdr:a"));
    assert_eq!(bound(header("p2", "/s/two"), "s", &panes).as_deref(), Some("herdr:b"));
    // Wrong socket for that pane id, unknown pane, a Codex pane, no headers.
    assert_eq!(bound(header("p1", "/s/two"), "s", &panes), None);
    assert_eq!(bound(header("p9", "/s/one"), "s", &panes), None);
    assert_eq!(bound(header("p1", "/s/three"), "s", &panes), None);
    assert_eq!(bound(HeaderRef::default(), "s", &panes), None);
    assert_eq!(bound(HeaderRef { pane: Some("p1"), socket_hash: None }, "s", &panes), None);
    assert_eq!(bound(HeaderRef { pane: Some("bad\u{1}id"), socket_hash: Some("/s/one") }, "s", &panes), None);
}

#[test]
fn a_pane_reporting_another_session_or_a_contradicting_pane_binds_nothing() {
    let reports_other = vec![pane("herdr:a", "p1", AgentKind::Claude, Some("other"), "/s/one")];
    assert!(header_pane_resolved(header("p1", "/s/one"), "s", &reports_other, &by_path).is_none());
    let reports_this = vec![pane("herdr:a", "p1", AgentKind::Claude, Some("s"), "/s/one")];
    assert!(header_pane_resolved(header("p1", "/s/one"), "s", &reports_this, &by_path).is_some());
    // A different Claude pane reports this very session: the conflict binds neither.
    let conflict = vec![
        pane("herdr:a", "p1", AgentKind::Claude, None, "/s/one"),
        pane("herdr:b", "p2", AgentKind::Claude, Some("s"), "/s/two"),
    ];
    assert!(header_pane_resolved(header("p1", "/s/one"), "s", &conflict, &by_path).is_none());
}

/// owner.rs:123 `header_pane -> None`: the header socket path is resolved through the
/// filesystem; one that does not resolve is no header.
#[test]
fn header_pane_resolves_the_socket_header_through_the_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("herdr.sock");
    std::fs::write(&sock, "").unwrap();
    let panes = vec![pane("herdr:a", "p1", AgentKind::Claude, None, &sock.to_string_lossy())];
    let hints = |socket: String| Hints { herdr_pane: Some("p1".into()), herdr_socket: Some(socket), entrypoint: None };
    let found = header_pane(&hints(sock.to_string_lossy().into_owned()), "s", &panes);
    assert_eq!(found.map(|p| p.agent_id.as_str()), Some("herdr:a"));
    // A path through a symlink resolves to the same socket.
    let link = dir.path().join("link.sock");
    std::os::unix::fs::symlink(&sock, &link).unwrap();
    let found = header_pane(&hints(link.to_string_lossy().into_owned()), "s", &panes);
    assert_eq!(found.map(|p| p.agent_id.as_str()), Some("herdr:a"));
    // Fail closed: a header path that does not resolve is no header.
    let missing = dir.path().join("gone.sock");
    assert!(header_pane(&hints(missing.to_string_lossy().into_owned()), "s", &panes).is_none());
    assert!(header_pane(&Hints::default(), "s", &panes).is_none());
}

/// owner.rs:300 `||` -> `&&` (`same_dir_with`): a relative path never matches, and
/// the key function is not even asked about it.
#[test]
fn a_relative_directory_never_matches() {
    let same = |_: &str| Some(PathBuf::from("/same"));
    assert_eq!(same_dir_with("/a", "/b", &same), Some(true));
    assert_eq!(same_dir_with("rel", "/b", &same), Some(false));
    assert_eq!(same_dir_with("/a", "rel", &same), Some(false));
    assert_eq!(same_dir_with("rel", "rel", &same), Some(false));
    let unknown = |_: &str| None;
    assert_eq!(same_dir_with("/a", "/b", &unknown), None);
    assert_eq!(same_dir_with("rel", "/b", &unknown), Some(false), "relative is decided before any lookup");
    assert!(!same_dir("rel", "rel"));
}

/// owner.rs:47 `SessionOwners for SessionMap`: the owner is the first claimant.
#[test]
fn the_session_map_reports_its_owner_through_the_trait() {
    let map = SessionMap::default();
    map.set_for_tests("t3:a", Some("s"));
    let owners: &dyn SessionOwners = &map;
    assert_eq!(owners.owner("s").as_deref(), Some("t3:a"));
    assert_eq!(owners.owner("other"), None);
}

/// paths.rs:34 `Debug` for `PathResolver`.
#[test]
fn a_path_resolver_debugs_as_its_name() {
    assert_eq!(format!("{:?}", PathResolver::fs()), "PathResolver");
}
