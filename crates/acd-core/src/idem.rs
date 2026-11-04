use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
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
    /// Who the entry is charged to. The peer chooses its `client.id`, so a
    /// quota keyed by it alone can be dodged by changing it.
    origin: Origin,
    state: State,
}

/// Where a connection comes from, as far as the daemon can prove it: the
/// local socket, or the source address of a TLS connection. Unlike
/// `client.id`, a peer cannot choose it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    Local,
    Net(IpAddr),
}

impl Origin {
    /// An IPv4-mapped IPv6 address counts as the IPv4 address.
    pub fn net(ip: IpAddr) -> Self {
        Origin::Net(ip.to_canonical())
    }
}

/// Default bound on reservations plus cached results.
pub const IDEM_CAPACITY: usize = 16 * 1024;

/// One `client.id` may hold at most this fraction (1/n) of the table.
pub const IDEM_CLIENT_SHARE_DIVISOR: usize = 4;

/// One network source address may hold at most this fraction (1/n) of the
/// table, whatever `client.id`s it uses.
pub const IDEM_SOURCE_SHARE_DIVISOR: usize = 8;

/// The local socket has its own fixed quota (1/n of the table), carved out of
/// the share network sources can reach, so no remote peer can take it.
pub const IDEM_LOCAL_SHARE_DIVISOR: usize = 4;

/// At most this many distinct `client.id`s may hold entries from one source.
pub const IDEM_CLIENT_IDS_PER_SOURCE: usize = 8;

/// Bounds on the table. A new entry must fit under every one of them.
#[derive(Clone, Copy, Debug)]
pub struct IdemLimits {
    /// Daemon-wide backstop.
    pub capacity: usize,
    /// Entries all network sources together may hold.
    pub network: usize,
    /// Entries the local socket may hold.
    pub local: usize,
    /// Entries one `client.id` may hold.
    pub per_client: usize,
    /// Entries one network source address may hold.
    pub per_source: usize,
    /// Distinct `client.id`s one network source may hold entries under.
    pub client_ids_per_source: usize,
}

impl Default for IdemLimits {
    fn default() -> Self {
        let local = IDEM_CAPACITY / IDEM_LOCAL_SHARE_DIVISOR;
        Self {
            capacity: IDEM_CAPACITY,
            network: IDEM_CAPACITY - local,
            local,
            per_client: IDEM_CAPACITY / IDEM_CLIENT_SHARE_DIVISOR,
            per_source: IDEM_CAPACITY / IDEM_SOURCE_SHARE_DIVISOR,
            client_ids_per_source: IDEM_CLIENT_IDS_PER_SOURCE,
        }
    }
}

/// Reservations (commands running) and cached results (commands that
/// delivered). Only a delivered command is ever stored, and a stored entry is
/// never evicted before its TTL: dropping it would let a replay deliver twice.
/// When a quota is reached the new command is refused instead.
pub struct IdemCache {
    ttl: Duration,
    limits: IdemLimits,
    entries: HashMap<(String, String), Entry>,
}

impl IdemCache {
    pub fn new(ttl: Duration) -> Self {
        Self::with_limits(ttl, IdemLimits::default())
    }

    /// A cache holding at most `capacity` entries and no other bound.
    pub fn with_capacity(ttl: Duration, capacity: usize) -> Self {
        Self::with_limits(
            ttl,
            IdemLimits {
                capacity,
                network: capacity,
                local: capacity,
                per_client: capacity,
                per_source: capacity,
                client_ids_per_source: usize::MAX,
            },
        )
    }

    pub fn with_limits(ttl: Duration, limits: IdemLimits) -> Self {
        Self { ttl, limits, entries: HashMap::new() }
    }

    /// `begin_from` for the local socket.
    pub fn begin(&mut self, client: &str, id: &str, hash: [u8; 32], now: Instant) -> Begin {
        self.begin_from(client, Origin::Local, id, hash, now)
    }

    pub fn begin_from(&mut self, client: &str, origin: Origin, id: &str, hash: [u8; 32], now: Instant) -> Begin {
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
        if !self.admits(client, origin) { return Begin::Full; }
        let (tx, _rx) = watch::channel(None);
        self.entries.insert(key, Entry { hash, origin, state: State::Pending(tx) });
        Begin::Run
    }

    /// Whether a new entry for `client` from `origin` fits under every bound.
    fn admits(&self, client: &str, origin: Origin) -> bool {
        let l = &self.limits;
        if self.entries.len() >= l.capacity { return false; }
        let (mut by_client, mut by_origin, mut network) = (0, 0, 0);
        let mut ids: HashSet<&str> = HashSet::new();
        for ((c, _), e) in &self.entries {
            if matches!(e.origin, Origin::Net(_)) { network += 1; }
            if c == client { by_client += 1; }
            if e.origin == origin {
                by_origin += 1;
                ids.insert(c);
            }
        }
        if by_client >= l.per_client { return false; }
        match origin {
            Origin::Local => by_origin < l.local,
            Origin::Net(_) => {
                network < l.network
                    && by_origin < l.per_source
                    && (ids.contains(client) || ids.len() < l.client_ids_per_source)
            }
        }
    }

    pub fn finish(&mut self, client: &str, id: &str, result: CommandResult, cacheable: bool, now: Instant) {
        let key = (client.to_string(), id.to_string());
        let Some(e) = self.entries.remove(&key) else { return };
        let (hash, origin) = (e.hash, e.origin);
        if let State::Pending(tx) = e.state {
            let _ = tx.send(Some(result.clone()));
        }
        if cacheable {
            self.entries.insert(key, Entry { hash, origin, state: State::Done { result, at: now } });
        }
    }
}
