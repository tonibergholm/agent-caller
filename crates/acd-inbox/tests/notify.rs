use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_client::view::{CardGroup, CardView, InboxView, TrayColor, TraySummary};
use acd_inbox::notify::*;
use acd_protocol::RequestKind;
use serde_json::json;

fn card(id: &str, revision: u64, countdown: bool) -> CardView {
    let agent = serde_json::from_value(json!({"id":id,"machine_id":"m","source":"herdr","kind":"claude","title":"api\u{202E} refactor\n","cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00Z","failed":false,"last_message":null,"can_send":true,"pending":{"id":format!("r-{id}"),"revision":revision,"agent_id":id,"kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"rm -rf SECRET-DETAIL","options":[],"questions":null,"free_text":false,"raw_screen":null,"opened_at":"2026-10-08T00:00:00Z"}})).unwrap();
    CardView {
        key: format!("m1/{id}"),
        machine_key: "m1".into(),
        machine_name: "Box".into(),
        agent,
        group: CardGroup::Request,
        live: true,
        offline_since_ms: None,
        answerable: true,
        free_text_answerable: false,
        selectable_options: vec![],
        can_reply: false,
        can_mark_seen: false,
        busy: false,
        notice: None,
        action: countdown.then(|| acd_client::view::ActionView {
            id: "x".into(),
            card_key: format!("m1/{id}"),
            label: "Yes".into(),
            detail: None,
            fires_at_ms: 0.,
        }),
    }
}

fn view(cards: Vec<CardView>) -> InboxView {
    InboxView {
        machines: vec![],
        cards,
        hidden_idle: 0,
        show_idle: false,
        show_subagents: false,
        show_hook_only: false,
        tray: TraySummary { color: TrayColor::Amber, needs_you: 1 },
        actions: vec![],
    }
}

fn key(id: &str, revision: u64) -> NoteKey {
    NoteKey { card: format!("m1/{id}"), request: format!("r-{id}"), revision }
}

fn shown(cmds: &[NoteCmd]) -> Vec<NoteKey> {
    cmds.iter().filter_map(|c| if let NoteCmd::Show { key, .. } = c { Some(key.clone()) } else { None }).collect()
}

#[test]
fn one_notification_per_request_revision_and_none_for_repeats() {
    let mut r = Reconciler::default();
    assert_eq!(shown(&r.reconcile(&view(vec![card("a1", 1, false)]), true, false)), [key("a1", 1)]);
    assert!(r.reconcile(&view(vec![card("a1", 1, false)]), true, false).is_empty());
    let bumped = r.reconcile(&view(vec![card("a1", 2, false)]), true, false);
    assert!(bumped.contains(&NoteCmd::Close { key: key("a1", 1) }));
    assert_eq!(shown(&bumped), [key("a1", 2)]);
    assert_eq!(r.reconcile(&view(vec![]), true, false), [NoteCmd::Close { key: key("a1", 2) }]);
}

#[test]
fn none_while_a_panel_is_open_off_or_counting_and_not_later_either() {
    let mut r = Reconciler::default();
    assert!(r.reconcile(&view(vec![card("a1", 1, false)]), true, true).is_empty());
    assert!(r.reconcile(&view(vec![card("a1", 1, false)]), true, false).is_empty(), "seen while the panel was open");
    assert!(r.reconcile(&view(vec![card("a1", 1, false), card("a2", 1, true)]), true, false).is_empty(), "own countdown");
    assert!(r.reconcile(&view(vec![card("a1", 1, false), card("a2", 1, false), card("a3", 1, false)]), false, false).is_empty());
}

#[test]
fn the_text_is_templated_cleaned_and_holds_no_detail() {
    let (summary, body) = text(&card("a1", 1, false)).unwrap();
    assert_eq!(summary, "api refactor on Box needs you");
    assert_eq!(body, "bash approval");
    assert!(!format!("{summary}{body}").contains("SECRET"));
    assert_eq!(request_label(RequestKind::Permission, "Run npm test?"), "approval");
    assert_eq!(request_label(RequestKind::Question, "x"), "question");
    assert_eq!(request_label(RequestKind::BlockedUnparsed, "x"), "blocked");
    assert_eq!(clean(&"y".repeat(100), 60).chars().count(), 60);
}

#[derive(Default)]
struct FakeNotify {
    next: Mutex<u32>,
    shows: Mutex<Vec<(u32, String, bool)>>,
    closes: Mutex<Vec<u32>>,
    owner: Mutex<Option<String>>,
    fail: Mutex<bool>,
    attempts: Mutex<u32>,
}

#[async_trait::async_trait]
impl NotifyBackend for FakeNotify {
    async fn show(&self, summary: &str, _body: &str, open_action: bool) -> Result<u32, NotifyError> {
        *self.attempts.lock().unwrap() += 1;
        if *self.fail.lock().unwrap() {
            return Err(NotifyError::Unavailable);
        }
        let mut n = self.next.lock().unwrap();
        *n += 1;
        self.shows.lock().unwrap().push((*n, summary.into(), open_action));
        Ok(*n)
    }
    async fn close(&self, id: u32) {
        self.closes.lock().unwrap().push(id);
    }
    async fn owner(&self) -> Option<String> {
        self.owner.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct Opened(Mutex<Vec<String>>);

impl Opener for Opened {
    fn open(&self, card: &str) {
        self.0.lock().unwrap().push(card.into());
    }
}

fn fake() -> (Arc<FakeNotify>, Arc<Opened>, Arc<Notifier>) {
    let backend = Arc::new(FakeNotify::default());
    *backend.owner.lock().unwrap() = Some(":1.10".into());
    let opened = Arc::new(Opened::default());
    let n = Notifier::new(backend.clone(), Some(opened.clone()));
    (backend, opened, n)
}

#[tokio::test]
async fn the_open_action_opens_its_card_and_resolving_closes_it() {
    let (backend, opened, n) = fake();
    n.apply(&view(vec![card("a1", 1, false)]), true, false).await;
    let (id, _, with_open) = backend.shows.lock().unwrap()[0].clone();
    assert!(with_open);
    assert!(n.on_action(&ActionSignal { id, action: OPEN_ACTION.into(), sender: ":1.10".into() }).await);
    assert_eq!(*opened.0.lock().unwrap(), ["m1/a1"]);
    n.apply(&view(vec![]), true, false).await;
    assert_eq!(*backend.closes.lock().unwrap(), [id]);
}

#[tokio::test]
async fn actions_from_unknown_ids_or_senders_are_ignored() {
    let (backend, opened, n) = fake();
    n.apply(&view(vec![card("a1", 1, false)]), true, false).await;
    let id = backend.shows.lock().unwrap()[0].0;
    assert!(!n.on_action(&ActionSignal { id: id + 7, action: OPEN_ACTION.into(), sender: ":1.10".into() }).await);
    assert!(!n.on_action(&ActionSignal { id, action: OPEN_ACTION.into(), sender: ":1.99".into() }).await);
    assert!(!n.on_action(&ActionSignal { id, action: "default".into(), sender: ":1.10".into() }).await);
    assert!(opened.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn actions_from_a_new_notification_server_are_ignored() {
    let (backend, opened, n) = fake();
    n.apply(&view(vec![card("a1", 1, false)]), true, false).await;
    let id = backend.shows.lock().unwrap()[0].0;
    *backend.owner.lock().unwrap() = Some(":1.42".into());
    assert!(!n.on_action(&ActionSignal { id, action: OPEN_ACTION.into(), sender: ":1.42".into() }).await);
    assert!(opened.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failing_server_turns_notifications_off_for_the_session() {
    let (backend, _opened, n) = fake();
    *backend.fail.lock().unwrap() = true;
    n.apply(&view(vec![card("a1", 1, false), card("a2", 1, false)]), true, false).await;
    assert_eq!(*backend.attempts.lock().unwrap(), 1, "the batch stops at the first failure (D28)");
    *backend.fail.lock().unwrap() = false;
    n.apply(&view(vec![card("a1", 1, false), card("a2", 1, false), card("a3", 1, false)]), true, false).await;
    assert!(backend.shows.lock().unwrap().is_empty());
    assert_eq!(*backend.attempts.lock().unwrap(), 1);
}

fn script(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("omarchy-shell");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn the_opener_runs_an_argv_without_a_shell_and_times_out() {
    let d = tempfile::tempdir().unwrap();
    let out = d.path().join("args");
    let shell = script(d.path(), &format!("printf '%s|' \"$@\" > '{}'", out.display()));
    let opener = ShellOpener { shell, timeout: Duration::from_secs(5) };
    assert_eq!(opener.run("m1/a b;touch x").await, OpenResult::Exited);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "tonibergholm.agentcaller|openCard|m1/a b;touch x|");
    assert!(!d.path().join("x").exists());
    let slow = ShellOpener { shell: script(d.path(), "sleep 30"), timeout: Duration::from_millis(200) };
    let started = std::time::Instant::now();
    assert_eq!(slow.run("m1/a").await, OpenResult::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn omarchy_shell_is_found_on_path_else_in_the_fallback_dir() {
    let d = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    script(d.path(), "true");
    let found = ShellOpener::find(Some(d.path().as_os_str()), empty.path()).unwrap();
    assert_eq!(found.shell, d.path().join("omarchy-shell"));
    let fallback = ShellOpener::find(Some(empty.path().as_os_str()), d.path()).unwrap();
    assert_eq!(fallback.shell, d.path().join("omarchy-shell"));
    assert!(ShellOpener::find(Some(empty.path().as_os_str()), empty.path()).is_none());
}
