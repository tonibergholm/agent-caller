//! The producer API end to end: HTTP over a Unix socket into the adapter,
//! the registry and the event stream. Time comes from a manual clock, so
//! lease and answer deadlines are crossed without waiting.
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use acd_adapters::external::{ExternalAdapter, ExternalConfig};
use acd_adapters::hooks::endpoint::{Credential, HookAuth};
use acd_core::{Core, testing::test_machine};
use acd_protocol::*;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

const TOKEN: &str = "tok-external-0123456789";
const LEASE: u64 = 60_000;
const START_MS: i64 = 1_760_000_000_000;

struct Rig {
    core: Arc<Core>,
    adapter: Arc<ExternalAdapter>,
    clock: Arc<AtomicI64>,
    rx: broadcast::Receiver<ServerMessage>,
    sock: std::path::PathBuf,
    next_command: u32,
    _dir: tempfile::TempDir,
}

#[derive(Debug, Clone, PartialEq)]
struct Resolved {
    request_id: String,
    by: ResolvedBy,
    option_id: Option<String>,
    reason: Option<ResolveReason>,
    message: Option<String>,
}

async fn rig() -> Rig {
    let clock = Arc::new(AtomicI64::new(START_MS));
    let c = clock.clone();
    let adapter = ExternalAdapter::new(ExternalConfig {
        clock: Arc::new(move || c.load(Ordering::SeqCst)),
        tick: Duration::from_secs(3600),
    });
    let core = Core::new(test_machine(), vec![adapter.clone()]);
    adapter.start(core.clone());
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("hook.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let auth = Arc::new(HookAuth::new(Credential {
        token: TOKEN.into(),
        generation: 1,
        expires_at_ms: None,
    }));
    let app = adapter.router(auth);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_, rx) = core.attach(None);
    Rig {
        core,
        adapter,
        clock,
        rx,
        sock,
        next_command: 0,
        _dir: dir,
    }
}

async fn http(
    sock: &std::path::Path,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: &str,
) -> (u16, Value) {
    let mut stream = UnixStream::connect(sock).await.unwrap();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: acd\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(token) = token {
        request.push_str(&format!("X-Acd-Token: {token}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    let response = String::from_utf8_lossy(&bytes).into_owned();
    let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or("");
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

impl Rig {
    fn advance(&self, ms: i64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
    }

    fn now(&self) -> i64 {
        self.clock.load(Ordering::SeqCst)
    }

    async fn put(&self, pid: &str, body: Value) -> (u16, Value) {
        http(
            &self.sock,
            "PUT",
            &format!("/external/v1/producers/{pid}/state"),
            Some(TOKEN),
            &body.to_string(),
        )
        .await
    }

    async fn ok(&self, pid: &str, body: Value) -> Value {
        let (status, body) = self.put(pid, body).await;
        assert_eq!(status, 200, "{body}");
        body
    }

    async fn get_answers(&self, pid: &str, wait_ms: u64) -> (u16, Value) {
        http(
            &self.sock,
            "GET",
            &format!("/external/v1/producers/{pid}/answers?wait_ms={wait_ms}"),
            Some(TOKEN),
            "",
        )
        .await
    }

    async fn stop(&self, pid: &str) -> u16 {
        http(
            &self.sock,
            "DELETE",
            &format!("/external/v1/producers/{pid}"),
            Some(TOKEN),
            "",
        )
        .await
        .0
    }

    fn agents(&self) -> Vec<Agent> {
        match self.core.attach(None).0.into_iter().next() {
            Some(ServerMessage::Snapshot { agents, .. }) => agents
                .into_iter()
                .filter(|a| a.source == Source::External)
                .collect(),
            other => panic!("no snapshot: {other:?}"),
        }
    }

    fn agent(&self, title: &str) -> Option<Agent> {
        self.agents().into_iter().find(|a| a.title == title)
    }

    fn request(&self, title: &str) -> Request {
        self.agent(title)
            .and_then(|a| a.pending)
            .unwrap_or_else(|| panic!("no pending request on {title}"))
    }

    async fn answer(&mut self, request: &Request, option: &str) -> CommandResult {
        self.next_command += 1;
        self.core
            .handle(
                "client",
                ClientMessage::RequestAnswer {
                    id: format!("c{}", self.next_command),
                    request_id: request.id.clone(),
                    revision: Some(request.revision),
                    option_id: Some(option.into()),
                    text: None,
                },
            )
            .await
    }

    async fn answer_text(&mut self, request: &Request, text: &str) -> CommandResult {
        self.next_command += 1;
        self.core
            .handle(
                "client",
                ClientMessage::RequestAnswer {
                    id: format!("c{}", self.next_command),
                    request_id: request.id.clone(),
                    revision: Some(request.revision),
                    option_id: None,
                    text: Some(text.into()),
                },
            )
            .await
    }

    fn resolved(&mut self) -> Vec<Resolved> {
        let mut out = Vec::new();
        while let Ok(message) = self.rx.try_recv() {
            if let ServerMessage::RequestResolved {
                request_id,
                by,
                option_id,
                reason,
                message,
                ..
            } = message
            {
                out.push(Resolved {
                    request_id,
                    by,
                    option_id,
                    reason,
                    message,
                });
            }
        }
        out
    }

    fn snapshot(&self) -> Value {
        serde_json::to_value(self.agents()).unwrap()
    }
}

fn item(key: &str, agent_key: &str, agent_title: &str) -> Value {
    json!({
        "key": key,
        "agent_key": agent_key,
        "agent": {"title": agent_title, "project": "engine", "cwd": "/work/engine"},
        "title": format!("{agent_title} needs you"),
        "detail": "details",
        "question": {"header": "Parked", "text": "What now?"},
        "options": [
            {"id": "approve", "label": "Approve", "description": "go on"},
            {"id": "abandon", "label": "Abandon", "confirm": true}
        ],
        "free_text": false
    })
}

fn state(items: Vec<Value>) -> Value {
    json!({"lease_ms": LEASE, "items": items})
}

fn with_acks(items: Vec<Value>, acks: Vec<Value>) -> Value {
    json!({"lease_ms": LEASE, "items": items, "acks": acks})
}

fn ack(answer_id: &str, outcome: &str, message: Option<&str>) -> Value {
    match message {
        Some(m) => json!({"answer_id": answer_id, "outcome": outcome, "message": m}),
        None => json!({"answer_id": answer_id, "outcome": outcome}),
    }
}

fn only_answer(body: &Value) -> Value {
    let answers = body["answers"].as_array().unwrap();
    assert_eq!(answers.len(), 1, "{body}");
    answers[0].clone()
}

#[tokio::test]
async fn an_item_becomes_an_answerable_question_with_the_producers_identity() {
    let rig = rig().await;
    let body = rig
        .ok(
            "sup",
            state(vec![item("occ-1:aa", "parked:ENG-12", "autopilot")]),
        )
        .await;
    assert_eq!(
        body,
        json!({"answers": [], "refused": [], "ack_refused": []})
    );
    let agent = rig.agent("autopilot").expect("agent");
    assert_eq!(agent.source, Source::External);
    assert_eq!(agent.state, AgentState::NeedsYou);
    assert_eq!(agent.project.as_deref(), Some("engine"));
    assert_eq!(agent.cwd.as_deref(), Some("/work/engine"));
    assert!(!agent.can_send);
    let request = agent.pending.unwrap();
    assert_eq!(request.kind, RequestKind::Question);
    assert_eq!(request.status, RequestStatus::Open);
    assert_eq!(request.answer_path, AnswerPath::External);
    assert_eq!(request.title, "autopilot needs you");
    assert!(request.is_answerable());
    let questions = request.questions.as_ref().unwrap();
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].header.as_deref(), Some("Parked"));
    assert_eq!(questions[0].text, "What now?");
    assert!(!questions[0].multi_select);
    assert_eq!(questions[0].options, request.options);
    assert_eq!(request.options.len(), 2);
    assert!(request.options.iter().all(|o| o.effect == Effect::Choose));
    assert_eq!(
        request
            .options
            .iter()
            .map(|o| (o.id.as_str(), o.confirm))
            .collect::<Vec<_>>(),
        [("approve", false), ("abandon", true)]
    );
}

#[tokio::test]
async fn a_repeated_call_changes_nothing_and_a_changed_item_bumps_the_revision() {
    let mut rig = rig().await;
    let first = state(vec![item("k1", "a1", "autopilot")]);
    rig.ok("sup", first.clone()).await;
    let before = rig.request("autopilot");
    while rig.rx.try_recv().is_ok() {}
    rig.advance(10_000);
    rig.ok("sup", first).await;
    assert!(
        rig.rx.try_recv().is_err(),
        "a heartbeat must not emit events"
    );
    let same = rig.request("autopilot");
    assert_eq!(
        (same.id.clone(), same.revision),
        (before.id.clone(), before.revision)
    );

    let mut changed = item("k1", "a1", "autopilot");
    changed["detail"] = json!("new details");
    rig.ok("sup", state(vec![changed])).await;
    let after = rig.request("autopilot");
    assert_eq!(after.id, before.id, "same key, same request");
    assert_eq!(after.revision, before.revision + 1);
    assert_eq!(after.detail, "new details");
}

#[tokio::test]
async fn an_item_without_options_or_free_text_is_read_only() {
    let rig = rig().await;
    let mut it = item("k1", "a1", "autopilot");
    it["options"] = json!([]);
    rig.ok("sup", state(vec![it])).await;
    let request = rig.request("autopilot");
    assert_eq!(request.answer_path, AnswerPath::None);
    assert!(!request.is_answerable());

    let mut typed = item("k2", "a2", "typist");
    typed["options"] = json!([]);
    typed["free_text"] = json!(true);
    rig.ok("sup", state(vec![typed])).await;
    let request = rig.request("typist");
    assert_eq!(request.answer_path, AnswerPath::External);
    assert!(request.free_text && request.is_answerable());
}

#[tokio::test]
async fn a_client_answer_is_queued_for_the_producer_until_acked() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "autopilot")]))
        .await;
    let request = rig.request("autopilot");
    rig.advance(5_000);
    let answered_at = rig.now();
    let result = rig.answer(&request, "abandon").await;
    assert_eq!(result.outcome, Some(Outcome::Delivered), "{result:?}");
    assert_eq!(rig.request("autopilot").status, RequestStatus::Answering);

    rig.advance(1_000);
    let body = rig
        .ok("sup", state(vec![item("k1", "a1", "autopilot")]))
        .await;
    let answer = only_answer(&body);
    assert_eq!(answer["key"], "k1");
    assert_eq!(answer["revision"], request.revision);
    assert_eq!(answer["option_id"], "abandon");
    assert_eq!(answer["text"], Value::Null);
    assert_eq!(answer["expires_at_ms"], answered_at + 180_000);
    let id = answer["answer_id"].as_str().unwrap().to_owned();
    assert!(!id.is_empty());

    let (status, polled) = rig.get_answers("sup", 0).await;
    assert_eq!(status, 200);
    assert_eq!(
        only_answer(&polled)["answer_id"],
        id,
        "offered again on GET"
    );
    let again = rig
        .ok("sup", state(vec![item("k1", "a1", "autopilot")]))
        .await;
    assert_eq!(only_answer(&again)["answer_id"], id, "and on every PUT");
    assert!(rig.resolved().is_empty(), "nothing resolves before the ack");
}

#[tokio::test]
async fn the_answer_timeout_sets_the_expiry() {
    let mut rig = rig().await;
    let mut body = state(vec![item("k1", "a1", "autopilot")]);
    body["answer_timeout_ms"] = json!(45_000);
    rig.ok("sup", body.clone()).await;
    let request = rig.request("autopilot");
    let at = rig.now();
    rig.answer(&request, "approve").await;
    let answer = only_answer(&rig.ok("sup", body).await);
    assert_eq!(answer["expires_at_ms"], at + 45_000);
}

#[tokio::test]
async fn a_typed_answer_reaches_the_producer_as_text() {
    let mut rig = rig().await;
    let mut it = item("k1", "a1", "autopilot");
    it["free_text"] = json!(true);
    rig.ok("sup", state(vec![it.clone()])).await;
    let request = rig.request("autopilot");
    let result = rig.answer_text(&request, "use the smaller plan").await;
    assert_eq!(result.outcome, Some(Outcome::Delivered), "{result:?}");
    let answer = only_answer(&rig.ok("sup", state(vec![it])).await);
    assert_eq!(answer["text"], "use the smaller plan");
    assert_eq!(answer["option_id"], Value::Null);
}

#[tokio::test]
async fn ack_applied_resolves_by_inbox_with_the_chosen_option() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "autopilot")]))
        .await;
    let request = rig.request("autopilot");
    rig.answer(&request, "approve").await;
    let id = only_answer(&rig.get_answers("sup", 0).await.1)["answer_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = rig
        .ok(
            "sup",
            with_acks(vec![], vec![ack(&id, "applied", Some("ignored"))]),
        )
        .await;
    assert_eq!(body["answers"], json!([]));
    assert_eq!(
        rig.resolved(),
        [Resolved {
            request_id: request.id,
            by: ResolvedBy::Inbox,
            option_id: Some("approve".into()),
            reason: None,
            message: None,
        }]
    );
}

#[tokio::test]
async fn ack_rejected_resolves_by_agent_with_the_message() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "autopilot")]))
        .await;
    let request = rig.request("autopilot");
    rig.answer(&request, "approve").await;
    let id = only_answer(&rig.get_answers("sup", 0).await.1)["answer_id"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.ok(
        "sup",
        with_acks(
            vec![item("k2", "a1", "autopilot")],
            vec![ack(&id, "rejected", Some("The plan changed."))],
        ),
    )
    .await;
    assert_eq!(
        rig.resolved(),
        [Resolved {
            request_id: request.id,
            by: ResolvedBy::Agent,
            option_id: None,
            reason: Some(ResolveReason::Rejected),
            message: Some("The plan changed.".into()),
        }]
    );
    assert_eq!(rig.request("autopilot").status, RequestStatus::Open);
}

#[tokio::test]
async fn ack_failed_resolves_uncertain_with_the_message() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "autopilot")]))
        .await;
    let request = rig.request("autopilot");
    rig.answer(&request, "abandon").await;
    let id = only_answer(&rig.get_answers("sup", 0).await.1)["answer_id"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.ok(
        "sup",
        with_acks(vec![], vec![ack(&id, "failed", Some("crashed mid-action"))]),
    )
    .await;
    assert_eq!(
        rig.resolved(),
        [Resolved {
            request_id: request.id,
            by: ResolvedBy::Uncertain,
            option_id: None,
            reason: None,
            message: Some("crashed mid-action".into()),
        }]
    );
}

#[tokio::test]
async fn a_late_or_unknown_ack_is_refused_and_changes_nothing() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "autopilot")]))
        .await;
    let request = rig.request("autopilot");
    rig.answer(&request, "approve").await;
    let id = only_answer(&rig.get_answers("sup", 0).await.1)["answer_id"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.ok("sup", with_acks(vec![], vec![ack(&id, "applied", None)]))
        .await;
    assert_eq!(rig.resolved().len(), 1);

    let body = rig
        .ok(
            "sup",
            with_acks(
                vec![],
                vec![
                    ack(&id, "failed", Some("again")),
                    ack("never-issued", "applied", None),
                ],
            ),
        )
        .await;
    assert_eq!(
        body["ack_refused"],
        json!([
            {"answer_id": id, "reason": "resolved"},
            {"answer_id": "never-issued", "reason": "unknown"}
        ])
    );
    assert!(rig.resolved().is_empty(), "a refused ack resolves nothing");
}

#[tokio::test]
async fn an_expired_answer_resolves_uncertain_and_its_ack_comes_too_late() {
    let mut rig = rig().await;
    let it = item("k1", "a1", "autopilot");
    let mut body = state(vec![it.clone()]);
    body["answer_timeout_ms"] = json!(30_000);
    rig.ok("sup", body.clone()).await;
    let request = rig.request("autopilot");
    rig.answer(&request, "abandon").await;
    let id = only_answer(&rig.get_answers("sup", 0).await.1)["answer_id"]
        .as_str()
        .unwrap()
        .to_owned();

    rig.advance(29_999);
    rig.adapter.sweep();
    assert!(rig.resolved().is_empty(), "still inside the deadline");
    assert_eq!(
        only_answer(&rig.get_answers("sup", 0).await.1)["answer_id"],
        id
    );

    rig.advance(1);
    let (_, polled) = rig.get_answers("sup", 0).await;
    assert_eq!(
        polled["answers"],
        json!([]),
        "never offered past its deadline"
    );
    assert_eq!(
        rig.resolved(),
        [Resolved {
            request_id: request.id,
            by: ResolvedBy::Uncertain,
            option_id: None,
            reason: None,
            message: None,
        }]
    );
    let late = rig
        .ok(
            "sup",
            with_acks(vec![], vec![ack(&id, "failed", Some("expired"))]),
        )
        .await;
    assert_eq!(
        late["ack_refused"],
        json!([{"answer_id": id, "reason": "resolved"}])
    );
    assert!(rig.resolved().is_empty());
}

#[tokio::test]
async fn omission_resolves_open_by_agent_and_answering_uncertain() {
    let mut rig = rig().await;
    rig.ok(
        "sup",
        state(vec![item("k1", "a1", "alpha"), item("k2", "a2", "beta")]),
    )
    .await;
    let (alpha, beta) = (rig.request("alpha"), rig.request("beta"));
    rig.answer(&beta, "approve").await;
    rig.ok("sup", state(vec![])).await;
    let mut resolved = rig.resolved();
    resolved.sort_by_key(|r| r.request_id.clone());
    let by = |id: &str| resolved.iter().find(|r| r.request_id == id).unwrap().by;
    assert_eq!(by(&alpha.id), ResolvedBy::Agent);
    assert_eq!(by(&beta.id), ResolvedBy::Uncertain);
    assert_eq!(resolved.len(), 2);
    assert!(rig.agents().is_empty(), "an agent with no item is removed");
    assert_eq!(
        rig.get_answers("sup", 0).await.1["answers"],
        json!([]),
        "the dropped answer is no longer offered"
    );
}

#[tokio::test]
async fn an_ack_in_the_same_call_keeps_an_answering_request_out_of_the_diff() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    let alpha = rig.request("alpha");
    rig.answer(&alpha, "approve").await;
    let id = only_answer(&rig.get_answers("sup", 0).await.1)["answer_id"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.ok("sup", with_acks(vec![], vec![ack(&id, "applied", None)]))
        .await;
    let resolved = rig.resolved();
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        resolved[0].by,
        ResolvedBy::Inbox,
        "acks run before the diff"
    );
}

#[tokio::test]
async fn a_new_key_on_the_same_agent_resolves_the_old_request_by_agent() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    let old = rig.request("alpha");
    rig.ok("sup", state(vec![item("k2", "a1", "alpha")])).await;
    let new = rig.request("alpha");
    assert_ne!(new.id, old.id);
    let resolved = rig.resolved();
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        (resolved[0].request_id.as_str(), resolved[0].by),
        (old.id.as_str(), ResolvedBy::Agent)
    );
    let body = rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    assert_eq!(
        body["refused"],
        json!([{"key": "k1", "reason": "resolved"}]),
        "the replaced key never reopens"
    );
}

#[tokio::test]
async fn replacing_an_answering_request_resolves_it_uncertain() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    let old = rig.request("alpha");
    rig.answer(&old, "abandon").await;
    rig.ok("sup", state(vec![item("k2", "a1", "alpha")])).await;
    let resolved = rig.resolved();
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        (resolved[0].request_id.as_str(), resolved[0].by),
        (old.id.as_str(), ResolvedBy::Uncertain)
    );
    assert_eq!(rig.get_answers("sup", 0).await.1["answers"], json!([]));
}

#[tokio::test]
async fn a_tombstoned_key_is_refused_while_the_other_items_still_apply() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    let request = rig.request("alpha");
    rig.answer(&request, "approve").await;
    let id = only_answer(&rig.get_answers("sup", 0).await.1)["answer_id"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.ok("sup", with_acks(vec![], vec![ack(&id, "applied", None)]))
        .await;
    rig.resolved();

    let body = rig
        .ok(
            "sup",
            state(vec![item("k1", "a1", "alpha"), item("k9", "a9", "omega")]),
        )
        .await;
    assert_eq!(
        body["refused"],
        json!([{"key": "k1", "reason": "resolved"}])
    );
    assert!(
        rig.agent("alpha").is_none(),
        "the refused item opens nothing"
    );
    assert_eq!(rig.request("omega").status, RequestStatus::Open);
    assert!(rig.resolved().is_empty());
}

#[tokio::test]
async fn a_request_the_registry_resolved_on_its_own_is_tombstoned_too() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    let request = rig.request("alpha");
    rig.core.observe(acd_core::Observation::RequestEndedById {
        request_id: request.id.clone(),
        by: ResolvedBy::Uncertain,
    });
    let body = rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    assert_eq!(
        body["refused"],
        json!([{"key": "k1", "reason": "resolved"}])
    );
    let resolved = rig.resolved();
    assert_eq!(
        resolved.len(),
        1,
        "only the original resolution: no new request opened"
    );
    assert!(rig.agent("alpha").is_none());
}

#[tokio::test]
async fn an_answer_for_a_request_resolved_elsewhere_is_no_longer_offered() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    let request = rig.request("alpha");
    rig.answer(&request, "approve").await;
    let id = only_answer(&rig.get_answers("sup", 0).await.1)["answer_id"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.core.observe(acd_core::Observation::RequestEndedById {
        request_id: request.id,
        by: ResolvedBy::Uncertain,
    });
    assert_eq!(rig.get_answers("sup", 0).await.1["answers"], json!([]));
    let late = rig
        .ok("sup", with_acks(vec![], vec![ack(&id, "applied", None)]))
        .await;
    assert_eq!(
        late["ack_refused"],
        json!([{"answer_id": id, "reason": "resolved"}])
    );
    assert_eq!(rig.resolved().len(), 1);
}

#[tokio::test]
async fn tombstones_keep_the_most_recent_4096_keys_per_producer() {
    let rig = rig().await;
    let total = 4096 + 64;
    let mut n = 0;
    while n < total {
        let batch: Vec<Value> = (n..(n + 32).min(total))
            .map(|i| item(&format!("k{i}"), &format!("a{i}"), "bulk"))
            .collect();
        n += batch.len();
        rig.ok("sup", state(batch)).await;
        rig.ok("sup", state(vec![])).await;
    }
    let body = rig
        .ok(
            "sup",
            state(vec![
                item(&format!("k{}", total - 1), "x1", "newest"),
                item("k0", "x2", "oldest"),
            ]),
        )
        .await;
    assert_eq!(
        body["refused"],
        json!([{"key": format!("k{}", total - 1), "reason": "resolved"}]),
        "the newest tombstone holds"
    );
    assert!(
        rig.agent("oldest").is_some(),
        "the oldest tombstone was forgotten beyond the bound"
    );
}

#[tokio::test]
async fn lease_expiry_ends_the_agents_and_drops_queued_answers() {
    let mut rig = rig().await;
    rig.ok(
        "sup",
        state(vec![item("k1", "a1", "alpha"), item("k2", "a2", "beta")]),
    )
    .await;
    let (alpha, beta) = (rig.request("alpha"), rig.request("beta"));
    rig.answer(&beta, "abandon").await;
    let id = only_answer(&rig.get_answers("sup", 0).await.1)["answer_id"]
        .as_str()
        .unwrap()
        .to_owned();

    rig.advance(LEASE as i64 - 1);
    rig.adapter.sweep();
    assert!(
        rig.resolved().is_empty(),
        "the lease holds until its last ms"
    );
    rig.advance(1);
    rig.adapter.sweep();
    let resolved = rig.resolved();
    let by = |id: &str| resolved.iter().find(|r| r.request_id == id).unwrap().by;
    assert_eq!(by(&alpha.id), ResolvedBy::Lost);
    assert_eq!(by(&beta.id), ResolvedBy::Uncertain);
    assert!(rig.agents().is_empty());
    assert_eq!(rig.get_answers("sup", 0).await.1["answers"], json!([]));

    let body = rig
        .ok(
            "sup",
            with_acks(
                vec![item("k1", "a1", "alpha")],
                vec![ack(&id, "applied", None)],
            ),
        )
        .await;
    assert_eq!(
        body["refused"],
        json!([{"key": "k1", "reason": "resolved"}]),
        "tombstones outlive the lease"
    );
    assert_eq!(
        body["ack_refused"],
        json!([{"answer_id": id, "reason": "resolved"}])
    );
    rig.ok("sup", state(vec![item("k3", "a3", "gamma")])).await;
    assert_eq!(
        rig.request("gamma").status,
        RequestStatus::Open,
        "a later PUT starts a new lease"
    );
}

#[tokio::test]
async fn an_answer_after_the_lease_ended_is_not_delivered() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    let request = rig.request("alpha");
    rig.advance(LEASE as i64);
    let result = rig.answer(&request, "approve").await;
    assert_eq!(
        result.error_code(),
        Some(ErrorCode::SourceUnavailable),
        "{result:?}"
    );
    assert_eq!(
        rig.request("alpha").status,
        RequestStatus::Open,
        "nothing was queued, so the request is open again"
    );
}

#[tokio::test]
async fn get_and_invalid_calls_do_not_renew_the_lease() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    rig.advance(LEASE as i64 - 10);
    rig.get_answers("sup", 0).await;
    let mut bad = state(vec![item("k1", "a1", "alpha"), item("k1", "a2", "dup")]);
    bad["lease_ms"] = json!(900_000);
    assert_eq!(rig.put("sup", bad).await.0, 422);
    rig.advance(10);
    rig.adapter.sweep();
    let resolved = rig.resolved();
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        resolved[0].by,
        ResolvedBy::Lost,
        "neither call extended the lease"
    );
}

#[tokio::test]
async fn delete_ends_the_lease_like_expiry() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    let request = rig.request("alpha");
    assert_eq!(rig.stop("sup").await, 204);
    assert_eq!(
        rig.resolved()
            .iter()
            .map(|r| (r.request_id.clone(), r.by))
            .collect::<Vec<_>>(),
        [(request.id, ResolvedBy::Lost)]
    );
    assert!(rig.agents().is_empty());
    assert_eq!(rig.stop("sup").await, 204, "stopping twice is fine");
    assert_eq!(rig.stop("never-seen").await, 204);
}

#[tokio::test]
async fn a_long_poll_returns_as_soon_as_an_answer_is_queued() {
    let mut rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    let request = rig.request("alpha");
    let sock = rig.sock.clone();
    let waiting = tokio::spawn(async move {
        let started = std::time::Instant::now();
        let (status, body) = http(
            &sock,
            "GET",
            "/external/v1/producers/sup/answers?wait_ms=20000",
            Some(TOKEN),
            "",
        )
        .await;
        (status, body, started.elapsed())
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    rig.answer(&request, "approve").await;
    let (status, body, took) = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the poll ends when the answer arrives")
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(only_answer(&body)["option_id"], "approve");
    assert!(took < Duration::from_secs(5), "{took:?}");
}

#[tokio::test]
async fn a_long_poll_with_nothing_queued_returns_empty_at_its_deadline() {
    let rig = rig().await;
    rig.ok("sup", state(vec![item("k1", "a1", "alpha")])).await;
    let started = std::time::Instant::now();
    let (status, body) = rig.get_answers("sup", 300).await;
    assert_eq!((status, &body["answers"]), (200, &json!([])));
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "{:?}",
        started.elapsed()
    );
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn bodies_that_break_a_rule_get_422_and_change_nothing() {
    let mut rig = rig().await;
    rig.ok(
        "sup",
        state(vec![item("k1", "a1", "alpha"), item("k2", "a2", "beta")]),
    )
    .await;
    let (alpha, beta) = (rig.request("alpha"), rig.request("beta"));
    rig.answer(&beta, "approve").await;
    let queued = only_answer(&rig.get_answers("sup", 0).await.1);
    let id = queued["answer_id"].as_str().unwrap().to_owned();
    let before = rig.snapshot();
    rig.resolved();

    let valid_ack = ack(&id, "applied", None);
    let long = |n: usize| "x".repeat(n);
    let mut too_many_options = item("k3", "a3", "gamma");
    too_many_options["options"] = (0..9)
        .map(|i| json!({"id": format!("o{i}"), "label": "L"}))
        .collect();
    let mut dup_option = item("k3", "a3", "gamma");
    dup_option["options"] = json!([{"id": "o", "label": "L"}, {"id": "o", "label": "M"}]);
    let with = |f: &dyn Fn(&mut Value)| {
        let mut it = item("k3", "a3", "gamma");
        f(&mut it);
        it
    };
    let cases: Vec<(&str, Value)> = vec![
        (
            "duplicate key",
            with_acks(
                vec![item("k3", "a3", "g"), item("k3", "a4", "g")],
                vec![valid_ack.clone()],
            ),
        ),
        (
            "duplicate agent_key",
            with_acks(
                vec![item("k3", "a3", "g"), item("k4", "a3", "g")],
                vec![valid_ack.clone()],
            ),
        ),
        (
            "33 items",
            with_acks(
                (0..33)
                    .map(|i| item(&format!("n{i}"), &format!("m{i}"), "g"))
                    .collect(),
                vec![valid_ack.clone()],
            ),
        ),
        (
            "9 options",
            with_acks(vec![too_many_options], vec![valid_ack.clone()]),
        ),
        (
            "duplicate option id",
            with_acks(vec![dup_option], vec![valid_ack.clone()]),
        ),
        (
            "long title",
            with_acks(
                vec![with(&|i| i["title"] = json!(long(201)))],
                vec![valid_ack.clone()],
            ),
        ),
        (
            "long detail",
            with_acks(
                vec![with(&|i| i["detail"] = json!(long(4001)))],
                vec![valid_ack.clone()],
            ),
        ),
        (
            "long label",
            with_acks(
                vec![with(&|i| i["options"][0]["label"] = json!(long(81)))],
                vec![valid_ack.clone()],
            ),
        ),
        (
            "long option description",
            with_acks(
                vec![with(&|i| i["options"][0]["description"] = json!(long(201)))],
                vec![valid_ack.clone()],
            ),
        ),
        (
            "long question",
            with_acks(
                vec![with(&|i| i["question"]["text"] = json!(long(1001)))],
                vec![valid_ack.clone()],
            ),
        ),
        (
            "long ack message",
            with_acks(
                vec![],
                vec![valid_ack.clone(), ack("zz", "failed", Some(&long(501)))],
            ),
        ),
        (
            "control character",
            with_acks(
                vec![with(&|i| i["title"] = json!("a\u{1b}b"))],
                vec![valid_ack.clone()],
            ),
        ),
        (
            "ack control character",
            with_acks(
                vec![],
                vec![valid_ack.clone(), ack("zz", "failed", Some("a\u{7}b"))],
            ),
        ),
        (
            "bad key",
            with_acks(
                vec![with(&|i| i["key"] = json!("has space"))],
                vec![valid_ack.clone()],
            ),
        ),
        (
            "bad agent key",
            with_acks(
                vec![with(&|i| i["agent_key"] = json!(""))],
                vec![valid_ack.clone()],
            ),
        ),
        (
            "33 acks",
            with_acks(
                vec![],
                (0..33)
                    .map(|i| ack(&format!("z{i}"), "failed", None))
                    .collect(),
            ),
        ),
        (
            "lease too short",
            json!({"lease_ms": 29_999, "acks": [valid_ack.clone()]}),
        ),
        (
            "lease too long",
            json!({"lease_ms": 900_001, "acks": [valid_ack.clone()]}),
        ),
        (
            "answer timeout too short",
            json!({"lease_ms": LEASE, "answer_timeout_ms": 29_999, "acks": [valid_ack.clone()]}),
        ),
        (
            "answer timeout too long",
            json!({"lease_ms": LEASE, "answer_timeout_ms": 900_001, "acks": [valid_ack.clone()]}),
        ),
        ("missing lease", json!({"acks": [valid_ack.clone()]})),
        (
            "unknown outcome",
            json!({"lease_ms": LEASE, "acks": [{"answer_id": id, "outcome": "maybe"}]}),
        ),
        (
            "key bound to another agent",
            with_acks(vec![item("k1", "other", "alpha")], vec![valid_ack.clone()]),
        ),
    ];
    for (name, body) in cases {
        let (status, response) = rig.put("sup", body).await;
        assert_eq!(status, 422, "{name}: {response}");
        assert!(response["error"].is_string(), "{name}");
        assert_eq!(rig.snapshot(), before, "{name}: agents changed");
        assert!(rig.resolved().is_empty(), "{name}: a request resolved");
    }
    let sock = rig.sock.clone();
    let raw = |text: String| {
        let sock = sock.clone();
        async move {
            http(
                &sock,
                "PUT",
                "/external/v1/producers/sup/state",
                Some(TOKEN),
                &text,
            )
            .await
        }
    };
    let valid = with_acks(vec![], vec![valid_ack.clone()]).to_string();
    for (name, text) in [
        ("truncated", valid[..valid.len() - 3].to_owned()),
        ("not json", "nope".to_owned()),
        ("empty", String::new()),
        (
            "over 256 KiB",
            format!(
                "{{\"lease_ms\":60000,\"pad\":\"{}\"}}",
                "p".repeat(256 * 1024)
            ),
        ),
    ] {
        let (status, _) = raw(text).await;
        assert_eq!(status, 422, "{name}");
        assert_eq!(rig.snapshot(), before, "{name}");
        assert!(rig.resolved().is_empty(), "{name}");
    }
    assert_eq!(rig.request("alpha"), alpha, "alpha is untouched");
    assert_eq!(rig.request("beta").status, RequestStatus::Answering);
    let still = only_answer(&rig.get_answers("sup", 0).await.1);
    assert_eq!(
        still["answer_id"], id,
        "no ack applied from any rejected body"
    );
    let ok = rig
        .ok(
            "sup",
            with_acks(vec![item("k1", "a1", "alpha")], vec![valid_ack]),
        )
        .await;
    assert_eq!(ok["ack_refused"], json!([]));
    let resolved = rig.resolved();
    assert_eq!(resolved.len(), 1, "only the valid call resolved anything");
    assert_eq!(
        (resolved[0].request_id.as_str(), resolved[0].by),
        (beta.id.as_str(), ResolvedBy::Inbox)
    );
}

#[tokio::test]
async fn a_ninth_live_producer_is_refused_until_one_stops() {
    let rig = rig().await;
    for i in 0..8 {
        rig.ok(
            &format!("p{i}"),
            state(vec![item("k", "a", &format!("agent{i}"))]),
        )
        .await;
    }
    let (status, _) = rig.put("p8", state(vec![item("k", "a", "late")])).await;
    assert_eq!(status, 422);
    assert!(rig.agent("late").is_none());
    assert!(rig.agent("agent0").is_some());
    rig.ok("p0", state(vec![item("k2", "a", "agent0b")])).await;
    assert_eq!(rig.stop("p1").await, 204);
    rig.ok("p8", state(vec![item("k", "a", "late")])).await;
    assert!(rig.agent("late").is_some());
}

#[tokio::test]
async fn producers_are_isolated_from_each_other() {
    let mut rig = rig().await;
    rig.ok("one", state(vec![item("k", "a", "first")])).await;
    rig.ok("two", state(vec![item("k", "a", "second")])).await;
    assert_ne!(rig.request("first").id, rig.request("second").id);
    rig.ok("one", state(vec![])).await;
    assert!(rig.agent("first").is_none());
    assert_eq!(rig.request("second").status, RequestStatus::Open);
    assert_eq!(rig.resolved().len(), 1);
}

#[tokio::test]
async fn calls_need_the_hook_token_the_right_method_and_a_valid_producer_id() {
    let rig = rig().await;
    let path = "/external/v1/producers/sup/state";
    let body = state(vec![item("k1", "a1", "alpha")]).to_string();
    assert_eq!(http(&rig.sock, "PUT", path, None, &body).await.0, 401);
    assert_eq!(
        http(&rig.sock, "PUT", path, Some("wrong"), &body).await.0,
        401
    );
    assert_eq!(
        http(&rig.sock, "POST", path, Some(TOKEN), &body).await.0,
        401
    );
    assert_eq!(
        http(
            &rig.sock,
            "GET",
            "/external/v1/producers/sup/answers",
            None,
            ""
        )
        .await
        .0,
        401
    );
    assert_eq!(
        http(&rig.sock, "DELETE", "/external/v1/producers/sup", None, "")
            .await
            .0,
        401
    );
    assert!(
        rig.agents().is_empty(),
        "unauthenticated calls change nothing"
    );
    for bad in ["Upper", "-lead", "under_score", &"a".repeat(33)] {
        let (status, _) = http(
            &rig.sock,
            "PUT",
            &format!("/external/v1/producers/{bad}/state"),
            Some(TOKEN),
            &body,
        )
        .await;
        assert_eq!(status, 422, "{bad}");
    }
    for wait in ["25001", "abc", "-1"] {
        let (status, _) = http(
            &rig.sock,
            "GET",
            &format!("/external/v1/producers/sup/answers?wait_ms={wait}"),
            Some(TOKEN),
            "",
        )
        .await;
        assert_eq!(status, 422, "wait_ms={wait}");
    }
}

#[tokio::test]
async fn titles_details_answers_and_ack_messages_never_reach_the_logs() {
    #[derive(Clone, Default)]
    struct Buffer(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish(),
    );

    let mut rig = rig().await;
    let mut it = item("log-key", "a1", "SECRET-AGENT-TITLE");
    it["title"] = json!("SECRET-ITEM-TITLE");
    it["detail"] = json!("SECRET-DETAIL");
    it["question"]["text"] = json!("SECRET-QUESTION");
    it["options"][0]["label"] = json!("SECRET-LABEL");
    it["free_text"] = json!(true);
    rig.ok("sup", state(vec![it.clone()])).await;
    let request = rig.request("SECRET-AGENT-TITLE");
    rig.answer_text(&request, "SECRET-ANSWER-TEXT").await;
    let id = only_answer(&rig.get_answers("sup", 0).await.1)["answer_id"]
        .as_str()
        .unwrap()
        .to_owned();
    rig.ok(
        "sup",
        with_acks(
            vec![it],
            vec![ack(&id, "rejected", Some("SECRET-ACK-MESSAGE"))],
        ),
    )
    .await;
    let (status, _) = rig
        .put(
            "sup",
            state(vec![
                json!({"key": "bad key", "agent_key": "a", "title": "SECRET-INVALID-TITLE"}),
            ]),
        )
        .await;
    assert_eq!(status, 422);
    rig.stop("sup").await;

    let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("sup"), "the producer id is logged: {logs}");
    assert!(logs.contains("log-key"), "the key is logged: {logs}");
    assert!(!logs.contains("SECRET"), "{logs}");
}
