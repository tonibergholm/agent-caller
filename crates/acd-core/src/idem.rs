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
    /// Reservation order, so a client over its share loses its oldest first.
    seq: u64,
    state: State,
}

/// Default bound on reservations plus cached results.
pub const IDEM_CAPACITY: usize = 16 * 1024;

/// One client may hold at most this fraction (1/n) of the table, so a single
/// paired peer cannot starve the others whatever it sends.
pub const IDEM_CLIENT_SHARE_DIVISOR: usize = 4;

pub struct IdemCache {
    ttl: Duration,
    capacity: usize,
    per_client: usize,
    next_seq: u64,
    entries: HashMap<(String, String), Entry>,
}

impl IdemCache {
    pub fn new(ttl: Duration) -> Self {
        Self::with_limits(ttl, IDEM_CAPACITY, IDEM_CAPACITY / IDEM_CLIENT_SHARE_DIVISOR)
    }

    /// A cache holding at most `capacity` entries. When it is full, new ids
    /// are refused rather than evicting results a retry may still need.
    pub fn with_capacity(ttl: Duration, capacity: usize) -> Self {
        Self::with_limits(ttl, capacity, capacity)
    }

    /// Like `with_capacity`, with one client held to `per_client` entries. A
    /// client at its share loses its own oldest cached result to make room, or
    /// is refused when all its entries are still pending; it never touches
    /// another client's entries. `capacity` stays the daemon-wide backstop.
    pub fn with_limits(ttl: Duration, capacity: usize, per_client: usize) -> Self {
        Self { ttl, capacity, per_client: per_client.max(1), next_seq: 0, entries: HashMap::new() }
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
        let mut held = 0;
        let mut oldest_done: Option<(u64, &(String, String))> = None;
        for (k, e) in &self.entries {
            if k.0 != client { continue; }
            held += 1;
            if matches!(e.state, State::Done { .. }) && oldest_done.is_none_or(|(s, _)| e.seq < s) {
                oldest_done = Some((e.seq, k));
            }
        }
        if held >= self.per_client {
            let Some((_, victim)) = oldest_done else { return Begin::Full };
            let victim = victim.clone();
            self.entries.remove(&victim);
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        let (tx, _rx) = watch::channel(None);
        self.entries.insert(key, Entry { hash, seq, state: State::Pending(tx) });
        Begin::Run
    }

    pub fn finish(&mut self, client: &str, id: &str, result: CommandResult, cacheable: bool, now: Instant) {
        let key = (client.to_string(), id.to_string());
        let Some(e) = self.entries.remove(&key) else { return };
        let (hash, seq) = (e.hash, e.seq);
        if let State::Pending(tx) = e.state {
            let _ = tx.send(Some(result.clone()));
        }
        if cacheable {
            self.entries.insert(key, Entry { hash, seq, state: State::Done { result, at: now } });
        }
    }
}
