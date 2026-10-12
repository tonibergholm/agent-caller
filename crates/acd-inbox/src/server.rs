//! The service (omarchy spec §5): connections, admission and close. Task 9
//! adds the socket side.
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_client::{actions::ActionOwner, inbox::Inbox, settings::SecretError};
use tokio::sync::watch;

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

    /// False when the id was seen among the remembered ones. Refused (and
    /// so true) once closed: nothing is replied then anyway.
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
    #[allow(dead_code)] // read by the unlock and refresh commands (Task 10)
    pub(crate) secrets: Arc<dyn SecretControl>,
    prefs_tx: watch::Sender<Prefs>,
    #[allow(dead_code)] // read by set_notifications callers (Task 10)
    prefs_path: Option<PathBuf>,
    pub(crate) registry: Mutex<Registry>,
    #[allow(dead_code)] // advanced by the settings broadcast (Task 9)
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

    #[allow(dead_code)] // wired by Task 10
    pub(crate) fn set_notifications(&self, on: bool) -> std::io::Result<()> {
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
}
