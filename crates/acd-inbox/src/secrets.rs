//! The secret worker (omarchy spec §9, plan D13): one thread owns the
//! keyring connection; every job carries a deadline.
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use acd_client::settings::{SecretError, SecretStore};

use crate::server::SecretControl;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendError {
    /// The collection or item is locked; nothing was prompted.
    Locked,
    /// The user dismissed the unlock prompt.
    Dismissed,
    Timeout,
    /// No Secret Service on the session bus.
    Unavailable,
    Failed,
}

impl BackendError {
    /// A class for logs; never the D-Bus message.
    pub fn tag(self) -> &'static str {
        match self {
            BackendError::Locked => "locked",
            BackendError::Dismissed => "dismissed",
            BackendError::Timeout => "timeout",
            BackendError::Unavailable => "unavailable",
            BackendError::Failed => "failed",
        }
    }
}

/// A keyring. Implementations must not prompt except in `unlock`.
#[async_trait::async_trait]
pub trait SecretBackend: Send + 'static {
    async fn read(&mut self, key: &str) -> Result<Option<String>, BackendError>;
    async fn write(&mut self, key: &str, value: &str) -> Result<(), BackendError>;
    async fn delete(&mut self, key: &str) -> Result<(), BackendError>;
    async fn unlock(&mut self) -> Result<(), BackendError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkerError {
    Backend(BackendError),
    /// The deadline passed before the job started: it never ran.
    Expired,
    /// A write or delete that may or may not have run: it was cut off, or it
    /// is still queued behind a slow job such as an unlock, and the serial
    /// worker will still run it. Callers map it to the unknown status (D36).
    Ambiguous,
}

impl WorkerError {
    pub fn tag(self) -> &'static str {
        match self {
            WorkerError::Backend(e) => e.tag(),
            WorkerError::Expired => "expired",
            WorkerError::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct WorkerTimeouts {
    pub op: Duration,
    pub unlock: Duration,
}

impl WorkerTimeouts {
    /// §9: read, write, delete 5 s; unlock 120 s.
    pub const LIVE: WorkerTimeouts = WorkerTimeouts { op: Duration::from_secs(5), unlock: Duration::from_secs(120) };
}

enum Op {
    Read(String),
    Write(String, String),
    Delete(String),
    Unlock,
}

impl Op {
    fn mutates(&self) -> bool {
        matches!(self, Op::Write(..) | Op::Delete(_))
    }
}

struct Job {
    op: Op,
    deadline: Instant,
    reply: std_mpsc::SyncSender<Result<Option<String>, WorkerError>>,
}

pub struct SecretWorker {
    tx: tokio::sync::mpsc::UnboundedSender<Job>,
    timeouts: WorkerTimeouts,
    /// Jobs sent and not yet started.
    pending: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl SecretWorker {
    /// Starts the worker thread; it owns `backend` for the process's life.
    pub fn spawn<B: SecretBackend>(mut backend: B, timeouts: WorkerTimeouts) -> SecretWorker {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Job>();
        let pending = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let started = pending.clone();
        std::thread::Builder::new()
            .name("acd-inbox-secrets".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("the secret worker's runtime");
                rt.block_on(async move {
                    while let Some(job) = rx.recv().await {
                        started.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                        let now = Instant::now();
                        if now >= job.deadline {
                            let _ = job.reply.send(Err(WorkerError::Expired));
                            continue;
                        }
                        let mutates = job.op.mutates();
                        let run = async {
                            match job.op {
                                Op::Read(k) => backend.read(&k).await,
                                Op::Write(k, v) => backend.write(&k, &v).await.map(|()| None),
                                Op::Delete(k) => backend.delete(&k).await.map(|()| None),
                                Op::Unlock => backend.unlock().await.map(|()| None),
                            }
                        };
                        // A started write or delete runs to completion, so no
                        // change can land after a later read (plan D36). A read
                        // or unlock stops at the caller's deadline.
                        let result = if mutates {
                            run.await.map_err(WorkerError::Backend)
                        } else {
                            match tokio::time::timeout(job.deadline - now, run).await {
                                Ok(r) => r.map_err(WorkerError::Backend),
                                Err(_) => Err(WorkerError::Backend(BackendError::Timeout)),
                            }
                        };
                        let _ = job.reply.send(result);
                    }
                });
            })
            .expect("spawn the secret worker thread");
        SecretWorker { tx, timeouts, pending }
    }

    /// Test hook: jobs sent and not yet started.
    #[cfg(any(test, feature = "testing"))]
    pub fn pending_for_test(&self) -> usize {
        self.pending.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Blocks the calling thread for up to `limit` plus a second (6 s for a
    /// read, write or delete; 121 s for an unlock). Call it from
    /// `spawn_blocking` or a dedicated thread, never from an async task and
    /// never while holding a lock others need.
    fn call(&self, op: Op, limit: Duration) -> Result<Option<String>, WorkerError> {
        let mutates = op.mutates();
        let deadline = Instant::now() + limit;
        let (reply, rx) = std_mpsc::sync_channel(1);
        self.pending.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.tx.send(Job { op, deadline, reply }).is_err() {
            self.pending.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            return Err(WorkerError::Backend(BackendError::Unavailable));
        }
        match rx.recv_timeout(limit + Duration::from_secs(1)) {
            Ok(r) => r,
            // The worker thread is gone: a read never ran; a change may have.
            Err(std_mpsc::RecvTimeoutError::Disconnected) if mutates => Err(WorkerError::Ambiguous),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => Err(WorkerError::Backend(BackendError::Unavailable)),
            // A write or delete still running (or queued behind one): its
            // outcome is unknown to this caller (plan D36).
            Err(_) if mutates => Err(WorkerError::Ambiguous),
            Err(_) => Err(WorkerError::Backend(BackendError::Timeout)),
        }
    }

    pub fn read(&self, key: &str) -> Result<Option<String>, WorkerError> {
        self.call(Op::Read(key.to_owned()), self.timeouts.op)
    }

    pub fn write(&self, key: &str, value: &str) -> Result<(), WorkerError> {
        self.call(Op::Write(key.to_owned(), value.to_owned()), self.timeouts.op).map(|_| ())
    }

    pub fn delete(&self, key: &str) -> Result<(), WorkerError> {
        self.call(Op::Delete(key.to_owned()), self.timeouts.op).map(|_| ())
    }

    pub fn unlock(&self) -> Result<(), WorkerError> {
        self.call(Op::Unlock, self.timeouts.unlock).map(|_| ())
    }
}

/// What the store knows about a key (plan D14, D36).
#[derive(Clone)]
enum Cached {
    Value(Option<String>),
    Locked,
    Unavailable,
    TimedOut,
    /// A write or delete whose caller stopped waiting, or a re-pairing that
    /// could not commit. Cleared only by a successful read (`refresh`,
    /// which runs after the change settled) or a confirmed change.
    Unknown,
}

impl Cached {
    fn of(e: WorkerError) -> Cached {
        match e {
            WorkerError::Backend(BackendError::Locked | BackendError::Dismissed) => Cached::Locked,
            WorkerError::Backend(BackendError::Unavailable | BackendError::Failed) => Cached::Unavailable,
            WorkerError::Backend(BackendError::Timeout) | WorkerError::Expired => Cached::TimedOut,
            WorkerError::Ambiguous => Cached::Unknown,
        }
    }

    fn error(&self) -> Option<SecretError> {
        match self {
            Cached::Value(_) => None,
            Cached::Locked => Some(SecretError::locked()),
            Cached::TimedOut => Some(SecretError::timeout()),
            Cached::Unknown => Some(SecretError::unknown()),
            Cached::Unavailable => Some(SecretError("keyring unavailable".into())),
        }
    }
}

/// Entries and their generations (plan D37). A key's generation goes up at
/// the start of every write or delete; a read publishes its result only if
/// the generation it started under is still current.
#[derive(Default)]
struct Cache {
    entries: HashMap<String, Cached>,
    generations: HashMap<String, u64>,
}

/// The inbox's `SecretStore`: a per-key cache in front of the worker.
///
/// Anything that can reach the worker (`prefill`, `refresh`, `unlock`, a
/// `get` for an uncached key, `set`, `delete`) blocks the calling thread for
/// seconds. Call them from `spawn_blocking` or a dedicated thread, never
/// from an async task and never under an `Inbox` lock. A `get` for a cached
/// key does not block.
pub struct WorkerStore {
    worker: SecretWorker,
    cache: Mutex<Cache>,
    #[cfg(any(test, feature = "testing"))]
    read_hook: Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>,
}

impl WorkerStore {
    pub fn new(worker: SecretWorker) -> WorkerStore {
        WorkerStore {
            worker,
            cache: Mutex::new(Cache::default()),
            #[cfg(any(test, feature = "testing"))]
            read_hook: Mutex::new(None),
        }
    }

    /// Test hook: runs after a read returns from the worker and before its
    /// result is published.
    #[cfg(any(test, feature = "testing"))]
    pub fn set_read_hook(&self, hook: std::sync::Arc<dyn Fn() + Send + Sync>) {
        *self.read_hook.lock().unwrap() = Some(hook);
    }

    /// Reads `keys` now and caches each outcome, errors included. Run on
    /// the blocking pool before `Inbox::start`. Never prompts (§9).
    pub fn prefill(&self, keys: &[String]) {
        for key in keys {
            let _ = self.read_through(key);
        }
    }

    fn generation(&self, key: &str) -> u64 {
        self.cache.lock().unwrap().generations.get(key).copied().unwrap_or(0)
    }

    /// Starts a change: a read already in flight for `key` will not publish.
    fn bump(&self, key: &str) -> u64 {
        let mut c = self.cache.lock().unwrap();
        let g = c.generations.entry(key.to_owned()).or_insert(0);
        *g += 1;
        *g
    }

    /// Compare-and-publish under the cache lock (plan D37). False when a
    /// change started since `generation` was taken.
    fn publish(&self, key: &str, generation: u64, entry: Cached) -> bool {
        let mut c = self.cache.lock().unwrap();
        if c.generations.get(key).copied().unwrap_or(0) != generation {
            return false;
        }
        c.entries.insert(key.to_owned(), entry);
        true
    }

    fn read_through(&self, key: &str) -> Result<Option<String>, SecretError> {
        let generation = self.generation(key);
        let result = self.worker.read(key);
        #[cfg(any(test, feature = "testing"))]
        if let Some(hook) = self.read_hook.lock().unwrap().clone() {
            hook();
        }
        let entry = match &result {
            Ok(value) => Cached::Value(value.clone()),
            Err(e) => {
                tracing::warn!(error = e.tag(), "keyring read failed");
                Cached::of(*e)
            }
        };
        if !self.publish(key, generation, entry.clone()) {
            tracing::debug!("a keyring read finished after a change; its result was dropped");
            // Answer from what the change left in the cache.
            let current = self.cache.lock().unwrap().entries.get(key).cloned();
            return match current {
                Some(Cached::Value(v)) => Ok(v),
                Some(other) => Err(other.error().expect("an error state")),
                None => Err(SecretError::unknown()),
            };
        }
        match entry {
            Cached::Value(v) => Ok(v),
            other => Err(other.error().expect("an error state")),
        }
    }

    /// Test hook: jobs waiting for the worker.
    #[cfg(any(test, feature = "testing"))]
    pub fn pending_for_test(&self) -> usize {
        self.worker.pending_for_test()
    }

    fn after_change(&self, key: &str, generation: u64, want: Option<&str>, r: Result<(), WorkerError>) -> Result<(), SecretError> {
        match r {
            Ok(()) => {
                self.publish(key, generation, Cached::Value(want.map(str::to_owned)));
                Ok(())
            }
            // The change is still running in the serial worker (plan D36):
            // unknown until a later read, which can only run after it.
            Err(WorkerError::Ambiguous) => {
                tracing::warn!("keyring change unconfirmed; the machine's secret is unknown");
                self.publish(key, generation, Cached::Unknown);
                Err(SecretError::unknown())
            }
            // Expired or refused: nothing changed. The cached entry stands.
            Err(e) => {
                tracing::warn!(error = e.tag(), "keyring change failed");
                Err(Cached::of(e).error().expect("an error state"))
            }
        }
    }
}

impl SecretStore for WorkerStore {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        let hit = self.cache.lock().unwrap().entries.get(key).cloned();
        match hit {
            Some(Cached::Value(v)) => Ok(v),
            Some(other) => Err(other.error().expect("an error state")),
            None => self.read_through(key),
        }
    }

    /// Called only under acd-client's edit lock, so changes to one key never
    /// overlap.
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let generation = self.bump(key);
        let r = self.worker.write(key, value);
        self.after_change(key, generation, Some(value), r)
    }

    fn delete(&self, key: &str) -> Result<(), SecretError> {
        let generation = self.bump(key);
        let r = self.worker.delete(key);
        self.after_change(key, generation, None, r)
    }

    fn mark_unknown(&self, key: &str) {
        let generation = self.bump(key);
        self.publish(key, generation, Cached::Unknown);
    }
}

impl SecretControl for WorkerStore {
    fn unlock(&self) -> Result<(), SecretError> {
        self.worker.unlock().map_err(|e| {
            tracing::info!(error = e.tag(), "keyring unlock failed");
            Cached::of(e).error().expect("an error state")
        })
    }

    /// Reads `keys` again; the authoritative way out of `Unknown` and of a
    /// cached error. Entries are replaced only through `publish`.
    fn refresh(&self, keys: &[String]) {
        self.prefill(keys);
    }
}

#[cfg(any(test, feature = "testing"))]
pub mod fake {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use super::{BackendError, SecretBackend};

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum UnlockBehaviour {
        #[default]
        Succeed,
        Dismiss,
        Hang,
    }

    /// How writes and deletes misbehave.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum Hang {
        #[default]
        No,
        /// Waits for `released`, then applies: a slow D-Bus call.
        UntilReleased,
        /// Never returns: a hung D-Bus call.
        Forever,
    }

    /// An in-memory keyring. `calls` records operations and keys, never values.
    #[derive(Default)]
    pub struct FakeKeyring {
        pub items: HashMap<String, String>,
        pub locked: bool,
        pub unavailable: bool,
        /// Reads fail with `Unavailable`; writes and deletes do not.
        pub fail_reads: bool,
        /// Writes and deletes answer with a prompt, which is dismissed.
        pub prompts: bool,
        pub unlock: UnlockBehaviour,
        pub hang_writes: Hang,
        pub hang_deletes: Hang,
        /// Lets `UntilReleased` changes finish.
        pub released: bool,
        /// The next read, write or delete panics: the worker thread dies.
        pub panic_next: bool,
        pub calls: Vec<String>,
    }

    #[derive(Clone, Default)]
    pub struct FakeBackend(pub Arc<Mutex<FakeKeyring>>);

    impl FakeBackend {
        /// Records the call; the error the keyring's state forces, if any;
        /// else how a write (`deleting == false`) or delete misbehaves.
        fn enter(&self, call: String, deleting: bool) -> Result<Hang, BackendError> {
            let mut k = self.0.lock().unwrap();
            k.calls.push(call);
            if std::mem::take(&mut k.panic_next) {
                drop(k);
                panic!("the fake keyring was told to kill its worker");
            }
            if k.unavailable {
                return Err(BackendError::Unavailable);
            }
            if k.locked {
                return Err(BackendError::Locked);
            }
            if k.prompts {
                // What the real backend does with a returned prompt (D13).
                let call = k.calls.last().cloned().unwrap_or_default();
                k.calls.push(format!("dismiss {call}"));
                return Err(BackendError::Locked);
            }
            Ok(if deleting { k.hang_deletes } else { k.hang_writes })
        }

        async fn mutate(&self, hang: Hang, f: impl FnOnce(&mut HashMap<String, String>)) {
            match hang {
                Hang::No => {}
                Hang::Forever => std::future::pending::<()>().await,
                Hang::UntilReleased => {
                    while !self.0.lock().unwrap().released {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                }
            }
            f(&mut self.0.lock().unwrap().items);
        }
    }

    #[async_trait::async_trait]
    impl SecretBackend for FakeBackend {
        async fn read(&mut self, key: &str) -> Result<Option<String>, BackendError> {
            let fail = {
                let mut k = self.0.lock().unwrap();
                k.calls.push(format!("read {key}"));
                if std::mem::take(&mut k.panic_next) {
                    drop(k);
                    panic!("the fake keyring was told to kill its worker");
                }
                k.fail_reads || k.unavailable
            };
            if fail {
                return Err(BackendError::Unavailable);
            }
            if self.0.lock().unwrap().locked {
                return Err(BackendError::Locked);
            }
            Ok(self.0.lock().unwrap().items.get(key).cloned())
        }

        async fn write(&mut self, key: &str, value: &str) -> Result<(), BackendError> {
            let hang = self.enter(format!("write {key}"), false)?;
            let (key, value) = (key.to_owned(), value.to_owned());
            self.mutate(hang, move |items| {
                items.insert(key, value);
            })
            .await;
            Ok(())
        }

        async fn delete(&mut self, key: &str) -> Result<(), BackendError> {
            let hang = self.enter(format!("delete {key}"), true)?;
            let key = key.to_owned();
            self.mutate(hang, move |items| {
                items.remove(&key);
            })
            .await;
            Ok(())
        }

        async fn unlock(&mut self) -> Result<(), BackendError> {
            let behaviour = {
                let mut k = self.0.lock().unwrap();
                k.calls.push("unlock".into());
                k.unlock
            };
            match behaviour {
                UnlockBehaviour::Succeed => {
                    self.0.lock().unwrap().locked = false;
                    Ok(())
                }
                UnlockBehaviour::Dismiss => Err(BackendError::Dismissed),
                UnlockBehaviour::Hang => std::future::pending().await,
            }
        }
    }
}
