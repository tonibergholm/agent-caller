//! `push.json`: the device registrations (relay spec §2.2, §4.10 F3).
//!
//! It holds relay handles, never push addresses (acd never sees one), and is
//! written owner-only (0600) through the same atomic writer as the tokens.
//! The daemon is the only writer. A file that is missing is an empty store;
//! a file that is unsafe or invalid starts the daemon with no registrations
//! and a reason, and is not rewritten until a registration replaces it.
//!
//! Nothing here logs or prints a handle, and no error carries file content
//! (serde errors can quote values, so they are never shown).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use acd_protocol::{PushDeviceStatus, PushPlatform};
use acd_relay::client::{Status, WakeState};
use serde::{Deserialize, Serialize};

pub const FILE: &str = "push.json";
/// Most devices one daemon wakes (relay spec §5.1).
pub const MAX_DEVICES: usize = 8;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const VERSION: u32 = 1;
const MAX_ID_BYTES: usize = 128;

/// Client id rule, shared by registration and `read_file`: what is stored
/// must always be loadable.
pub fn client_id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES
}

/// Handle syntax: 1 to 512 bytes of base64url (acd does not parse it).
pub fn handle_ok(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= acd_relay::wire::MAX_HANDLE_LEN
        && h.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StatusRepr {
    Ok,
    Expired,
    Invalid,
    RelayError,
}

impl From<Status> for StatusRepr {
    fn from(s: Status) -> Self {
        match s {
            Status::Ok => StatusRepr::Ok,
            Status::Expired => StatusRepr::Expired,
            Status::Invalid => StatusRepr::Invalid,
            Status::RelayError => StatusRepr::RelayError,
        }
    }
}

impl From<StatusRepr> for Status {
    fn from(s: StatusRepr) -> Self {
        match s {
            StatusRepr::Ok => Status::Ok,
            StatusRepr::Expired => Status::Expired,
            StatusRepr::Invalid => Status::Invalid,
            StatusRepr::RelayError => Status::RelayError,
        }
    }
}

/// One registered device, as stored.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub platform: PushPlatform,
    handle: String,
    /// RFC 3339, informational: the relay decides expiry (spec §7).
    pub expires_at: String,
    status: StatusRepr,
    failures: u32,
    retry_after: u64,
    last_wake_at: Option<u64>,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("platform", &self.platform)
            .field("handle_bytes", &self.handle.len())
            .finish_non_exhaustive()
    }
}

impl Device {
    pub fn new(platform: PushPlatform, handle: String, expires_at: String) -> Device {
        Device::with_state(platform, handle, expires_at, &WakeState::default())
    }

    fn with_state(platform: PushPlatform, handle: String, expires_at: String, w: &WakeState) -> Device {
        Device {
            platform,
            handle,
            expires_at,
            status: w.status.into(),
            failures: w.failures,
            retry_after: w.retry_after,
            last_wake_at: w.last_wake_at,
        }
    }

    pub fn handle(&self) -> &str {
        &self.handle
    }

    pub fn wake_state(&self) -> WakeState {
        WakeState {
            status: self.status.into(),
            failures: self.failures,
            retry_after: self.retry_after,
            last_wake_at: self.last_wake_at,
        }
    }

    pub fn set_wake_state(&mut self, w: &WakeState) {
        self.status = w.status.into();
        self.failures = w.failures;
        self.retry_after = w.retry_after;
        self.last_wake_at = w.last_wake_at;
    }

    pub fn status(&self) -> PushDeviceStatus {
        match self.status {
            StatusRepr::Ok => PushDeviceStatus::Ok,
            StatusRepr::Expired => PushDeviceStatus::Expired,
            StatusRepr::Invalid => PushDeviceStatus::Invalid,
            StatusRepr::RelayError => PushDeviceStatus::RelayError,
        }
    }

    pub fn last_wake_at(&self) -> Option<u64> {
        self.last_wake_at
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRepr {
    version: u32,
    devices: BTreeMap<String, Device>,
}

/// A device and the registration generation it belongs to. A wake outcome
/// is applied only to the generation it was sent for, so a device that
/// registers again while a wake is in flight keeps its fresh state.
#[derive(Debug, Clone)]
pub struct Entry {
    pub device: Device,
    pub generation: u64,
}

pub struct Store {
    path: PathBuf,
    devices: BTreeMap<String, Entry>,
    next_generation: u64,
    degraded: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    TooMany,
    BadId,
    Write,
}

impl Store {
    /// Never fails: an unusable file gives an empty store with a reason.
    pub fn load(dir: &Path) -> Store {
        let path = dir.join(FILE);
        let mut store = Store { path, devices: BTreeMap::new(), next_generation: 1, degraded: None };
        match read_file(&store.path) {
            Ok(None) => {}
            Ok(Some(devices)) => {
                for (id, device) in devices {
                    let generation = store.next_generation;
                    store.next_generation += 1;
                    store.devices.insert(id, Entry { device, generation });
                }
            }
            Err(why) => store.degraded = Some(why.to_string()),
        }
        store
    }

    /// Why the file was not used, if it was not.
    pub fn degraded(&self) -> Option<&str> {
        self.degraded.as_deref()
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn get(&self, client_id: &str) -> Option<&Entry> {
        self.devices.get(client_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Entry)> {
        self.devices.iter()
    }

    /// Stores (or replaces) a registration with a fresh wake state.
    pub fn register(&mut self, client_id: &str, device: Device) -> Result<(), StoreError> {
        if !client_id_ok(client_id) {
            return Err(StoreError::BadId);
        }
        if !self.devices.contains_key(client_id) && self.devices.len() >= MAX_DEVICES {
            return Err(StoreError::TooMany);
        }
        let generation = self.next_generation;
        let mut next = self.devices.clone();
        next.insert(client_id.to_string(), Entry { device, generation });
        self.commit(next)?;
        self.next_generation += 1;
        Ok(())
    }

    /// Idempotent: no write when `client_id` is not registered.
    pub fn remove(&mut self, client_id: &str) -> Result<(), StoreError> {
        if !self.devices.contains_key(client_id) {
            return Ok(());
        }
        let mut next = self.devices.clone();
        next.remove(client_id);
        self.commit(next)
    }

    /// Applies a wake outcome to `client_id`, if it is still registered at
    /// `generation`. Returns whether it changed anything.
    pub fn apply_wake(&mut self, client_id: &str, generation: u64, state: &WakeState) -> bool {
        let Some(e) = self.devices.get_mut(client_id) else {
            return false;
        };
        if e.generation != generation || e.device.wake_state() == *state {
            return false;
        }
        e.device.set_wake_state(state);
        // The state in memory is what the next wake uses; a failed write only
        // loses the backoff across a restart.
        let _ = self.persist(&self.devices);
        true
    }

    fn commit(&mut self, next: BTreeMap<String, Entry>) -> Result<(), StoreError> {
        self.persist(&next)?;
        self.devices = next;
        self.degraded = None;
        Ok(())
    }

    fn persist(&self, devices: &BTreeMap<String, Entry>) -> Result<(), StoreError> {
        let repr = FileRepr {
            version: VERSION,
            devices: devices.iter().map(|(k, v)| (k.clone(), v.device.clone())).collect(),
        };
        let bytes = serde_json::to_vec_pretty(&repr).map_err(|_| StoreError::Write)?;
        crate::secrets::write_atomic(&self.path, &bytes, 0o600).map_err(|_| StoreError::Write)
    }
}

/// Reads `push.json`. `Ok(None)`: no file. `Err`: fixed text, never content.
fn read_file(path: &Path) -> Result<Option<BTreeMap<String, Device>>, &'static str> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("push.json cannot be inspected"),
        Ok(_) => {}
    }
    let bytes = acd_securefs::read_private(path, 0o600, MAX_FILE_BYTES)
        .map_err(|_| "push.json is unsafe (mode, owner or type) or unreadable")?;
    let file: FileRepr = serde_json::from_slice(&bytes).map_err(|_| "push.json is not valid")?;
    if file.version != VERSION {
        return Err("push.json has an unknown version");
    }
    if file.devices.len() > MAX_DEVICES {
        return Err("push.json lists too many devices");
    }
    // One bad entry is skipped, never the whole file: the others still wake.
    let mut devices = file.devices;
    devices.retain(|id, d| client_id_ok(id) && handle_ok(&d.handle));
    Ok(Some(devices))
}
