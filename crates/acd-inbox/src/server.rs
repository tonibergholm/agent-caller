//! The service (omarchy spec §5): connections, admission and close, and the
//! socket side: accept, read and write tasks and the broadcasters.
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_client::voice::ActionEvent;
use acd_client::{actions::ActionOwner, inbox::Inbox, settings::SecretError};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, watch};

use crate::{commands, startup};
use crate::outbox::{OUTCOME_QUEUE, Outbox, REPLY_QUEUE};
use crate::prefs::{self, Prefs};
use crate::protocol;

/// What the server needs from the keyring (§9). Both calls block: run them
/// on `spawn_blocking`.
pub trait SecretControl: Send + Sync {
    /// Unlocks the keyring; may wait for the user's prompt.
    fn unlock(&self) -> Result<(), SecretError>;
    /// Reads `keys` from the keyring again, replacing cached values and
    /// cached errors (plan D14).
    fn refresh(&self, keys: &[String]);
}

/// A keyring that is never locked: tests, and stores without one.
pub struct NoKeyring;

impl SecretControl for NoKeyring {
    fn unlock(&self) -> Result<(), SecretError> {
        Ok(())
    }
    fn refresh(&self, _: &[String]) {}
}

#[derive(Clone, Debug)]
pub struct ServerOptions {
    pub max_line: usize,
    pub write_timeout: Duration,
    pub outcome_queue: usize,
    pub reply_queue: usize,
    /// Command ids remembered per connection for duplicate detection.
    pub remembered_ids: usize,
    pub version: String,
}

impl Default for ServerOptions {
    fn default() -> Self {
        ServerOptions {
            max_line: protocol::MAX_LINE,
            write_timeout: Duration::from_secs(30),
            outcome_queue: OUTCOME_QUEUE,
            reply_queue: REPLY_QUEUE,
            remembered_ids: 1024,
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    Eof,
    ReadError,
    Oversize,
    Malformed,
    WriteTimeout,
    WriteError,
    ReplyOverflow,
    /// The server task or the read task was dropped or aborted.
    Shutdown,
}

impl CloseReason {
    pub fn tag(self) -> &'static str {
        match self {
            CloseReason::Eof => "eof",
            CloseReason::ReadError => "read_error",
            CloseReason::Oversize => "oversize",
            CloseReason::Malformed => "malformed",
            CloseReason::WriteTimeout => "write_timeout",
            CloseReason::WriteError => "write_error",
            CloseReason::ReplyOverflow => "reply_overflow",
            CloseReason::Shutdown => "shutdown",
        }
    }
}

/// The last `cap` command ids of a connection (plan D10).
pub(crate) struct RecentIds {
    order: VecDeque<String>,
    set: HashSet<String>,
    cap: usize,
}

impl RecentIds {
    fn new(cap: usize) -> Self {
        RecentIds { order: VecDeque::new(), set: HashSet::new(), cap }
    }
    /// False when `id` is among the remembered ones.
    pub(crate) fn insert(&mut self, id: &str) -> bool {
        if self.set.contains(id) {
            return false;
        }
        if self.order.len() >= self.cap
            && let Some(old) = self.order.pop_front()
        {
            self.set.remove(&old);
        }
        self.order.push_back(id.to_owned());
        self.set.insert(id.to_owned());
        true
    }
}

struct ConnState {
    closed: bool,
    ids: RecentIds,
}

/// One view connection.
pub struct Conn {
    pub id: u64,
    pub owner: ActionOwner,
    pub outbox: Outbox,
    /// The admission lock (plan D31).
    gate: Mutex<ConnState>,
    waiting: AtomicUsize,
    pub(crate) shut: watch::Sender<bool>,
}

impl Conn {
    fn lock(&self) -> std::sync::MutexGuard<'_, ConnState> {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let g = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        g
    }

    /// False when the id was seen among the remembered ones. Ids are still
    /// recorded after close; replies to a closed connection are dropped by
    /// the outbox.
    pub(crate) fn remember(&self, id: &str) -> bool {
        self.lock().ids.insert(id)
    }

    #[cfg(feature = "testing")]
    pub fn gate_waiters_for_test(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
}

/// Connections and the newest published lines, under one lock, so a new
/// connection is seeded and a broadcast reaches it without a gap or an
/// older line overwriting a newer one (plan D32).
pub(crate) struct Registry {
    pub(crate) conns: HashMap<u64, Arc<Conn>>,
    pub(crate) settings: (u64, String),
    pub(crate) view: String,
}

pub struct Service {
    pub inbox: Arc<Inbox>,
    pub(crate) secrets: Arc<dyn SecretControl>,
    prefs_tx: watch::Sender<Prefs>,
    prefs_path: Option<PathBuf>,
    /// One preference write at a time: read, save and publish are a unit,
    /// and two views may ask together.
    prefs_write: Mutex<()>,
    pub(crate) registry: Mutex<Registry>,
    pub(crate) settings_gen: AtomicU64,
    next_conn: AtomicU64,
    panels: Mutex<HashSet<u64>>,
    panel_tx: watch::Sender<bool>,
    #[cfg(feature = "testing")]
    hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    pub opts: ServerOptions,
}

impl Service {
    pub fn new(
        inbox: Arc<Inbox>,
        prefs: Prefs,
        prefs_path: Option<PathBuf>,
        secrets: Arc<dyn SecretControl>,
        opts: ServerOptions,
    ) -> Arc<Service> {
        let settings = protocol::settings_line(&prefs::snapshot(&inbox, prefs), opts.max_line);
        let view = protocol::view_or_overflow_line(&inbox.view().borrow().clone(), opts.max_line);
        Arc::new(Service {
            inbox,
            secrets,
            prefs_tx: watch::Sender::new(prefs),
            prefs_path,
            prefs_write: Mutex::new(()),
            registry: Mutex::new(Registry { conns: HashMap::new(), settings: (0, settings), view }),
            settings_gen: AtomicU64::new(0),
            next_conn: AtomicU64::new(1),
            panels: Mutex::new(HashSet::new()),
            panel_tx: watch::Sender::new(false),
            #[cfg(feature = "testing")]
            hook: Mutex::new(None),
            opts,
        })
    }

    /// True while any view reports its panel open (§8).
    pub fn panel_open(&self) -> watch::Receiver<bool> {
        self.panel_tx.subscribe()
    }

    pub fn prefs(&self) -> watch::Receiver<Prefs> {
        self.prefs_tx.subscribe()
    }

    pub fn connections(&self) -> usize {
        self.registry.lock().unwrap().conns.len()
    }

    /// A new connection, registered (and so subscribed to outcomes, §5.3)
    /// and seeded with `hello`, the newest `settings` and the newest `view`
    /// under the registry lock.
    pub(crate) fn register(&self) -> Arc<Conn> {
        let id = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let conn = Arc::new(Conn {
            id,
            owner: ActionOwner(id),
            outbox: Outbox::new(protocol::hello_line(&self.opts.version), self.opts.outcome_queue, self.opts.reply_queue),
            gate: Mutex::new(ConnState { closed: false, ids: RecentIds::new(self.opts.remembered_ids) }),
            waiting: AtomicUsize::new(0),
            shut: watch::Sender::new(false),
        });
        let mut reg = self.registry.lock().unwrap();
        conn.outbox.set_settings(reg.settings.1.clone());
        conn.outbox.set_view(reg.view.clone());
        reg.conns.insert(id, conn.clone());
        conn
    }

    #[cfg(feature = "testing")]
    pub fn connect_for_test(&self) -> Arc<Conn> {
        self.register()
    }

    #[cfg(feature = "testing")]
    pub fn set_admission_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self.hook.lock().unwrap() = Some(hook);
    }

    /// Runs `f` under the connection's admission lock if it is still open
    /// (plan D31). `f` must not call `close` or `admit`.
    pub fn admit<R>(&self, conn: &Conn, f: impl FnOnce() -> R) -> Option<R> {
        let state = conn.lock();
        if state.closed {
            return None;
        }
        #[cfg(feature = "testing")]
        if let Some(hook) = self.hook.lock().unwrap().clone() {
            hook();
        }
        let r = f();
        drop(state);
        Some(r)
    }

    /// Queues a reply; a view with too many unread replies is closed.
    pub(crate) fn reply(&self, conn: &Conn, line: String) {
        if !conn.outbox.reply(line) {
            self.close(conn, CloseReason::ReplyOverflow);
        }
    }

    /// Called only inside `admit`.
    pub(crate) fn set_panel(&self, conn: u64, open: bool) {
        let mut panels = self.panels.lock().unwrap();
        if open {
            panels.insert(conn);
        } else {
            panels.remove(&conn);
        }
        let any = !panels.is_empty();
        self.panel_tx.send_if_modified(|v| std::mem::replace(v, any) != any);
    }

    pub(crate) fn set_notifications(&self, on: bool) -> std::io::Result<()> {
        let _one_at_a_time = self.prefs_write.lock().unwrap();
        let mut p = *self.prefs_tx.borrow();
        p.notifications = on;
        if let Some(path) = &self.prefs_path {
            p.save(path)?;
        }
        self.prefs_tx.send_replace(p);
        Ok(())
    }

    /// Ends a connection, once (plan D29, D31). Under the admission lock it
    /// is marked closed, its unsent countdowns are cancelled (§6) and its
    /// panel flag is cleared; then it leaves the registry.
    pub fn close(&self, conn: &Conn, why: CloseReason) {
        let cancelled = {
            let mut state = conn.lock();
            if state.closed {
                return;
            }
            state.closed = true;
            let cancelled = self.inbox.cancel_owned(conn.owner);
            self.set_panel(conn.id, false);
            cancelled
        };
        self.registry.lock().unwrap().conns.remove(&conn.id);
        conn.outbox.close();
        conn.shut.send_replace(true);
        tracing::info!(conn = conn.id, reason = why.tag(), cancelled, "connection closed");
    }

    /// Serves `listener`. The view and outcome broadcasters stop when the
    /// returned task is aborted.
    pub fn spawn(self: &Arc<Self>, listener: UnixListener) -> tokio::task::JoinHandle<()> {
        let me = self.clone();
        let outcomes = self.inbox.action_outcomes();
        tokio::spawn(async move {
            // Dropped with the task (abort, runtime drop): closes every
            // connection, so command intake stops and countdowns cancel.
            let _guard = CloseAllOnDrop(me.clone());
            let mut tasks = tokio::task::JoinSet::new();
            tasks.spawn(me.clone().broadcast_views());
            tasks.spawn(me.clone().route_outcomes(outcomes));
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => me.accept(stream),
                    Err(e) => {
                        tracing::warn!(kind = ?e.kind(), "accept failed");
                        // Not a busy loop when descriptors run out (EMFILE).
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        })
    }

    /// Recomputes `settings` off the async workers and publishes it unless
    /// a newer publication got there first (plan D32).
    pub async fn publish_settings(self: &Arc<Self>) {
        let generation = self.settings_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let me = self.clone();
        let Ok(line) = tokio::task::spawn_blocking(move || {
            let p = *me.prefs_tx.borrow();
            protocol::settings_line(&prefs::snapshot(&me.inbox, p), me.opts.max_line)
        })
        .await
        else {
            return;
        };
        self.store_settings(generation, line);
    }

    pub(crate) fn store_settings(&self, generation: u64, line: String) {
        let mut reg = self.registry.lock().unwrap();
        if generation <= reg.settings.0 {
            return;
        }
        reg.settings = (generation, line.clone());
        for c in reg.conns.values() {
            c.outbox.set_settings(line.clone());
        }
    }

    /// `publish_settings` in the background.
    pub(crate) fn settings_changed(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move { me.publish_settings().await });
    }

    /// The only publisher of views (plan D32). Each view is encoded once
    /// and the line is shared by every connection.
    async fn broadcast_views(self: Arc<Self>) {
        let mut rx = self.inbox.view();
        loop {
            let view = rx.borrow_and_update().clone();
            // A view that cannot fit becomes `view_overflow`, which replaces
            // the previous view: the panel never shows a stale inbox (D9).
            let line = protocol::view_or_overflow_line(&view, self.opts.max_line);
            {
                let mut reg = self.registry.lock().unwrap();
                reg.view = line.clone();
                for c in reg.conns.values() {
                    c.outbox.set_view(line.clone());
                }
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Sends each outcome to every view; a lag sends `outcomes_lost` to
    /// every view (§5.1). `spawn` runs it on the inbox's stream.
    pub async fn route_outcomes(self: Arc<Self>, mut rx: broadcast::Receiver<ActionEvent>) {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let line = protocol::outcome_line(&event);
                    for c in self.registry.lock().unwrap().conns.values() {
                        c.outbox.outcome(line.clone());
                    }
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "outcome stream lagged");
                    for c in self.registry.lock().unwrap().conns.values() {
                        c.outbox.outcomes_lost();
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }

    fn accept(self: &Arc<Self>, stream: UnixStream) {
        if !startup::peer_ok(&stream) {
            tracing::warn!("connection refused: the peer is another user");
            return;
        }
        // Registered before anything is written: the connection receives
        // outcomes from here on, hello included.
        let conn = self.register();
        tracing::info!(conn = conn.id, "connection accepted");
        let (r, w) = stream.into_split();
        tokio::spawn(self.clone().write_loop(conn.clone(), w));
        tokio::spawn(self.clone().read_loop(conn, r));
    }

    async fn write_loop(self: Arc<Self>, conn: Arc<Conn>, mut w: OwnedWriteHalf) {
        while let Some(line) = conn.outbox.next().await {
            let mut bytes = line.into_bytes();
            bytes.push(b'\n');
            match tokio::time::timeout(self.opts.write_timeout, w.write_all(&bytes)).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return self.close(&conn, CloseReason::WriteError),
                Err(_) => return self.close(&conn, CloseReason::WriteTimeout),
            }
        }
        let _ = w.shutdown().await;
    }

    async fn read_loop(self: Arc<Self>, conn: Arc<Conn>, r: OwnedReadHalf) {
        let mut reader = BufReader::new(r);
        let mut line = Vec::new();
        let mut shut = conn.shut.subscribe();
        // Cancellation, a runtime drop or a panic close the connection too.
        let _guard = CloseOnDrop(self.clone(), conn.clone());
        loop {
            let read = tokio::select! {
                r = read_line(&mut reader, &mut line, self.opts.max_line) => r,
                _ = shut.wait_for(|closed| *closed) => return,
            };
            let why = match read {
                Ok(true) => match commands::handle(&self, &conn, &line) {
                    Ok(()) => continue,
                    Err(commands::Malformed) => CloseReason::Malformed,
                },
                Ok(false) => CloseReason::Eof,
                Err(LineError::TooLong) => CloseReason::Oversize,
                Err(LineError::Io) => CloseReason::ReadError,
            };
            if why != CloseReason::Eof {
                // Tag and length only: raw lines are never logged (§4.1).
                tracing::warn!(conn = conn.id, reason = why.tag(), len = line.len(), "line rejected");
            }
            return self.close(&conn, why);
        }
    }
}

/// Closes one connection when dropped; `close` is idempotent.
struct CloseOnDrop(Arc<Service>, Arc<Conn>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close(&self.1, CloseReason::Shutdown);
    }
}

/// Closes every registered connection when dropped.
struct CloseAllOnDrop(Arc<Service>);

impl Drop for CloseAllOnDrop {
    fn drop(&mut self) {
        let conns: Vec<Arc<Conn>> = self.0.registry.lock().unwrap().conns.values().cloned().collect();
        for c in conns {
            self.0.close(&c, CloseReason::Shutdown);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum LineError {
    TooLong,
    Io,
}

/// Reads one line, newline excluded, into `buf`. `Ok(false)` at end of
/// input; a final line without a newline is dropped. A line longer than
/// `max` is `TooLong`, and nothing past `max` is buffered.
pub async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R, buf: &mut Vec<u8>, max: usize) -> Result<bool, LineError> {
    buf.clear();
    loop {
        let available = r.fill_buf().await.map_err(|_| LineError::Io)?;
        if available.is_empty() {
            return Ok(false);
        }
        if let Some(i) = available.iter().position(|&b| b == b'\n') {
            if buf.len() + i > max {
                return Err(LineError::TooLong);
            }
            buf.extend_from_slice(&available[..i]);
            r.consume(i + 1);
            return Ok(true);
        }
        let n = available.len();
        if buf.len() + n > max {
            return Err(LineError::TooLong);
        }
        buf.extend_from_slice(available);
        r.consume(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acd_client::{
        inbox::InboxOptions,
        settings::{MemorySecrets, Settings},
    };

    #[tokio::test]
    async fn lines_are_split_capped_and_a_final_partial_line_is_dropped() {
        let mut r = BufReader::new(&b"one\ntwo\nthree"[..]);
        let mut buf = Vec::new();
        assert_eq!(read_line(&mut r, &mut buf, 16).await, Ok(true));
        assert_eq!(buf, b"one");
        assert_eq!(read_line(&mut r, &mut buf, 16).await, Ok(true));
        assert_eq!(buf, b"two");
        assert_eq!(read_line(&mut r, &mut buf, 16).await, Ok(false));
        let mut r = BufReader::new(&b"12345\n"[..]);
        assert_eq!(read_line(&mut r, &mut buf, 5).await, Ok(true), "exactly max fits");
        let mut r = BufReader::new(&b"123456\n"[..]);
        assert_eq!(read_line(&mut r, &mut buf, 5).await, Err(LineError::TooLong));
        let mut r = BufReader::with_capacity(2, &b"123456"[..]);
        assert_eq!(read_line(&mut r, &mut buf, 5).await, Err(LineError::TooLong), "without a newline too");
    }

    #[test]
    fn an_older_settings_publication_never_overwrites_a_newer_one() {
        let inbox = Inbox::new(
            Settings { machines: vec![], ..Settings::fresh() },
            Arc::new(MemorySecrets::default()),
            InboxOptions { timing: Default::default(), settings_path: None, client_name: "t".into(), client_version: "0".into() },
        );
        let svc = Service::new(inbox, Prefs::default(), None, Arc::new(NoKeyring), ServerOptions::default());
        let conn = svc.register();
        svc.store_settings(2, "new".into());
        svc.store_settings(1, "old".into());
        assert_eq!(svc.registry.lock().unwrap().settings, (2, "new".to_owned()));
        let lines: Vec<String> = std::iter::from_fn(|| conn.outbox.try_next_for_test()).collect();
        assert!(lines.contains(&"new".to_owned()) && !lines.contains(&"old".to_owned()));
    }
}
