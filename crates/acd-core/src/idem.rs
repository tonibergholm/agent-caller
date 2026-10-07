use std::collections::HashMap;
use std::time::{Duration, Instant};

use acd_protocol::CommandResult;
use tokio::sync::watch;

/// What the caller should do with a command (spec §7.4).
pub enum Begin {
    /// No entry exists: run the command, then call `finish`.
    Run,
    /// The same command is running: wait for its result.
    Wait(watch::Receiver<Option<CommandResult>>),
    /// The command already finished: return the cached result.
    Done(CommandResult),
    /// The id was reused with a different payload.
    Mismatch,
    /// The table is full; nothing was reserved.
    Full,
}

enum State {
    Pending(watch::Sender<Option<CommandResult>>),
    Done { result: CommandResult, at: Instant },
}

struct Entry {
    hash: [u8; 32],
    state: State,
}

/// Default bound on reservations plus cached results.
pub const IDEM_CAPACITY: usize = 16 * 1024;

pub struct IdemCache {
    ttl: Duration,
    capacity: usize,
    entries: HashMap<(String, String), Entry>,
}

impl IdemCache {
    pub fn new(ttl: Duration) -> Self { Self::with_capacity(ttl, IDEM_CAPACITY) }

    /// A cache holding at most `capacity` entries. When it is full, new ids
    /// are refused rather than evicting results a retry may still need.
    pub fn with_capacity(ttl: Duration, capacity: usize) -> Self {
        Self { ttl, capacity, entries: HashMap::new() }
    }

    pub fn begin(&mut self, client: &str, id: &str, hash: [u8; 32], now: Instant) -> Begin {
        let ttl = self.ttl;
        self.entries.retain(|_, e| match &e.state {
            State::Done { at, .. } => now.duration_since(*at) < ttl,
            State::Pending(_) => true,
        });
        let key = (client.to_string(), id.to_string());
        if let Some(e) = self.entries.get(&key) {
            if e.hash != hash { return Begin::Mismatch; }
            return match &e.state {
                State::Pending(tx) => Begin::Wait(tx.subscribe()),
                State::Done { result, .. } => Begin::Done(result.clone()),
            };
        }
        if self.entries.len() >= self.capacity { return Begin::Full; }
        let (tx, _rx) = watch::channel(None);
        self.entries.insert(key, Entry { hash, state: State::Pending(tx) });
        Begin::Run
    }

    pub fn finish(&mut self, client: &str, id: &str, result: CommandResult, cacheable: bool, now: Instant) {
        let key = (client.to_string(), id.to_string());
        let Some(e) = self.entries.remove(&key) else { return };
        let hash = e.hash;
        if let State::Pending(tx) = e.state {
            let _ = tx.send(Some(result.clone()));
        }
        if cacheable {
            self.entries.insert(key, Entry { hash, state: State::Done { result, at: now } });
        }
    }
}
