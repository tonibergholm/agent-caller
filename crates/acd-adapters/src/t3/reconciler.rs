//! Per-server T3 reconciler (spec §6.5): keeps the registry in step with one
//! T3 server. One task owns all sync state. It subscribes to the shell,
//! resumes from its cursor after a reconnect, and opens a bounded set of
//! thread streams: request streams, working streams and one discovery
//! stream that learns provider session ids for ownership rule 1.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use acd_core::{AgentObs, Core, CorrelationKey, Observation};
use acd_protocol::{AgentKind, AgentState, ResolvedBy, Source, SourceState, SourceStatus};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep};

use super::conn::{self, Conn, ConnTiming, ConnectError, StreamMsg};
use super::deferred_ends::DeferredEnds;
use super::detail::ThreadDetail;
use super::map;
use super::sessions::SessionMap;
use super::targets::Targets;
use super::wire::{self, Failure, Project, ShellItem, ShellSnapshot, ShellThread, ThreadItem};
use crate::text::redact_log;

/// Most consecutive discovery picks from the overflow group while the recent
/// group has an eligible candidate.
pub const OVERFLOW_BURST: usize = 3;
/// Most thread streams one policy pass opens.
pub const OPEN_PER_PASS: usize = 16;

#[derive(Clone, Debug)]
pub struct T3Timing {
    pub conn: ConnTiming,
    pub backoff_start: Duration,
    pub backoff_max: Duration,
    pub tick: Duration,
    /// How long a request or working stream stays open once unneeded.
    pub release_after: Duration,
    /// Most working streams.
    pub max_live: usize,
    /// Idle Claude and Codex threads updated this recently are discovered.
    /// `ZERO` disables only this group.
    pub learn_window: Duration,
    pub relearn_after: Duration,
    /// Every stream's first `synchronized` is due this long after it opens.
    pub sync_timeout: Duration,
    /// Unreadable items, or a problem the adapter reported, keep the source
    /// `degraded` until this long passes without a new one.
    pub degraded_for: Duration,
}

impl Default for T3Timing {
    fn default() -> Self {
        T3Timing {
            conn: ConnTiming::default(),
            backoff_start: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
            tick: Duration::from_secs(1),
            release_after: Duration::from_secs(10),
            max_live: 32,
            learn_window: Duration::from_secs(24 * 60 * 60),
            relearn_after: Duration::from_secs(60),
            sync_timeout: Duration::from_secs(15),
            degraded_for: Duration::from_secs(10 * 60),
        }
    }
}

/// The adapter-to-reconciler channel. It never drops an entry: answered
/// pairs coalesce in a set and the degraded message is one slot.
#[derive(Default)]
pub struct Mailbox {
    inner: Mutex<MailboxInner>,
    notify: Notify,
}

#[derive(Default)]
struct MailboxInner {
    answered: HashSet<(String, String)>,
    ended: Vec<(String, ResolvedBy)>,
    degraded: Option<&'static str>,
}

impl Mailbox {
    #[cfg(any(test, feature = "testing"))]
    pub fn fill_for_tests(&self, count: usize) {
        let mut inner = self.inner.lock().expect("mailbox");
        for i in 0..count {
            inner
                .answered
                .insert((format!("ghost{i}"), "rq-x".to_owned()));
        }
        self.notify.notify_one();
    }

    pub fn answered(&self, thread_id: &str, t3_request_id: &str) {
        self.inner
            .lock()
            .expect("mailbox")
            .answered
            .insert((thread_id.to_owned(), t3_request_id.to_owned()));
        self.notify.notify_one();
    }

    pub fn degraded(&self, msg: &'static str) {
        self.inner.lock().expect("mailbox").degraded = Some(msg);
        self.notify.notify_one();
    }

    pub fn request_ended(&self, request_id: String, by: ResolvedBy) {
        self.inner
            .lock()
            .expect("mailbox")
            .ended
            .push((request_id, by));
        self.notify.notify_one();
    }

    #[doc(hidden)]
    pub fn take(&self) -> (Vec<(String, String)>, Option<&'static str>) {
        let mut inner = self.inner.lock().expect("mailbox");
        (inner.answered.drain().collect(), inner.degraded.take())
    }

    fn take_ended(&self) -> Vec<(String, ResolvedBy)> {
        std::mem::take(&mut self.inner.lock().expect("mailbox").ended)
    }

    /// A notification sent before this is awaited is kept as a permit.
    pub(crate) async fn notified(&self) {
        self.notify.notified().await
    }
}

#[derive(Clone, Debug, Default)]
pub struct SyncStats {
    pub details: usize,
    pub requests: usize,
    pub items: usize,
    pub targets: usize,
    pub request_streams: usize,
    pub working_streams: usize,
    pub discovery_streams: usize,
    pub peak_working_streams: usize,
    pub peak_discovery_streams: usize,
}

/// Held in an `Arc` by the reconciler and the adapter.
#[derive(Default)]
pub struct Shared {
    pub conn: Mutex<Option<Conn>>,
    /// Agent id -> thread id.
    pub threads: RwLock<HashMap<String, String>>,
    pub sessions: Arc<SessionMap>,
    pub mailbox: Mailbox,
    pub targets: Targets,
    pub stats: Mutex<SyncStats>,
}

impl Shared {
    pub fn current_conn(&self) -> Option<Conn> {
        self.conn.lock().expect("conn").clone()
    }

    pub fn stats(&self) -> SyncStats {
        self.stats.lock().expect("stats").clone()
    }
}

pub struct SyncConfig {
    pub ws_url: Result<String, String>,
    pub token_path: PathBuf,
    pub timing: T3Timing,
}

pub struct SyncHandle {
    task: JoinHandle<()>,
}

impl SyncHandle {
    pub async fn shutdown(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

pub fn spawn(cfg: SyncConfig, core: Arc<Core>, shared: Arc<Shared>) -> SyncHandle {
    SyncHandle {
        task: tokio::spawn(run(cfg, core, shared)),
    }
}

/// Closes the published connection when the task ends, aborted or not: the
/// adapter may hold clones, so dropping ours would not close it.
struct ConnSlot(Arc<Shared>);

impl Drop for ConnSlot {
    fn drop(&mut self) {
        let conn = match self.0.conn.lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => None,
        };
        if let Some(c) = conn {
            c.close("acd stopped the T3 sync");
        }
    }
}

async fn run(cfg: SyncConfig, core: Arc<Core>, shared: Arc<Shared>) {
    let _slot = ConnSlot(shared.clone());
    let mut s = Reconciler::new(core, shared, cfg.timing.clone());
    let mut backoff = cfg.timing.backoff_start;
    loop {
        let url = match &cfg.ws_url {
            Ok(u) => u.clone(),
            Err(why) => {
                // `why` is acd's own text (`conn::ws_url`).
                s.status(SourceState::Unconfigured, Some(why.clone()));
                return;
            }
        };
        let token = match read_token(&cfg.token_path) {
            Ok(t) => t,
            Err(why) => {
                s.status(SourceState::Unconfigured, Some(why));
                sleep(cfg.timing.backoff_max).await;
                continue;
            }
        };
        match conn::connect(&url, &token, &cfg.timing.conn).await {
            Err(e) => {
                // Fixed messages (Task 3).
                s.status(SourceState::Degraded, Some(e.to_string()));
                let wait = if matches!(e, ConnectError::Unreachable) {
                    backoff
                } else {
                    cfg.timing.backoff_max
                };
                sleep(wait).await;
                backoff = (backoff * 2).min(cfg.timing.backoff_max);
            }
            Ok((conn, mut events)) => {
                let synced = s.serve(conn, &mut events).await;
                if synced {
                    backoff = cfg.timing.backoff_start;
                }
                s.connection_lost();
                sleep(backoff).await;
                backoff = (backoff * 2).min(cfg.timing.backoff_max);
            }
        }
    }
}

/// The trimmed token. Messages never include the token.
fn read_token(path: &Path) -> Result<String, String> {
    use std::os::unix::fs::PermissionsExt;
    let missing = || {
        format!(
            "T3 token missing: save `t3 auth session issue` output to {} with mode 0600",
            path.display()
        )
    };
    let unreadable = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::NotFound {
            missing()
        } else {
            "t3.token cannot be read".to_owned()
        }
    };
    let meta = std::fs::metadata(path).map_err(unreadable)?;
    if meta.permissions().mode() & 0o077 != 0 {
        return Err("t3.token must be mode 0600".into());
    }
    let bytes = std::fs::read(path).map_err(unreadable)?;
    let invalid = || "t3.token is not a valid token".to_owned();
    let text = String::from_utf8(bytes).map_err(|_| invalid())?;
    let token = text.trim();
    if token.is_empty() || !token.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(invalid());
    }
    Ok(token.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// Mandatory: the thread has a pending or surfaced request.
    Request,
    /// One of at most `max_live` working threads.
    Working,
    /// Opened only to learn the session id; at most one.
    Discovery,
}

/// Session discovery, keyed by the active provider thread identity.
#[derive(Clone, Debug, PartialEq)]
enum Discovery {
    /// Never looked, or the shell's `activeProviderThreadId` changed since.
    Unknown,
    /// Looked while no native id could be published. `provider_thread` is
    /// the shell's active provider thread at that time. Retried after
    /// `relearn_after`, or at once when the entry of an idle thread changes
    /// or the shell names another provider thread.
    NotLoaded {
        provider_thread: Option<String>,
        at: Instant,
    },
    Known {
        provider_thread: String,
    },
}

struct ThreadState {
    /// The last entry that decoded. An unreadable entry leaves it in place
    /// (it is not identity evidence and does not change the stream class).
    shell: Option<ShellThread>,
    /// The last entry did not decode: the agent shows `unknown`.
    unreadable: bool,
    /// The last observation sent, for `connection_lost` and bad entries.
    last_obs: Option<AgentObs>,
    detail: Option<ThreadDetail>,
    /// Thread stream cursor.
    cursor: Option<u64>,
    /// Open stream id on this connection, and its class.
    stream: Option<String>,
    class: Option<Class>,
    unneeded_since: Option<Instant>,
    /// First `synchronized` due by then.
    sync_deadline: Option<Instant>,
    /// Published (provider thread, native session id).
    owner: Option<(String, String)>,
    /// Skipped by every class until then.
    stalled_until: Option<Instant>,
    discovery: Discovery,
    /// When a discovery stream last opened for this thread.
    checked_at: Option<Instant>,
    /// Answered request ids (D9).
    hidden: HashSet<String>,
    /// Requests ended by thread detail while the shell still lags.
    ended: HashSet<String>,
    /// Surfaced T3 request id (Task 6).
    open: Option<String>,
}

impl ThreadState {
    fn new() -> ThreadState {
        ThreadState {
            shell: None,
            unreadable: false,
            last_obs: None,
            detail: None,
            cursor: None,
            stream: None,
            class: None,
            unneeded_since: None,
            sync_deadline: None,
            owner: None,
            stalled_until: None,
            discovery: Discovery::Unknown,
            checked_at: None,
            hidden: HashSet::new(),
            ended: HashSet::new(),
            open: None,
        }
    }

    fn stalled(&self, now: Instant) -> bool {
        self.stalled_until.is_some_and(|u| now < u)
    }

    fn wants_request_stream(&self) -> bool {
        self.open.is_some()
            || self
                .shell
                .as_ref()
                .is_some_and(|s| s.pending_runtime_request.is_some())
    }

    fn is_working(&self) -> bool {
        self.shell
            .as_ref()
            .is_some_and(|s| map::shell_state(s, false).0 == AgentState::Working)
    }

    fn updated(&self) -> Option<DateTime<Utc>> {
        let s = self.shell.as_ref()?.updated_at.as_deref()?;
        DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|d| d.with_timezone(&Utc))
    }
}

struct Reconciler {
    core: Arc<Core>,
    shared: Arc<Shared>,
    timing: T3Timing,
    shell_cursor: Option<u64>,
    shell_stream: Option<String>,
    shell_deadline: Option<Instant>,
    /// The shell delivered a value on this connection; thread streams wait
    /// for it so they are chosen from current shell state.
    shell_seen: bool,
    shell_synced: bool,
    projects: HashMap<String, Project>,
    threads: HashMap<String, ThreadState>,
    /// Shell entries of hidden threads (subagent, settled, snoozed): no agent
    /// and no streams, kept to notice a snooze ending or a thread needing us.
    hidden_shells: HashMap<String, ShellThread>,
    /// Session claims kept for threads that became hidden after acd learned
    /// their (provider thread, session id): the Codex hooks adapter still
    /// sees them as T3-owned and creates no duplicate agent.
    hidden_owners: HashMap<String, (String, String)>,
    /// Stream id -> thread id.
    streams: HashMap<String, String>,
    /// Unreadable items since the source was last clear of them.
    decode_errors: usize,
    last_decode_error: Option<Instant>,
    /// A problem the adapter reported through the mailbox, and when.
    adapter_degraded: Option<(&'static str, Instant)>,
    overflow_streak: usize,
    peak_working: usize,
    peak_discovery: usize,
    pending_ends: DeferredEnds,
}

fn unreadable_detail(v: &Value) -> Value {
    wire::value_fingerprint(v)
}

fn log_unreadable(what: &str, v: &Value) {
    tracing::debug!(item = %redact_log(&unreadable_detail(v).to_string()), "T3 {what} cannot be read");
}

impl Reconciler {
    fn new(core: Arc<Core>, shared: Arc<Shared>, timing: T3Timing) -> Reconciler {
        Reconciler {
            core,
            shared,
            timing,
            shell_cursor: None,
            shell_stream: None,
            shell_deadline: None,
            shell_seen: false,
            shell_synced: false,
            projects: HashMap::new(),
            threads: HashMap::new(),
            hidden_shells: HashMap::new(),
            hidden_owners: HashMap::new(),
            streams: HashMap::new(),
            decode_errors: 0,
            last_decode_error: None,
            adapter_degraded: None,
            overflow_streak: 0,
            peak_working: 0,
            peak_discovery: 0,
            pending_ends: DeferredEnds::default(),
        }
    }

    fn status(&self, state: SourceState, message: Option<String>) {
        self.core
            .set_source_status(Source::T3, SourceStatus { state, message });
    }

    /// The status of a synchronized connection. Until the shell
    /// synchronizes, the connection status set by `run` stands.
    fn refresh_status(&self) {
        if !self.shell_synced {
            return;
        }
        if let Some((msg, _)) = self.adapter_degraded {
            self.status(SourceState::Degraded, Some(msg.to_owned()));
        } else if self.decode_errors > 0 {
            self.status(
                SourceState::Degraded,
                Some(format!(
                    "{} T3 items could not be read (see the debug log)",
                    self.decode_errors
                )),
            );
        } else {
            self.status(SourceState::Ok, None);
        }
    }

    /// Clears degradations that did not recur within `degraded_for`.
    fn expire_degradations(&mut self, now: Instant) {
        let old = |at: Instant| now.duration_since(at) >= self.timing.degraded_for;
        let mut changed = false;
        if self.last_decode_error.is_some_and(old) {
            self.decode_errors = 0;
            self.last_decode_error = None;
            changed = true;
        }
        if self.adapter_degraded.is_some_and(|(_, at)| old(at)) {
            self.adapter_degraded = None;
            changed = true;
        }
        if changed {
            self.refresh_status();
        }
    }

    /// Returns whether the shell synchronized on this connection.
    async fn serve(&mut self, conn: Conn, events: &mut mpsc::Receiver<StreamMsg>) -> bool {
        *self.shared.conn.lock().expect("conn") = Some(conn.clone());
        self.shell_seen = false;
        self.shell_synced = false;
        let mut payload = json!({"requestCompletionMarker": true});
        if let Some(c) = self.shell_cursor {
            payload["afterSequence"] = json!(c);
        }
        match conn.subscribe(wire::SUBSCRIBE_SHELL, payload) {
            Ok(id) => {
                self.shell_stream = Some(id);
                self.shell_deadline = Some(Instant::now() + self.timing.sync_timeout);
            }
            Err(_) => conn.close("connection to T3 lost"),
        }
        let shared = self.shared.clone();
        // One persistent interval, so a steady flow of chunks cannot
        // postpone the tick.
        let mut tick = interval(self.timing.tick);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                msg = events.recv() => match msg {
                    Some(StreamMsg::Chunk { stream, values }) => self.on_chunk(&conn, &stream, values),
                    Some(StreamMsg::End { stream, failure }) => self.on_end(&conn, &stream, failure),
                    None => break,
                },
                _ = shared.mailbox.notified() => self.drain_mailbox(),
                _ = tick.tick() => {
                    self.drain_mailbox();
                    self.on_tick(&conn);
                }
            }
        }
        *self.shared.conn.lock().expect("conn") = None;
        conn.close("connection to T3 lost");
        self.shell_synced
    }

    /// Stream ids belong to the dead connection; cursors and detail stay so
    /// resumed streams continue where they were.
    fn connection_lost(&mut self) {
        self.status(
            SourceState::Degraded,
            Some("connection to T3 lost".to_owned()),
        );
        self.shell_stream = None;
        self.shell_deadline = None;
        self.shell_synced = false;
        // An adapter-reported problem lasts until the next reconnect.
        self.adapter_degraded = None;
        self.streams.clear();
        for t in self.threads.values_mut() {
            if t.class == Some(Class::Discovery) {
                t.detail = None;
                t.cursor = None;
            }
            t.stream = None;
            t.class = None;
            t.sync_deadline = None;
            t.unneeded_since = None;
        }
        let ids: Vec<String> = self.threads.keys().cloned().collect();
        for id in ids {
            self.upsert_unknown(&id);
        }
        self.write_stats();
    }

    fn drain_mailbox(&mut self) {
        let (answered, degraded) = self.shared.mailbox.take();
        for (request_id, by) in self.shared.mailbox.take_ended() {
            self.pending_ends.enqueue(request_id, by);
        }
        for (thread_id, request_id) in answered {
            let Some(t) = self.threads.get_mut(&thread_id) else {
                continue;
            };
            if t.open.as_deref() == Some(request_id.as_str()) {
                t.open = None;
            }
            t.hidden.insert(request_id.clone());
            t.ended.remove(&request_id);
            self.shared.targets.remove(&request_id);
            self.upsert(&thread_id);
            self.reconcile_requests(&thread_id);
        }
        if let Some(msg) = degraded {
            self.adapter_degraded = Some((msg, Instant::now()));
            self.refresh_status();
        }
    }

    fn on_tick(&mut self, conn: &Conn) {
        let now = Instant::now();
        let ready_ends = self
            .pending_ends
            .ready_to_end(|rid| self.core.request(rid).map(|request| request.status));
        for (rid, by) in ready_ends {
            self.core.observe(Observation::RequestEndedIfOpen {
                request_id: rid,
                by,
            });
        }
        if self.shell_deadline.is_some_and(|d| now >= d) {
            conn.close("T3 shell did not synchronize");
            return;
        }
        self.expire_degradations(now);
        self.reveal_unsnoozed(conn);
        self.hide_settled_requests(conn);
        let expired: Vec<String> = self
            .threads
            .iter()
            .filter(|(_, t)| t.sync_deadline.is_some_and(|d| now >= d))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.stall(conn, &id, now);
        }
        self.policy(conn);
        self.write_stats();
    }

    fn on_chunk(&mut self, conn: &Conn, stream: &str, values: Vec<Value>) {
        if self.shell_stream.as_deref() == Some(stream) {
            for v in &values {
                self.on_shell_value(conn, v);
            }
            self.shell_seen = true;
            self.policy(conn);
            return;
        }
        let Some(thread_id) = self.streams.get(stream).cloned() else {
            return; // a stream released since
        };
        for v in &values {
            let current = self
                .threads
                .get(&thread_id)
                .is_some_and(|t| t.stream.as_deref() == Some(stream));
            if !current {
                break;
            }
            self.on_thread_value(conn, &thread_id, v);
        }
    }

    fn on_end(&mut self, conn: &Conn, stream: &str, failure: Option<Failure>) {
        if let Some(f) = &failure {
            tracing::debug!(failure = %crate::text::redact_log(&wire::failure_json(f).to_string()), "T3 stream ended");
        }
        if self.shell_stream.as_deref() == Some(stream) {
            // The shell must never end.
            conn.close("T3 ended the shell subscription");
            return;
        }
        let Some(thread_id) = self.streams.remove(stream) else {
            return;
        };
        // `on_end` clears only the stream target and waits before retrying;
        // policy eviction is what removes retained detail and its cursor.
        let stall = Instant::now() + self.timing.relearn_after;
        if let Some(t) = self.threads.get_mut(&thread_id) {
            t.stream = None;
            t.class = None;
            t.sync_deadline = None;
            t.unneeded_since = None;
            t.stalled_until = Some(stall);
        }
    }

    // ---- shell ----

    fn on_shell_value(&mut self, conn: &Conn, v: &Value) {
        if wire::is_project_only(v) {
            // Projects only: no cursor change, no thread removal.
            match ShellItem::decode(v) {
                Ok(ShellItem::Snapshot { snapshot }) => self.merge_projects(&snapshot),
                _ => self.decode_error("shell item", v, None),
            }
            return;
        }
        if let Some(n) = wire::item_sequence(v) {
            if self.shell_cursor.is_some_and(|c| n <= c) {
                return;
            }
            // Before decoding, so an unreadable item is never replayed.
            self.shell_cursor = Some(n);
        }
        match ShellItem::decode(v) {
            Ok(ShellItem::Snapshot { snapshot }) => self.apply_snapshot(conn, snapshot),
            Ok(ShellItem::Synchronized {}) => {
                self.shell_deadline = None;
                self.shell_synced = true;
                self.refresh_status();
                // Restores agents that went unknown on a connection loss.
                let ids: Vec<String> = self.threads.keys().cloned().collect();
                for id in ids {
                    self.upsert(&id);
                }
            }
            Ok(ShellItem::ThreadUpdated {
                location, thread, ..
            }) => {
                let set = |k: &str| thread.get(k).is_some_and(|x| !x.is_null());
                if location != "active" || set("archivedAt") || set("deletedAt") {
                    if let Some(id) = thread["id"].as_str() {
                        self.gone(conn, id);
                    }
                    return;
                }
                match ShellThread::decode(&thread) {
                    Ok(t) => self.apply_shell(conn, t),
                    Err(_) => self.decode_error("shell item", v, thread["id"].as_str()),
                }
            }
            Ok(ShellItem::ThreadRemoved { thread_id, .. }) => self.gone(conn, &thread_id),
            Ok(ShellItem::ProjectUpdated { project, .. }) => {
                let id = project.id.clone();
                self.projects.insert(id.clone(), project);
                self.upsert_project_threads(&id);
            }
            Ok(ShellItem::Other) => {
                // `project.removed` has no recorded payload; `projectId` is
                // by analogy with `thread.removed`'s `threadId`.
                if v["kind"] == "project.removed"
                    && let Some(id) = v["projectId"].as_str()
                    && self.projects.remove(id).is_some()
                {
                    self.upsert_project_threads(id);
                } else {
                    tracing::debug!(kind = %redact_log(&wire::safe_tag(&v["kind"]).to_string()), "T3 shell item ignored");
                }
            }
            Err(_) => self.decode_error("shell item", v, v["thread"]["id"].as_str()),
        }
    }

    fn decode_projects(&mut self, snapshot: &ShellSnapshot) -> Vec<Project> {
        let mut out = vec![];
        for p in &snapshot.projects {
            match Project::decode(p) {
                Ok(p) => out.push(p),
                Err(_) => self.decode_error("project", p, None),
            }
        }
        out
    }

    fn merge_projects(&mut self, snapshot: &ShellSnapshot) {
        for p in self.decode_projects(snapshot) {
            if self.projects.get(&p.id) != Some(&p) {
                let id = p.id.clone();
                self.projects.insert(id.clone(), p);
                self.upsert_project_threads(&id);
            }
        }
    }

    fn apply_snapshot(&mut self, conn: &Conn, snapshot: ShellSnapshot) {
        self.projects = self
            .decode_projects(&snapshot)
            .into_iter()
            .map(|p| (p.id.clone(), p))
            .collect();
        let mut seen = HashSet::new();
        for entry in &snapshot.threads {
            let set = |k: &str| entry.get(k).is_some_and(|x| !x.is_null());
            if set("archivedAt") || set("deletedAt") {
                continue;
            }
            match ShellThread::decode(entry) {
                Ok(t) => {
                    seen.insert(t.id.clone());
                    self.apply_shell(conn, t);
                }
                Err(_) => {
                    let id = entry["id"].as_str();
                    if let Some(id) = id {
                        seen.insert(id.to_owned());
                    }
                    self.decode_error("shell thread", entry, id);
                }
            }
        }
        let vanished: Vec<String> = self
            .threads
            .keys()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect();
        for id in vanished {
            self.gone(conn, &id);
        }
        self.hidden_shells.retain(|id, _| seen.contains(id));
        let stale: Vec<String> = self
            .hidden_owners
            .keys()
            .filter(|id| !seen.contains(*id))
            .cloned()
            .collect();
        for id in stale {
            self.gone(conn, &id);
        }
        self.shell_cursor = Some(snapshot.snapshot_sequence);
    }

    /// Counts and logs an unreadable value and degrades the source. With a
    /// thread id, that thread's agent becomes `unknown` (D12).
    fn decode_error(&mut self, what: &str, v: &Value, thread_id: Option<&str>) {
        self.count_unreadable(what, v, 1);
        if let Some(id) = thread_id {
            let t = self
                .threads
                .entry(id.to_owned())
                .or_insert_with(ThreadState::new);
            // The last good entry stays: owner, stream class and detail are
            // unchanged until a readable entry replaces it (R10).
            t.unreadable = true;
            self.upsert(id);
        }
    }

    fn count_unreadable(&mut self, what: &str, v: &Value, n: usize) {
        self.decode_errors += n;
        self.last_decode_error = Some(Instant::now());
        log_unreadable(what, v);
        self.refresh_status();
    }

    /// Shows a visible thread and treats a hidden one like an archived
    /// thread: no agent, no discovery stream, no request stream. A thread
    /// with an open request stays until that request has ended
    /// (`hide_settled_requests` hides it then), so the request resolves with
    /// its real outcome instead of being lost with the agent.
    fn apply_shell(&mut self, conn: &Conn, shell: ShellThread) {
        // An answer in flight keeps the agent too: it is pending in the core
        // until T3 acks the command.
        let keep = self
            .threads
            .get(&shell.id)
            .is_some_and(|t| t.open.is_some())
            || self
                .core
                .agent(&map::agent_id(&shell.id))
                .is_some_and(|a| a.pending.is_some());
        if keep || shell.visible(Utc::now()) {
            let id = shell.id.clone();
            self.hidden_shells.remove(&id);
            self.put_shell(shell);
            if let Some(owner) = self.hidden_owners.remove(&id) {
                if let Some(t) = self.threads.get_mut(&id)
                    && t.owner.is_none()
                {
                    // Known, so the reveal does not pull a snapshot again.
                    t.discovery = Discovery::Known {
                        provider_thread: owner.0.clone(),
                    };
                    t.owner = Some(owner);
                }
                self.publish_owner(&id);
            }
            return;
        }
        let id = shell.id.clone();
        // The claim outlives the agent, unless the provider thread changed.
        let owner = self
            .threads
            .get(&id)
            .and_then(|t| t.owner.clone())
            .or_else(|| self.hidden_owners.get(&id).cloned())
            .filter(|(pt, _)| shell.active_provider_thread_id.as_deref() == Some(pt.as_str()));
        match owner {
            Some(owner) => {
                self.retire(conn, &id, true);
                self.hidden_owners.insert(id.clone(), owner);
            }
            None => {
                self.hidden_owners.remove(&id);
                self.retire(conn, &id, false);
            }
        }
        self.hidden_shells.insert(id, shell);
    }

    /// Hides threads that were kept for an open request once the request
    /// has ended in the core.
    fn hide_settled_requests(&mut self, conn: &Conn) {
        let now = Utc::now();
        let ready: Vec<ShellThread> = self
            .threads
            .iter()
            .filter(|(id, t)| {
                t.open.is_none()
                    && t.shell.as_ref().is_some_and(|s| !s.visible(now))
                    && self
                        .core
                        .agent(&map::agent_id(id))
                        .is_none_or(|a| a.pending.is_none())
            })
            .filter_map(|(_, t)| t.shell.clone())
            .collect();
        for shell in ready {
            self.apply_shell(conn, shell);
        }
    }

    /// A snooze that ended shows the thread again.
    fn reveal_unsnoozed(&mut self, conn: &Conn) {
        let now = Utc::now();
        let ready: Vec<String> = self
            .hidden_shells
            .iter()
            .filter(|(_, s)| s.visible(now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ready {
            if let Some(shell) = self.hidden_shells.remove(&id) {
                self.apply_shell(conn, shell);
            }
        }
    }

    fn put_shell(&mut self, shell: ShellThread) {
        let id = shell.id.clone();
        let t = self
            .threads
            .entry(id.clone())
            .or_insert_with(ThreadState::new);
        if t.shell.as_ref() != Some(&shell) {
            // A changed entry retries a null idle thread at once. A working
            // thread's entry changes all the time, so an overflow thread
            // waits for `relearn_after` unless its provider thread changes.
            let working = map::shell_state(&shell, false).0 == AgentState::Working;
            let reset = match &t.discovery {
                Discovery::NotLoaded {
                    provider_thread, ..
                } => !working || shell.active_provider_thread_id != *provider_thread,
                Discovery::Known { provider_thread } => {
                    shell.active_provider_thread_id.as_deref() != Some(provider_thread.as_str())
                }
                Discovery::Unknown => false,
            };
            if reset {
                t.discovery = Discovery::Unknown;
            }
        }
        t.shell = Some(shell);
        t.unreadable = false;
        self.reconcile_requests(&id);
        self.upsert(&id);
        self.publish_owner(&id);
    }

    fn upsert_project_threads(&mut self, project_id: &str) {
        let ids: Vec<String> = self
            .threads
            .iter()
            .filter(|(_, t)| {
                t.shell
                    .as_ref()
                    .is_some_and(|s| s.project_id.as_deref() == Some(project_id))
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.upsert(&id);
        }
    }

    fn upsert(&mut self, thread_id: &str) {
        let Some(t) = self.threads.get_mut(thread_id) else {
            return;
        };
        let obs = match &t.shell {
            Some(s) if !t.unreadable => {
                let project = s.project_id.as_ref().and_then(|p| self.projects.get(p));
                map::agent_obs(s, project, &t.hidden)
            }
            _ => unknown_obs(thread_id, t.last_obs.as_ref()),
        };
        self.observe(thread_id, obs);
    }

    fn upsert_unknown(&mut self, thread_id: &str) {
        let Some(t) = self.threads.get(thread_id) else {
            return;
        };
        let obs = unknown_obs(thread_id, t.last_obs.as_ref());
        self.observe(thread_id, obs);
    }

    fn observe(&mut self, thread_id: &str, obs: AgentObs) {
        if let Some(t) = self.threads.get_mut(thread_id) {
            t.last_obs = Some(obs.clone());
        }
        // Before the core sees the agent, so the adapter can resolve it.
        self.shared
            .threads
            .write()
            .expect("threads")
            .insert(obs.id.clone(), thread_id.to_owned());
        self.core.observe(Observation::AgentUpsert(obs));
    }

    fn gone(&mut self, conn: &Conn, thread_id: &str) {
        self.hidden_shells.remove(thread_id);
        self.hidden_owners.remove(thread_id);
        self.retire(conn, thread_id, false);
    }

    /// Removes the thread's agent and streams. `keep_claim` leaves its
    /// session claim in place (a hidden thread, see `hidden_owners`).
    fn retire(&mut self, conn: &Conn, thread_id: &str, keep_claim: bool) {
        let agent_id = map::agent_id(thread_id);
        let Some(t) = self.threads.remove(thread_id) else {
            if !keep_claim {
                self.shared.sessions.remove_agent(&agent_id);
            }
            return;
        };
        self.shared.targets.retain_thread(thread_id, None);
        if let Some(s) = t.stream {
            conn.interrupt(&s);
            self.streams.remove(&s);
        }
        self.shared
            .threads
            .write()
            .expect("threads")
            .remove(&agent_id);
        if !keep_claim {
            self.shared.sessions.remove_agent(&agent_id);
        }
        self.core.observe(Observation::AgentGone { agent_id });
    }

    /// Publishes ownership only when the shell and the thread detail agree
    /// on the active provider thread (Sol rounds 2, 4 and 5). Runs after
    /// every shell update of the thread and every value of its stream.
    fn publish_owner(&mut self, thread_id: &str) {
        let Some(t) = self.threads.get_mut(thread_id) else {
            return;
        };
        let agent = map::agent_id(thread_id);
        let shell_pt = t
            .shell
            .as_ref()
            .and_then(|s| s.active_provider_thread_id.clone());

        // 1. Invalidation, before any synchronization gate. A validated
        //    owner is removed only by identity evidence: the shell names
        //    another active provider thread, or the detail names another
        //    provider thread (or none), or a null or different native id.
        //    A stall, a timeout or a missing detail is not evidence.
        if let Some((owner_pt, owner_session)) = t.owner.clone() {
            let detail_says = t.detail.as_ref().map(|d| {
                (
                    d.active_provider_thread.clone(),
                    d.native_session_id().map(str::to_owned),
                )
            });
            let invalid = shell_pt.as_deref() != Some(owner_pt.as_str())
                || detail_says.is_some_and(|(pt, native)| {
                    pt.as_deref() != Some(owner_pt.as_str())
                        || native.as_deref() != Some(owner_session.as_str())
                });
            if invalid {
                self.shared.sessions.set(&agent, None);
                t.owner = None;
                t.discovery = Discovery::Unknown;
            }
        }

        // 2. Publish whenever shell and detail agree and the native id is
        //    set, whatever the stream's sync state: T3's snapshot is its
        //    current projection.
        let Some(detail) = t.detail.as_ref() else {
            return;
        };
        let detail_pt = detail.active_provider_thread.clone();
        match (shell_pt.clone(), detail_pt) {
            (Some(s), Some(d)) if s == d => match detail.native_session_id().map(str::to_owned) {
                Some(n) => {
                    // After step 1, an existing owner here is the same one.
                    if t.owner.is_none() {
                        self.shared.sessions.set(&agent, Some(&n));
                        t.owner = Some((d.clone(), n));
                    }
                    t.discovery = Discovery::Known { provider_thread: d };
                }
                None => {
                    t.discovery = Discovery::NotLoaded {
                        provider_thread: Some(d),
                        at: Instant::now(),
                    }
                }
            },
            // Disagreement, or a null on either side: nothing to publish.
            // The shell's side is recorded, so later shell entries compare
            // like with like.
            _ => {
                if t.owner.is_none() {
                    t.discovery = Discovery::NotLoaded {
                        provider_thread: shell_pt,
                        at: Instant::now(),
                    };
                }
            }
        }
    }

    // ---- thread streams ----

    fn on_thread_value(&mut self, conn: &Conn, thread_id: &str, v: &Value) {
        let item = ThreadItem::decode(v);
        let Some(t) = self.threads.get_mut(thread_id) else {
            return;
        };
        let mut skipped = 0;
        let mut finish_discovery = false;
        let mut synchronized = false;
        match item {
            // A snapshot acd cannot read leaves nothing to keep the stream
            // for: stall and retry with a fresh one after `relearn_after`.
            Ok(ThreadItem::Snapshot { projection, .. })
                if !ThreadDetail::valid_projection(&projection) =>
            {
                return self.unreadable_snapshot(conn, thread_id, v);
            }
            Err(_) if v["kind"] == "snapshot" => {
                return self.unreadable_snapshot(conn, thread_id, v);
            }
            Ok(ThreadItem::Snapshot {
                snapshot_sequence,
                projection,
            }) => {
                let (detail, n) = ThreadDetail::from_projection(&projection);
                t.detail = Some(detail);
                t.cursor = Some(snapshot_sequence);
                skipped = n;
            }
            Ok(ThreadItem::Event { sequence, event }) => {
                if t.cursor.is_some_and(|c| sequence <= c) {
                    return;
                }
                t.cursor = Some(sequence);
                if let Some(d) = t.detail.as_mut()
                    && d.apply(&event).is_err()
                {
                    skipped = 1;
                }
            }
            Ok(ThreadItem::Synchronized {}) => {
                t.sync_deadline = None;
                synchronized = true;
                finish_discovery = t.class == Some(Class::Discovery);
            }
            Ok(ThreadItem::Other) => {
                tracing::debug!(kind = %redact_log(&wire::safe_tag(&v["kind"]).to_string()), "T3 thread item ignored");
            }
            Err(_) => skipped = 1,
        }
        if skipped > 0 {
            self.count_unreadable("thread item", v, skipped);
        }
        self.publish_owner(thread_id);
        self.reconcile_requests(thread_id);
        if synchronized {
            // A fresh snapshot that does not list the request means T3 no
            // longer has it: end it, or the thread would be kept forever.
            let missing = self.threads.get(thread_id).and_then(|t| {
                let open = t.open.clone()?;
                t.detail
                    .as_ref()
                    .filter(|d| d.request_status(&open).is_none())
                    .map(|_| open)
            });
            if let Some(open) = missing {
                self.end_open(thread_id, open, ResolvedBy::Lost);
            }
        }
        if finish_discovery {
            self.finish_discovery(conn, thread_id);
        }
    }

    /// Ends the request acd surfaced for the thread and forgets it.
    fn end_open(&mut self, thread_id: &str, open: String, by: ResolvedBy) {
        let agent_id = map::agent_id(thread_id);
        if let Some(t) = self.threads.get_mut(thread_id) {
            t.open = None;
            t.ended.insert(open.clone());
        }
        self.shared.targets.remove(&open);
        if let Some(rid) = self.core.request_id_for(&agent_id, &CorrelationKey(open)) {
            // Queue independently of the current status: a claim can win
            // between the detail observation and RequestEndedIfOpen.
            self.pending_ends.enqueue(rid.clone(), by);
            self.core.observe(Observation::RequestEndedIfOpen {
                request_id: rid,
                by,
            });
        }
    }

    fn reconcile_requests(&mut self, thread_id: &str) {
        let Some(t) = self.threads.get(thread_id) else {
            return;
        };
        let agent_id = map::agent_id(thread_id);
        let shell_pending = t
            .shell
            .as_ref()
            .and_then(|s| s.pending_runtime_request.as_ref())
            .map(|p| p.id.clone());
        let detail = t.detail.clone();
        let open = t.open.clone();
        let hidden: HashSet<String> = if let Some(t) = self.threads.get_mut(thread_id) {
            t.hidden
                .retain(|h| shell_pending.as_deref() == Some(h.as_str()));
            t.ended
                .retain(|h| shell_pending.as_deref() == Some(h.as_str()));
            t.hidden.clone()
        } else {
            return;
        };
        let Some(detail) = detail else { return };

        if let Some(open) = open
            && let Some(status) = detail.request_status(&open).filter(|s| *s != "pending")
        {
            let by = if status == "resolved" {
                ResolvedBy::Terminal
            } else {
                ResolvedBy::Lost
            };
            self.end_open(thread_id, open, by);
        }

        let ended = self
            .threads
            .get(thread_id)
            .map(|t| t.ended.clone())
            .unwrap_or_default();
        if let Some(p) = shell_pending.filter(|p| !hidden.contains(p) && !ended.contains(p))
            && let Some(view) = detail.pending_view(&p)
        {
            let (obs, target) = map::request_obs(&agent_id, thread_id, &view);
            self.shared.targets.retain_thread(thread_id, Some(&p));
            self.shared.targets.insert(target);
            self.core.observe(Observation::RequestSeen(obs));
            if let Some(t) = self.threads.get_mut(thread_id) {
                t.open = Some(p);
            }
        }
        if let Some(t) = self.threads.get_mut(thread_id) {
            let keep: Vec<String> = t
                .shell
                .as_ref()
                .and_then(|s| s.pending_runtime_request.as_ref())
                .map(|p| p.id.clone())
                .into_iter()
                .chain(t.open.clone())
                .collect();
            if let Some(detail) = t.detail.as_mut() {
                detail.prune(&keep.iter().map(String::as_str).collect::<Vec<_>>());
            }
        }
    }

    /// A discovery stream is released right after its `synchronized`.
    fn finish_discovery(&mut self, conn: &Conn, thread_id: &str) {
        let Some(t) = self.threads.get_mut(thread_id) else {
            return;
        };
        if t.discovery == Discovery::Unknown {
            // No snapshot reached the owner rule: looked, found nothing.
            t.discovery = Discovery::NotLoaded {
                provider_thread: t
                    .shell
                    .as_ref()
                    .and_then(|s| s.active_provider_thread_id.clone()),
                at: Instant::now(),
            };
        }
        self.release(conn, thread_id);
        // The slot is free: pick the next candidate now, not on the tick.
        self.policy(conn);
    }

    /// Interrupts the thread's stream and evicts its detail and cursor: a
    /// later subscription gets a fresh snapshot.
    fn release(&mut self, conn: &Conn, thread_id: &str) {
        let Some(t) = self.threads.get_mut(thread_id) else {
            return;
        };
        if let Some(s) = t.stream.take() {
            conn.interrupt(&s);
            self.streams.remove(&s);
        }
        t.class = None;
        t.unneeded_since = None;
        t.sync_deadline = None;
        t.detail = None;
        t.cursor = None;
    }

    fn unreadable_snapshot(&mut self, conn: &Conn, thread_id: &str, v: &Value) {
        self.count_unreadable("thread snapshot", v, 1);
        self.stall(conn, thread_id, Instant::now());
    }

    /// The first `synchronized` did not arrive in time, or the snapshot
    /// could not be read. The owner stays (neither is identity evidence);
    /// discovery starts over after the stall, which is how acd gets the
    /// thread's live updates again.
    fn stall(&mut self, conn: &Conn, thread_id: &str, now: Instant) {
        self.release(conn, thread_id);
        if let Some(t) = self.threads.get_mut(thread_id) {
            t.stalled_until = Some(now + self.timing.relearn_after);
            t.discovery = Discovery::Unknown;
        }
    }

    fn open(&mut self, conn: &Conn, thread_id: &str, class: Class, now: Instant) -> bool {
        let Some(t) = self.threads.get_mut(thread_id) else {
            return false;
        };
        let mut payload = json!({"threadId": thread_id, "requestCompletionMarker": true});
        if let Some(c) = t.cursor {
            payload["afterSequence"] = json!(c);
        }
        match conn.subscribe(wire::SUBSCRIBE_THREAD, payload) {
            Ok(id) => {
                self.streams.insert(id.clone(), thread_id.to_owned());
                t.stream = Some(id);
                t.class = Some(class);
                t.unneeded_since = None;
                t.sync_deadline = Some(now + self.timing.sync_timeout);
                if class == Class::Discovery {
                    t.checked_at = Some(now);
                }
                true
            }
            Err(_) => false,
        }
    }

    fn eligible(&self, t: &ThreadState, now: Instant) -> bool {
        !t.stalled(now)
            && t.stream.is_none()
            && match &t.discovery {
                Discovery::Unknown => true,
                Discovery::NotLoaded { at, .. } => {
                    now.duration_since(*at) >= self.timing.relearn_after
                }
                Discovery::Known { .. } => false,
            }
    }

    /// The stream policy pass: classify open streams, release unneeded ones,
    /// then open request, working and discovery streams, at most
    /// `OPEN_PER_PASS` in all.
    fn policy(&mut self, conn: &Conn) {
        if !self.shell_seen || conn.is_closed() {
            return;
        }
        let now = Instant::now();
        let mut ids: Vec<String> = self.threads.keys().cloned().collect();
        ids.sort();
        let requests: HashSet<String> = ids
            .iter()
            .filter(|id| {
                let t = &self.threads[*id];
                !t.stalled(now) && t.wants_request_stream()
            })
            .cloned()
            .collect();
        let working: HashSet<String> = ids
            .iter()
            .filter(|id| {
                let t = &self.threads[*id];
                !t.stalled(now) && !requests.contains(*id) && t.is_working()
            })
            .cloned()
            .collect();

        // Open streams. Working streams keep their slot until released,
        // so the cap holds while one waits out `release_after`.
        let mut slots = self
            .threads
            .values()
            .filter(|t| t.class == Some(Class::Working))
            .count();
        let mut to_release = vec![];
        for id in &ids {
            let max_live = self.timing.max_live;
            let t = self.threads.get_mut(id).expect("thread");
            let Some(class) = t.class else { continue };
            let needed = if requests.contains(id) {
                if class == Class::Working {
                    slots -= 1;
                }
                t.class = Some(Class::Request);
                true
            } else {
                match class {
                    Class::Working => working.contains(id),
                    Class::Request | Class::Discovery => {
                        if working.contains(id) && slots < max_live {
                            t.class = Some(Class::Working);
                            slots += 1;
                            true
                        } else {
                            // A discovery stream ends at its `synchronized`.
                            class == Class::Discovery
                        }
                    }
                }
            };
            if needed {
                t.unneeded_since = None;
            } else {
                let since = *t.unneeded_since.get_or_insert(now);
                if now.duration_since(since) >= self.timing.release_after {
                    to_release.push(id.clone());
                }
            }
        }
        for id in to_release {
            if self.threads[&id].class == Some(Class::Working) {
                slots -= 1;
            }
            self.release(conn, &id);
        }

        let mut budget = OPEN_PER_PASS;
        let mut deferred: HashSet<String> = HashSet::new();
        for id in ids.iter().filter(|id| requests.contains(*id)) {
            if self.threads[id].stream.is_some() {
                continue;
            }
            if budget == 0 {
                deferred.insert(id.clone());
                continue;
            }
            if self.open(conn, id, Class::Request, now) {
                budget -= 1;
            }
        }

        // Most recently updated first.
        let mut waiting: Vec<(Option<DateTime<Utc>>, String)> = ids
            .iter()
            .filter(|id| working.contains(*id) && self.threads[*id].stream.is_none())
            .map(|id| (self.threads[id].updated(), id.clone()))
            .collect();
        waiting.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        for (_, id) in &waiting {
            if slots >= self.timing.max_live {
                break;
            }
            if budget == 0 {
                deferred.insert(id.clone());
                continue;
            }
            if self.open(conn, id, Class::Working, now) {
                budget -= 1;
                slots += 1;
            }
        }

        let discovering = self
            .threads
            .values()
            .any(|t| t.class == Some(Class::Discovery));
        if !discovering && budget > 0 {
            self.discover(conn, &ids, &requests, &working, now);
        }

        // Only threads with an open stream hold detail, apart from those
        // this pass had no budget left to reopen.
        for (id, t) in self.threads.iter_mut() {
            if t.stream.is_none()
                && (t.detail.is_some() || t.cursor.is_some())
                && !deferred.contains(id)
            {
                t.detail = None;
                t.cursor = None;
            }
        }
        self.note_peaks();
    }

    /// Opens at most one discovery stream: overflow working threads first,
    /// but the recent group gets every `OVERFLOW_BURST + 1`th pick when it
    /// has a candidate.
    fn discover(
        &mut self,
        conn: &Conn,
        ids: &[String],
        requests: &HashSet<String>,
        working: &HashSet<String>,
        now: Instant,
    ) {
        let window = chrono::TimeDelta::from_std(self.timing.learn_window).ok();
        let recent_enabled = !self.timing.learn_window.is_zero();
        let utc_now = Utc::now();
        let mut overflow = vec![];
        let mut recent = vec![];
        for id in ids {
            let t = &self.threads[id];
            if !self.eligible(t, now) || requests.contains(id) {
                continue;
            }
            if working.contains(id) {
                overflow.push((t.checked_at, id.clone()));
                continue;
            }
            let kind = map::kind(
                t.shell
                    .as_ref()
                    .and_then(|s| s.provider_instance_id.as_deref()),
            );
            let is_recent = recent_enabled
                && matches!(kind, AgentKind::Claude | AgentKind::Codex)
                && t.updated()
                    .is_some_and(|u| window.is_none_or(|w| utc_now - u <= w));
            if is_recent {
                recent.push((t.checked_at, id.clone()));
            }
        }
        // Checked longest ago first; never checked before that.
        overflow.sort();
        recent.sort();
        let pick = if !overflow.is_empty()
            && (self.overflow_streak < OVERFLOW_BURST || recent.is_empty())
        {
            if recent.is_empty() {
                self.overflow_streak = 0;
            } else {
                self.overflow_streak += 1;
            }
            overflow.swap_remove(0).1
        } else if !recent.is_empty() {
            self.overflow_streak = 0;
            recent.swap_remove(0).1
        } else {
            return;
        };
        self.open(conn, &pick, Class::Discovery, now);
    }

    fn counts(&self) -> (usize, usize, usize) {
        let mut c = (0, 0, 0);
        for t in self.threads.values() {
            match t.class {
                Some(Class::Request) => c.0 += 1,
                Some(Class::Working) => c.1 += 1,
                Some(Class::Discovery) => c.2 += 1,
                None => {}
            }
        }
        c
    }

    fn note_peaks(&mut self) {
        let (_, working, discovery) = self.counts();
        self.peak_working = self.peak_working.max(working);
        self.peak_discovery = self.peak_discovery.max(discovery);
    }

    fn write_stats(&mut self) {
        self.note_peaks();
        let (request_streams, working_streams, discovery_streams) = self.counts();
        let mut st = SyncStats {
            request_streams,
            working_streams,
            discovery_streams,
            peak_working_streams: self.peak_working,
            peak_discovery_streams: self.peak_discovery,
            ..SyncStats::default()
        };
        for d in self.threads.values().filter_map(|t| t.detail.as_ref()) {
            let (requests, items) = d.retained();
            st.details += 1;
            st.requests += requests;
            st.items += items;
        }
        *self.shared.stats.lock().expect("stats") = st;
    }
}

/// The last observation with `state: unknown`, or a bare one for a thread
/// that never decoded.
fn unknown_obs(thread_id: &str, last: Option<&AgentObs>) -> AgentObs {
    let mut obs = last.cloned().unwrap_or_else(|| AgentObs {
        id: map::agent_id(thread_id),
        source: Source::T3,
        kind: AgentKind::Other,
        title: String::new(),
        cwd: None,
        project: None,
        state: AgentState::Unknown,
        failed: false,
        last_message: None,
        can_send: true,
    });
    obs.state = AgentState::Unknown;
    obs.failed = false;
    obs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    #[derive(Clone, Default)]
    struct Buffer(Arc<StdMutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn unreadable_log_omits_item_text() {
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let item = json!({"kind": "token=abc def", "message": "token=abc def"});
        let expected = wire::value_fingerprint(&item).to_string();

        log_unreadable("item", &item);

        let output = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        assert!(!output.contains("token=abc def"), "{output}");
        assert!(output.contains(&expected), "{output}");
    }
}
