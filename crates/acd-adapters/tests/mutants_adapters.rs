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

// ---- t3 thread detail ----
use acd_adapters::t3::detail::ThreadDetail;
use acd_adapters::t3::wire::ThreadEvent;
use serde_json::json;

fn provider_thread(id: &str, native: Option<&str>) -> serde_json::Value {
    json!({"id": id, "nativeThreadRef": native.map(|n| json!({"nativeId": n}))})
}

fn request(id: &str, status: &str) -> serde_json::Value {
    json!({"id": id, "kind": "approval", "status": status})
}

fn event(kind: &str, payload: serde_json::Value) -> ThreadEvent {
    serde_json::from_value(json!({"type": kind, "payload": payload})).unwrap()
}

/// detail.rs:52 `delete field active_provider_thread`, :62 / 70 / 76 `+=` -> `-=` / `*=`
/// (`from_projection`): the active thread is read, and every entry that cannot be read is counted.
#[test]
fn a_projection_reports_its_active_thread_and_what_it_skipped() {
    let projection = json!({
        "thread": {"activeProviderThreadId": "p1"},
        "providerThreads": [provider_thread("p1", Some("native-1")), {"id": 7}],
        "runtimeRequests": [request("r1", "pending"), {"id": "r2"}],
        "turnItems": [{"type": "turn-item", "id": 1}, {"nonsense": true}],
    });
    assert!(ThreadDetail::valid_projection(&projection));
    let (detail, skipped) = ThreadDetail::from_projection(&projection);
    assert_eq!(detail.active_provider_thread.as_deref(), Some("p1"));
    assert_eq!(detail.native_session_id(), Some("native-1"));
    assert_eq!(detail.request_status("r1"), Some("pending"));
    assert_eq!(detail.request_status("r2"), None);
    assert!(skipped >= 3, "one bad entry in each of the three lists: {skipped}");
    let one_each = json!({
        "thread": {"activeProviderThreadId": null},
        "providerThreads": [{"id": 7}],
        "runtimeRequests": [{"id": "r2"}],
        "turnItems": [{"nonsense": true}],
    });
    let (detail, skipped) = ThreadDetail::from_projection(&one_each);
    assert_eq!(detail.active_provider_thread, None);
    assert_eq!(skipped, 3);
    assert!(!ThreadDetail::valid_projection(&json!({"thread": {}, "providerThreads": [], "runtimeRequests": [], "turnItems": []})));
    assert!(!ThreadDetail::valid_projection(&json!({"thread": {"activeProviderThreadId": 5}, "providerThreads": [], "runtimeRequests": [], "turnItems": []})));
}

/// detail.rs:103 `put_provider_thread -> ()` and :104 `!=` -> `==`: a provider
/// thread's native session is remembered, and updating a thread again does not
/// crowd the others out of the eight that are kept.
#[test]
fn provider_threads_are_remembered_once_each() {
    let mut d = ThreadDetail::default();
    d.apply(&event("provider-thread.updated", provider_thread("keep", Some("native-keep")))).unwrap();
    for _ in 0..12 {
        d.apply(&event("provider-thread.updated", provider_thread("other", Some("native-other")))).unwrap();
    }
    d.prune(&[]);
    d.apply(&event("thread.metadata-updated", json!({"activeProviderThreadId": "keep"}))).unwrap();
    assert_eq!(d.native_session_id(), Some("native-keep"));
    d.apply(&event("thread.metadata-updated", json!({"activeProviderThreadId": "other"}))).unwrap();
    assert_eq!(d.native_session_id(), Some("native-other"), "repeated updates are one entry, not twelve");
    d.apply(&event("thread.metadata-updated", json!({"activeProviderThreadId": null}))).unwrap();
    assert_eq!(d.native_session_id(), None);
}

/// detail.rs:112 `||` -> `&&` (`prune`): a request stays while it is pending or
/// while the reconciler still names it.
#[test]
fn prune_keeps_pending_and_named_requests() {
    let mut d = ThreadDetail::default();
    for (id, status) in [("pending", "pending"), ("named", "completed"), ("done", "completed")] {
        d.apply(&event("runtime-request.updated", request(id, status))).unwrap();
    }
    assert_eq!(d.retained().0, 3);
    d.prune(&["named"]);
    assert_eq!(d.request_status("pending"), Some("pending"));
    assert_eq!(d.request_status("named"), Some("completed"));
    assert_eq!(d.request_status("done"), None);
    assert_eq!(d.retained().0, 2);
}

// ---- t3 mapping, outcome, targets ----
use acd_adapters::t3::conn::CallError;
use acd_adapters::t3::detail::PendingView;
use acd_adapters::t3::map::{self, Target, TargetAnswer};
use acd_adapters::t3::outcome::{Outcome, classify};
use acd_adapters::t3::targets::Targets;
use acd_adapters::t3::wire::{Failure, RuntimeRequest, TurnItem};

/// map.rs:41 match guard `starts_with("codex")` (`kind`): only a codex provider instance is Codex.
#[test]
fn provider_instances_map_to_their_agent_kind() {
    assert_eq!(map::kind(Some("claude-main")), AgentKind::Claude);
    assert_eq!(map::kind(Some("codex")), AgentKind::Codex);
    assert_eq!(map::kind(Some("codex-2")), AgentKind::Codex);
    assert_eq!(map::kind(Some("gemini")), AgentKind::Other);
    assert_eq!(map::kind(None), AgentKind::Other);
}

/// map.rs:239 match guard `!questions.is_empty()` (`request_obs`): a question
/// request with no questions is shown read-only; it must not be indexed.
#[test]
fn a_question_request_without_questions_is_read_only() {
    let view = PendingView {
        request: RuntimeRequest { id: "rq".into(), kind: "user-input".into(), status: "pending".into() },
        item: Some(TurnItem::UserInputRequest { id: "i".into(), request_id: "rq".into(), questions: vec![] }),
        command: None,
    };
    let (obs, target) = map::request_obs("t3:a", "thread", Some("codex"), &view);
    assert_eq!(obs.answer_path, acd_protocol::AnswerPath::None);
    assert!(obs.options.is_empty() && obs.questions.is_none() && !obs.free_text);
    assert_eq!(target.answer, TargetAnswer::ReadOnly);
}

/// outcome.rs:58 match guard `tag == DISPATCH_ERROR` (`classify`): only T3's dispatch
/// error can be read as "this request is already over"; the same words under any
/// other tag are an unknown refusal (fail closed: uncertain, never "not delivered").
#[test]
fn only_a_dispatch_error_can_say_the_request_is_over() {
    let fail = |tag: &str, message: &str| -> Result<serde_json::Value, CallError> {
        Err(CallError::Failed(Failure::Fail { tag: tag.into(), message: message.into() }))
    };
    let dispatch = "OrchestrationV2DispatchCommandError";
    assert!(matches!(
        classify(&fail(dispatch, "Runtime request rq is resolved."), Some("rq")),
        Outcome::NotDelivered { code: acd_protocol::ErrorCode::RequestGone, .. }
    ));
    for message in ["Runtime request rq is resolved.", "Runtime request rq was not found."] {
        assert!(matches!(classify(&fail("SomethingElse", message), Some("rq")), Outcome::Uncertain(_)), "{message}");
    }
    assert!(matches!(classify(&fail(dispatch, "Runtime request rq is resolved."), None), Outcome::Uncertain(_)));
    assert!(matches!(classify(&fail(dispatch, "Runtime request other is resolved."), Some("rq")), Outcome::Uncertain(_)));
    assert!(matches!(classify(&Ok(json!({})), Some("rq")), Outcome::Delivered));
}

fn target(agent: &str, thread: &str, request: &str) -> Target {
    Target { agent_id: agent.into(), thread_id: thread.into(), t3_request_id: request.into(), answer: TargetAnswer::Decision }
}

/// targets.rs:26 / 29 / 33 / 37 (`retain_thread`, `len`, `is_empty`): a thread's targets
/// are dropped except the one named; other threads are left alone.
#[test]
fn retain_thread_keeps_the_named_target_and_other_threads() {
    let targets = Targets::default();
    assert!(targets.is_empty());
    assert_eq!(targets.len(), 0);
    targets.insert(target("t3:a", "ta", "r1"));
    targets.insert(target("t3:a", "ta", "r2"));
    targets.insert(target("t3:a", "ta", "r3"));
    targets.insert(target("t3:b", "tb", "r4"));
    assert!(!targets.is_empty());
    assert_eq!(targets.len(), 4);
    targets.retain_thread("ta", Some("r2"));
    assert_eq!(targets.len(), 2, "r2 and the other thread's r4 remain");
    targets.retain_thread("ta", None);
    assert_eq!(targets.len(), 1, "only the other thread's target remains");
    targets.remove("r4");
    assert!(targets.is_empty());
}
