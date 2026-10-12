//! The secret worker (omarchy spec §9, plan D13): one thread owns the
//! keyring connection; every job carries a deadline.
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

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
    /// A write or delete started and was cut off: it may have applied.
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

    fn call(&self, op: Op, limit: Duration) -> Result<Option<String>, WorkerError> {
        let mutates = op.mutates();
        let deadline = Instant::now() + limit;
        let (reply, rx) = std_mpsc::sync_channel(1);
        self.pending.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.tx
            .send(Job { op, deadline, reply })
            .map_err(|_| WorkerError::Backend(BackendError::Unavailable))?;
        match rx.recv_timeout(limit + Duration::from_secs(1)) {
            Ok(r) => r,
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
