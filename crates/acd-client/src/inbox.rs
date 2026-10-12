use crate::{
    Timing,
    actions::{Action, ActionKind, ActionOwner, Actions, Semantics, TextSemantics, check_reason, notice_for},
    commands::Command,
    conn::{self, MachineHandle, MachineState, now_ms},
    endpoint::{self, Endpoint},
    settings::{EndpointConfig, MachineEntry, MachineSecret, SecretError, SecretErrorKind, SecretStatus, SecretStore, Settings},
    view::{
        self, ActionView, InboxView, MachineEntryView, MachineInput, Notice, NoticeTone,
        PairingState,
    },
    voice::{ActionEvent, ActionOutcome, ConnStamp, Guard, NotSentReason, StampedView},
};
use acd_protocol::{Effect, pairing::Pairing};
use std::sync::Mutex;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};
use std::{net::IpAddr, time::Instant};
use tokio::sync::{Notify, broadcast, watch};

/// Longest typed answer, matching the T3 adapter's prompt cap.
pub const ANSWER_TEXT_MAX_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub struct InboxOptions {
    pub timing: Timing,
    pub settings_path: Option<PathBuf>,
    pub client_name: String,
    pub client_version: String,
}
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PickError {
    #[error("unknown card")]
    UnknownCard,
    #[error("this request cannot be answered here")]
    NotAnswerable,
    #[error("The request changed. Review it again.")]
    StaleRequest,
    #[error("unknown or unselectable option")]
    UnknownOption,
    #[error("a countdown is already running for this card")]
    AlreadyCounting,
    #[error("the reply is empty or longer than 64 KiB")]
    BadText,
    #[error("the answer is empty, too long, or has control, bidi or separator characters")]
    BadAnswerText,
    #[error("replies are not possible for this agent right now")]
    CannotReply,
    #[error("The connection changed. Nothing was sent.")]
    ConnectionChanged,
    #[error("The agent is hidden now. Nothing was sent.")]
    Hidden,
}
/// Why a machine operation failed. `Display` keeps the texts the Mac app
/// shows today.
#[derive(Debug, thiserror::Error)]
pub enum MachineError {
    #[error("Unknown machine")]
    UnknownMachine,
    #[error("That is not an acd pairing line. Run `acd pair` on the machine and copy its output.")]
    BadPairing,
    #[error("This machine is not paired")]
    NotPaired,
    #[error("That address is not a Tailscale or loopback address. Use the machine's Tailscale name or address.")]
    NotTailscale,
    #[error("Could not read machine credentials")]
    SecretRead(SecretError),
    #[error("Could not save machine credentials")]
    SecretWrite(SecretError),
    #[error("{0}")]
    SecretDelete(SecretError),
    #[error("{0}")]
    Settings(String),
    #[error("Machine pairing recovery failed")]
    PairRecoveryFailed,
    #[error("Machine removal recovery failed")]
    RemoveRecoveryFailed,
}

/// Serializes every change to machines or to the saved settings: pair,
/// rename, remove, reconnect and each settings write (plan D3).
/// SecretStore calls may run under it; they never run under `inner`.
/// Timers, cancels, views, `machines()` and `settings()` never take it.
#[derive(Default)]
struct EditLock {
    lock: Mutex<()>,
    waiting: AtomicUsize,
}

impl EditLock {
    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        let guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        guard
    }
}

struct Machine {
    entry: MachineEntry,
    handle: Option<MachineHandle>,
}
fn default_machine_name(host: &str) -> String {
    if host.parse::<IpAddr>().is_ok() {
        return host.to_owned();
    }
    host.split('.')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(host)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::default_machine_name;

    #[test]
    fn default_names_preserve_ip_addresses_and_use_dns_first_label() {
        assert_eq!(default_machine_name("127.0.0.1"), "127.0.0.1");
        assert_eq!(default_machine_name("2001:db8::1"), "2001:db8::1");
        assert_eq!(default_machine_name("box.example.org"), "box");
    }
}
struct Inner {
    settings: Settings,
    machines: Vec<Machine>,
    actions: Actions,
    busy: HashSet<String>,
    notices: HashMap<String, (Notice, Instant)>,
    #[allow(dead_code)] // Reserved for the notification state added by Task 11.
    notified: HashSet<(String, String)>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum InboxEvent {
    RequestOpened {
        machine_key: String,
        machine_name: String,
        agent_title: String,
        request_title: String,
        request_id: String,
    },
}

/// How many action outcomes the broadcast holds before a slow receiver lags.
pub const OUTCOME_CHANNEL_CAPACITY: usize = 256;
pub struct Inbox {
    inner: Mutex<Inner>,
    edits: EditLock,
    secrets: Arc<dyn SecretStore>,
    opts: InboxOptions,
    view_tx: watch::Sender<InboxView>,
    events_tx: broadcast::Sender<InboxEvent>,
    outcomes_tx: broadcast::Sender<ActionEvent>,
    stamped_tx: watch::Sender<Arc<StampedView>>,
    /// Test hook: while set, `rebuild` publishes nothing.
    rebuild_held: AtomicBool,
    wake: Notify,
    next_instance: AtomicU64,
    started: std::sync::atomic::AtomicBool,
}
/// Who asks for an action, and what it was planned on (voice guards).
#[derive(Clone, Copy)]
struct Origin<'a> {
    owner: ActionOwner,
    expect: Option<&'a Guard>,
}

impl Origin<'_> {
    const KEYS: Origin<'static> = Origin { owner: ActionOwner::DEFAULT, expect: None };
    fn owned(owner: ActionOwner) -> Origin<'static> {
        Origin { owner, expect: None }
    }
}

impl Inbox {
    pub fn new(settings: Settings, secrets: Arc<dyn SecretStore>, opts: InboxOptions) -> Arc<Self> {
        let initial = InboxView {
            machines: vec![],
            cards: vec![],
            hidden_idle: 0,
            show_idle: settings.show_idle,
            show_subagents: settings.show_subagents,
            show_hook_only: settings.show_hook_only,
            tray: view::TraySummary {
                color: view::TrayColor::Grey,
                needs_you: 0,
            },
            actions: vec![],
        };
        let (stamped_tx, _) = watch::channel(Arc::new(StampedView {
            view: initial.clone(),
            hidden_idle_cards: vec![],
            stamps: HashMap::new(),
        }));
        let (tx, _) = watch::channel(initial);
        let (events_tx, _) = broadcast::channel(64);
        let (outcomes_tx, _) = broadcast::channel(OUTCOME_CHANNEL_CAPACITY);
        Arc::new(Self {
            inner: Mutex::new(Inner {
                machines: settings
                    .machines
                    .iter()
                    .cloned()
                    .map(|entry| Machine {
                        entry,
                        handle: None,
                    })
                    .collect(),
                settings,
                actions: Actions::default(),
                busy: HashSet::new(),
                notices: HashMap::new(),
                notified: HashSet::new(),
            }),
            edits: EditLock::default(),
            secrets,
            opts,
            view_tx: tx,
            events_tx,
            outcomes_tx,
            stamped_tx,
            rebuild_held: AtomicBool::new(false),
            wake: Notify::new(),
            next_instance: AtomicU64::new(1),
            started: std::sync::atomic::AtomicBool::new(false),
        })
    }
    pub fn view(&self) -> watch::Receiver<InboxView> {
        self.view_tx.subscribe()
    }
    pub fn events(&self) -> broadcast::Receiver<InboxEvent> {
        self.events_tx.subscribe()
    }
    /// Outcomes of every countdown action (voice spec §5.6). Subscribe
    /// before starting an action. A lagged receiver must cancel or treat as
    /// uncertain the actions it watches.
    pub fn action_outcomes(&self) -> broadcast::Receiver<ActionEvent> {
        self.outcomes_tx.subscribe()
    }

    fn emit(&self, action: &Action, outcome: ActionOutcome) {
        let _ = self.outcomes_tx.send(ActionEvent {
            action_id: action.id.clone(),
            card_key: action.card_key.clone(),
            outcome,
        });
    }

    /// The view, the idle cards it hides, and the stamps of the machine
    /// states it was built from, all from one rebuild (voice plan D5).
    pub fn stamped_view(&self) -> Arc<StampedView> {
        self.stamped_tx.borrow().clone()
    }

    /// The machine's live connection.
    pub fn conn_stamp(&self, machine_key: &str) -> Option<ConnStamp> {
        let handle = {
            let inner = self.inner.lock().unwrap();
            inner.machines.iter().find(|m| m.entry.key == machine_key)?.handle.clone()?
        };
        let state = handle.state();
        let s = state.borrow();
        Some(ConnStamp { instance: s.instance, conn_gen: s.conn_gen })
    }

    /// Test hook: hold `rebuild`, as if the timer loop were slow.
    #[cfg(feature = "testing")]
    pub fn hold_rebuild_for_test(&self, held: bool) {
        self.rebuild_held.store(held, Ordering::SeqCst);
        self.wake();
    }

    /// Test hook: end every countdown now.
    #[cfg(feature = "testing")]
    pub fn expire_countdowns_for_test(&self) {
        self.inner.lock().unwrap().actions.expire_all(Instant::now());
        self.wake();
    }
    /// Test hook: callers blocked on the edit lock right now.
    #[cfg(feature = "testing")]
    pub fn edit_waiters_for_test(&self) -> usize {
        self.edits.waiting.load(Ordering::SeqCst)
    }
    pub fn machines(&self) -> Vec<MachineEntryView> {
        // The SecretStore is never called under `inner` (omarchy spec §4.3).
        let entries = self.inner.lock().unwrap().settings.machines.clone();
        entries
            .iter()
            .map(|entry| {
                let secret = match &entry.endpoint {
                    EndpointConfig::Local => SecretStatus::Ok,
                    EndpointConfig::Tcp { .. } => self.secrets.status(&entry.key),
                };
                let pairing = match secret {
                    SecretStatus::Ok => PairingState::Paired,
                    SecretStatus::Missing => PairingState::NotPaired,
                    SecretStatus::Locked | SecretStatus::Unreadable | SecretStatus::Unknown => PairingState::Unreadable,
                };
                MachineEntryView {
                    key: entry.key.clone(),
                    name: entry.name.clone(),
                    endpoint: entry.endpoint.clone(),
                    pairing,
                    secret,
                }
            })
            .collect()
    }
    pub fn import_pairing(self: &Arc<Self>, line: &str) -> Result<MachineEntry, String> {
        self.pair_machine(line).map_err(|e| e.to_string())
    }

    /// Pairs a machine from an `acd pair` line. The secret is stored first,
    /// outside `inner`; the machine is then committed under it. A failed
    /// commit deletes a new machine's secret or restores a re-paired
    /// machine's previous one (omarchy spec §4.3, §9).
    pub fn pair_machine(self: &Arc<Self>, line: &str) -> Result<MachineEntry, MachineError> {
        let pairing = Pairing::parse(line).map_err(|_| MachineError::BadPairing)?;
        // Refused before any secret is written; the connect-time rule
        // (`endpoint::vet`) would refuse it later anyway.
        if endpoint::refuses_literal(&pairing.host) {
            return Err(MachineError::NotTailscale);
        }
        let _edit = self.edits.lock();
        let existing = self
            .inner
            .lock()
            .unwrap()
            .settings
            .machines
            .iter()
            .find(|m| matches!(&m.endpoint, EndpointConfig::Tcp { host, port } if host == &pairing.host && *port == pairing.port))
            .cloned();
        let entry = existing.unwrap_or_else(|| MachineEntry {
            key: uuid::Uuid::new_v4().to_string(),
            name: default_machine_name(&pairing.host),
            endpoint: EndpointConfig::Tcp { host: pairing.host.clone(), port: pairing.port },
        });
        let secret = MachineSecret { token: pairing.token, pin: pairing.pin };
        /// What a failed commit puts back (D36).
        enum Restore {
            Previous(Option<String>),
            /// The previous item is unknown: nothing to restore, no guess.
            Nothing,
        }
        let restore = match self.secrets.get(&entry.key) {
            Ok(previous) => Restore::Previous(previous),
            Err(e) if e.kind() == SecretErrorKind::Unknown => Restore::Nothing,
            Err(e) => {
                self.fail_closed(&entry.key, &e);
                return Err(MachineError::SecretRead(e));
            }
        };
        self.secrets
            .set(&entry.key, &secret.to_json())
            .map_err(|e| {
                self.fail_closed(&entry.key, &e);
                MachineError::SecretWrite(e)
            })?;
        let info = tcp_info(&pairing.host, pairing.port, &secret);
        let mut inner = self.inner.lock().unwrap();
        let mut candidate = inner.settings.clone();
        candidate.machines.retain(|m| m.key != entry.key);
        candidate.machines.push(entry.clone());
        if let Some(path) = &self.opts.settings_path
            && let Err(error) = candidate.save(path)
        {
            drop(inner);
            let recovery = match restore {
                Restore::Previous(Some(value)) => self.secrets.set(&entry.key, &value),
                Restore::Previous(None) => self.secrets.delete(&entry.key),
                Restore::Nothing => {
                    self.secrets.mark_unknown(&entry.key);
                    self.fail_closed(&entry.key, &SecretError::unknown());
                    return Err(MachineError::Settings(error.to_string()));
                }
            };
            if recovery.is_err() {
                let mut inner = self.inner.lock().unwrap();
                if let Some(machine) = inner.machines.iter_mut().find(|m| m.entry.key == entry.key)
                    && let Some(handle) = machine.handle.take()
                {
                    handle.cancel();
                }
                eprintln!("machine pairing recovery failed");
                return Err(MachineError::PairRecoveryFailed);
            }
            return Err(MachineError::Settings(error.to_string()));
        }
        inner.settings = candidate;
        if let Some(old) = inner.machines.iter().position(|m| m.entry.key == entry.key) {
            for action in inner.actions.end_machine(&entry.key) {
                self.emit(&action, ActionOutcome::NotSent { reason: NotSentReason::Offline });
                inner.notices.insert(
                    action.card_key,
                    (
                        notice("Offline. Nothing was sent.", NoticeTone::Warning),
                        Instant::now() + self.opts.timing.notice_ttl,
                    ),
                );
            }
            if let Some(handle) = inner.machines[old].handle.take() {
                handle.cancel();
            }
            inner.machines.remove(old);
        }
        let instance = self.next_instance.fetch_add(1, Ordering::Relaxed);
        let client_id = inner.settings.client_id.clone();
        let handle = spawn_with(&entry, info, &self.opts, instance, &client_id);
        inner.machines.push(Machine { entry: entry.clone(), handle: Some(handle.clone()) });
        drop(inner);
        self.watch_machine(entry.key.clone(), handle);
        self.wake();
        Ok(entry)
    }
    pub fn rename_machine(&self, key: &str, name: &str) -> Result<(), String> {
        let _edit = self.edits.lock();
        let mut inner = self.inner.lock().unwrap();
        let mut candidate_settings = inner.settings.clone();
        let mut candidate = candidate_settings
            .machines
            .iter()
            .find(|m| m.key == key)
            .cloned()
            .ok_or("Unknown machine")?;
        candidate.name = name.to_owned();
        candidate.validate().map_err(|e| e.to_string())?;
        candidate_settings
            .machines
            .iter_mut()
            .find(|m| m.key == key)
            .unwrap()
            .name = name.to_owned();
        if let Some(path) = &self.opts.settings_path {
            candidate_settings.save(path).map_err(|e| e.to_string())?;
        }
        inner.settings = candidate_settings;
        if let Some(machine) = inner.machines.iter_mut().find(|m| m.entry.key == key) {
            machine.entry.name = name.to_owned();
        }
        drop(inner);
        self.wake();
        Ok(())
    }
    pub fn remove_machine(&self, key: &str) -> Result<(), String> {
        self.remove_machine_checked(key).map_err(|e| e.to_string())
    }

    /// Removes a machine and its secret. SecretStore calls run outside
    /// `inner`; the settings file is written first, the secret deleted
    /// second, and the file restored if the delete fails.
    pub fn remove_machine_checked(&self, key: &str) -> Result<(), MachineError> {
        let _edit = self.edits.lock();
        if !self.inner.lock().unwrap().settings.machines.iter().any(|m| m.key == key) {
            return Err(MachineError::UnknownMachine);
        }
        self.secrets.get(key).map_err(MachineError::SecretRead)?;
        {
            let inner = self.inner.lock().unwrap();
            let mut candidate = inner.settings.clone();
            candidate.machines.retain(|m| m.key != key);
            if let Some(path) = &self.opts.settings_path {
                candidate.save(path).map_err(|e| MachineError::Settings(e.to_string()))?;
            }
        }
        if let Err(error) = self.secrets.delete(key) {
            self.fail_closed(key, &error);
            let mut inner = self.inner.lock().unwrap();
            // `inner.settings` still lists the machine: it is the old file.
            if let Some(path) = &self.opts.settings_path
                && inner.settings.save(path).is_err()
            {
                if let Some(machine) = inner.machines.iter_mut().find(|m| m.entry.key == key)
                    && let Some(handle) = machine.handle.take()
                {
                    handle.cancel();
                }
                eprintln!("machine removal recovery failed");
                return Err(MachineError::RemoveRecoveryFailed);
            }
            return Err(MachineError::SecretDelete(error));
        }
        // The file already matches: every settings writer holds `edits`, so
        // none can have saved this machine back since (plan D3).
        let mut inner = self.inner.lock().unwrap();
        inner.settings.machines.retain(|m| m.key != key);
        for machine in inner.machines.iter().filter(|m| m.entry.key == key) {
            if let Some(handle) = &machine.handle {
                handle.cancel();
            }
        }
        inner.machines.retain(|m| m.entry.key != key);
        for action in inner.actions.end_machine(key) {
            self.emit(&action, ActionOutcome::NotSent { reason: NotSentReason::Offline });
            inner.notices.insert(
                action.card_key,
                (
                    notice("Offline. Nothing was sent.", NoticeTone::Warning),
                    Instant::now() + self.opts.timing.notice_ttl,
                ),
            );
        }
        drop(inner);
        self.wake();
        Ok(())
    }
    pub fn retry_machine(&self, key: &str) {
        if let Some(handle) = self
            .inner
            .lock()
            .unwrap()
            .machines
            .iter()
            .find(|m| m.entry.key == key)
            .and_then(|m| m.handle.clone())
        {
            handle.retry_now();
        }
    }

    /// Retries a machine's connection, or creates its handle if it has none,
    /// for example because its secret could not be read at start (omarchy
    /// spec §4.3, plan D6). The secret is read outside `inner`.
    pub fn reconnect_machine(self: &Arc<Self>, key: &str) -> Result<(), MachineError> {
        let _edit = self.edits.lock();
        let (entry, handle) = {
            let inner = self.inner.lock().unwrap();
            let m = inner.machines.iter().find(|m| m.entry.key == key).ok_or(MachineError::UnknownMachine)?;
            (m.entry.clone(), m.handle.clone())
        };
        if let Some(handle) = handle {
            handle.retry_now();
            return Ok(());
        }
        let info = match &entry.endpoint {
            EndpointConfig::Local => connect_info(&entry, &self.secrets).ok_or(MachineError::NotPaired)?,
            EndpointConfig::Tcp { host, port } => {
                let raw = self
                    .secrets
                    .get(&entry.key)
                    .map_err(MachineError::SecretRead)?
                    .ok_or(MachineError::NotPaired)?;
                let secret = MachineSecret::from_json(&raw).map_err(MachineError::SecretRead)?;
                tcp_info(host, *port, &secret)
            }
        };
        let handle = {
            let mut inner = self.inner.lock().unwrap();
            let client_id = inner.settings.client_id.clone();
            let instance = self.next_instance.fetch_add(1, Ordering::Relaxed);
            let Some(machine) = inner.machines.iter_mut().find(|m| m.entry == entry) else {
                return Err(MachineError::UnknownMachine);
            };
            if let Some(existing) = &machine.handle {
                existing.retry_now();
                return Ok(());
            }
            let handle = spawn_with(&entry, info, &self.opts, instance, &client_id);
            machine.handle = Some(handle.clone());
            handle
        };
        self.watch_machine(entry.key.clone(), handle);
        self.wake();
        Ok(())
    }

    /// After a SecretStore error of kind `Unknown`, the machine's credentials
    /// are uncertain: stop its handle and end its countdowns (fail closed).
    /// Only a successful read (`reconnect_machine`) or a re-pairing brings
    /// it back.
    fn fail_closed(&self, key: &str, e: &SecretError) {
        if e.kind() != SecretErrorKind::Unknown {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        if let Some(machine) = inner.machines.iter_mut().find(|m| m.entry.key == key)
            && let Some(handle) = machine.handle.take()
        {
            handle.cancel();
        }
        for action in inner.actions.end_machine(key) {
            self.emit(&action, ActionOutcome::NotSent { reason: NotSentReason::Offline });
        }
        drop(inner);
        self.wake();
    }
    pub fn settings(&self) -> Settings {
        self.inner.lock().unwrap().settings.clone()
    }
    pub fn set_hotkey(&self, hotkey: &str) -> Result<(), String> {
        if hotkey.trim().is_empty() {
            return Err("Hotkey cannot be empty".into());
        }
        let _edit = self.edits.lock();
        let mut inner = self.inner.lock().unwrap();
        let mut candidate = inner.settings.clone();
        candidate.hotkey = hotkey.to_owned();
        if let Some(path) = &self.opts.settings_path {
            candidate.save(path).map_err(|e| e.to_string())?;
        }
        inner.settings = candidate;
        Ok(())
    }
    /// Shows or hides subagent agents everywhere (cards, counts, tray,
    /// notifications) and remembers the choice.
    pub fn set_show_subagents(&self, on: bool) -> Result<(), String> {
        let _edit = self.edits.lock();
        let mut inner = self.inner.lock().unwrap();
        let mut candidate = inner.settings.clone();
        candidate.show_subagents = on;
        if let Some(path) = &self.opts.settings_path {
            candidate.save(path).map_err(|e| e.to_string())?;
        }
        inner.settings = candidate;
        for a in drop_hidden_actions(&mut inner) {
            self.emit(&a, ActionOutcome::NotSent { reason: NotSentReason::Hidden });
        }
        drop(inner);
        self.wake();
        Ok(())
    }
    /// Shows or hides hook-only agents that have no request, and remembers
    /// the choice. A pending request always shows.
    pub fn set_show_hook_only(&self, on: bool) -> Result<(), String> {
        let _edit = self.edits.lock();
        let mut inner = self.inner.lock().unwrap();
        let mut candidate = inner.settings.clone();
        candidate.show_hook_only = on;
        if let Some(path) = &self.opts.settings_path {
            candidate.save(path).map_err(|e| e.to_string())?;
        }
        inner.settings = candidate;
        for a in drop_hidden_actions(&mut inner) {
            self.emit(&a, ActionOutcome::NotSent { reason: NotSentReason::Hidden });
        }
        drop(inner);
        self.wake();
        Ok(())
    }
    #[cfg(feature = "testing")]
    pub fn drop_connections_for_test(&self) {
        for m in &self.inner.lock().unwrap().machines {
            if let Some(h) = &m.handle {
                h.drop_connection();
            }
        }
    }
    fn watch_machine(self: &Arc<Self>, key: String, handle: MachineHandle) {
        let ib = self.clone();
        let mut state = handle.state();
        let mut events = handle.events();
        tokio::spawn(async move {
            loop {
                tokio::select! { r=state.changed()=>{if r.is_err(){break} ib.wake.notify_one();}, e=events.recv()=>match e { Ok((instance,event))=>ib.on_machine_event(&key,instance,event).await, Err(broadcast::error::RecvError::Lagged(_))=>{let instance=state.borrow().instance; ib.end_machine(&key,instance).await;}, Err(_)=>break } }
            }
        });
    }
    pub fn start(self: &Arc<Self>) {
        let pending: Vec<MachineEntry> = {
            let inner = self.inner.lock().unwrap();
            if self.started.swap(true, Ordering::AcqRel) {
                return;
            }
            inner.machines.iter().filter(|m| m.handle.is_none()).map(|m| m.entry.clone()).collect()
        };
        // Secrets are read outside `inner` (omarchy spec §4.3).
        let infos: Vec<(MachineEntry, Option<(Endpoint, conn::Credential)>)> = pending
            .into_iter()
            .map(|entry| {
                let info = connect_info(&entry, &self.secrets);
                (entry, info)
            })
            .collect();
        let handles = {
            let mut inner = self.inner.lock().unwrap();
            let client_id = inner.settings.client_id.clone();
            for (entry, info) in infos {
                let Some(info) = info else { continue };
                let Some(machine) = inner.machines.iter_mut().find(|m| m.entry == entry && m.handle.is_none()) else {
                    continue;
                };
                let instance = self.next_instance.fetch_add(1, Ordering::Relaxed);
                machine.handle = Some(spawn_with(&entry, info, &self.opts, instance, &client_id));
            }
            inner
                .machines
                .iter()
                .filter_map(|m| m.handle.clone().map(|h| (m.entry.key.clone(), h)))
                .collect::<Vec<_>>()
        };
        for (key, handle) in handles {
            self.watch_machine(key, handle);
        }
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                this.fire_due().await;
                this.rebuild().await;
                let due = {
                    let inner = this.inner.lock().unwrap();
                    inner
                        .actions
                        .next_due()
                        .into_iter()
                        .chain(inner.notices.values().map(|(_, t)| *t))
                        .min()
                };
                match due {
                    Some(t) => {
                        tokio::select! {_ = this.wake.notified()=>{},_ = tokio::time::sleep_until(tokio::time::Instant::from_std(t))=>{}}
                    }
                    None => this.wake.notified().await,
                }
            }
        });
    }
    fn wake(&self) {
        self.wake.notify_one();
    }
    async fn rebuild(&self) {
        if self.rebuild_held.load(Ordering::SeqCst) {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        let now = Instant::now();
        inner.notices.retain(|_, (_, expires)| *expires > now);
        let states: HashMap<String, MachineState> = inner
            .machines
            .iter()
            .filter_map(|m| {
                m.handle
                    .as_ref()
                    .map(|h| (m.entry.key.clone(), h.state().borrow().clone()))
            })
            .collect();
        // Semantic validation runs on every published rebuild; the timer loop also validates immediately before firing.
        for (key, state) in &states {
            let invalid = inner.actions.remove_invalid(key, |a| check_reason(a, state).err());
            for (action, reason) in invalid {
                self.emit(&action, ActionOutcome::NotSent { reason });
                inner
                    .notices
                    .insert(action.card_key, (notice_for(reason), now + self.opts.timing.notice_ttl));
            }
        }
        let inputs: Vec<MachineInput> = inner
            .machines
            .iter()
            .map(|m| MachineInput {
                entry: &m.entry,
                state: states.get(&m.entry.key),
            })
            .collect();
        let action_views = inner.actions.views();
        let notices = inner
            .notices
            .iter()
            .map(|(k, (n, _))| (k.clone(), n.clone()))
            .collect();
        let busy = inner.busy.clone();
        let input = view::ViewInput {
            machines: inputs,
            actions: &action_views,
            busy: &busy,
            notices: &notices,
            show_idle: inner.settings.show_idle,
            show_subagents: inner.settings.show_subagents,
            show_hook_only: inner.settings.show_hook_only,
        };
        let v = view::build_view(&input);
        // Voice (plan D5, D7): the idle cards this view hides, and the
        // stamps of the very states it was built from, published together.
        let hidden_idle_cards = if input.show_idle {
            Vec::new()
        } else {
            view::build_view(&view::ViewInput { show_idle: true, ..input })
                .cards
                .into_iter()
                .filter(|c| c.group == view::CardGroup::Idle && !v.cards.iter().any(|s| s.key == c.key))
                .collect()
        };
        let stamps = states
            .iter()
            .map(|(k, s)| (k.clone(), ConnStamp { instance: s.instance, conn_gen: s.conn_gen }))
            .collect();
        self.stamped_tx.send_replace(Arc::new(StampedView { view: v.clone(), hidden_idle_cards, stamps }));
        self.view_tx.send_replace(v);
    }
    async fn fire_due(self: &Arc<Self>) {
        let due = { self.inner.lock().unwrap().actions.due(Instant::now()) };
        for action in due {
            // The countdown ended: `cancel(id)` can no longer stop it.
            self.emit(&action, ActionOutcome::Fired);
            let data = {
                let inner = self.inner.lock().unwrap();
                let m = inner
                    .machines
                    .iter()
                    .find(|m| m.entry.key == action.machine_key);
                m.and_then(|m| {
                    m.handle.clone().map(|h| {
                        let state = h.state().borrow().clone();
                        (h, state)
                    })
                })
            };
            let Some((handle, state)) = data else {
                self.not_sent(&action, NotSentReason::Offline).await;
                continue;
            };
            if let Err(reason) = check_reason(&action, &state) {
                self.not_sent(&action, reason).await;
                continue;
            }
            let validation_action = action.clone();
            let cmd = match action.kind {
                ActionKind::Answer(s) => Command::Answer {
                    request_id: s.request_id,
                    revision: s.revision,
                    option_id: s.option.id,
                },
                ActionKind::AnswerText(s) => Command::AnswerText {
                    request_id: s.request_id,
                    revision: s.revision,
                    text: s.text,
                },
                ActionKind::Reply { text } => Command::Send {
                    agent_id: action.agent_id.clone(),
                    text,
                },
            };
            {
                self.inner
                    .lock()
                    .unwrap()
                    .busy
                    .insert(action.card_key.clone());
            }
            let ib = self.clone();
            tokio::spawn(async move {
                let latest = { handle.state().borrow().clone() };
                let out = match check_reason(&validation_action, &latest) {
                    Ok(()) => Some(handle.submit(cmd, Some(action.conn_gen)).await),
                    Err(reason) => {
                        ib.emit(&validation_action, ActionOutcome::NotSent { reason });
                        let mut inner = ib.inner.lock().unwrap();
                        inner.notices.insert(
                            validation_action.card_key.clone(),
                            (notice_for(reason), Instant::now() + ib.opts.timing.notice_ttl),
                        );
                        None
                    }
                };
                if let Some(out) = &out {
                    ib.emit(&validation_action, ActionOutcome::from_submit(out));
                }
                let mut inner = ib.inner.lock().unwrap();
                inner.busy.remove(&action.card_key);
                if let Some(out) = out
                    && let Some(n) = view::notice_for_outcome(&out)
                {
                    inner.notices.insert(
                        action.card_key,
                        (n, Instant::now() + ib.opts.timing.notice_ttl),
                    );
                }
                drop(inner);
                ib.wake();
            });
        }
    }

    /// An action that ended without being sent: notice and outcome (D1).
    async fn not_sent(&self, action: &Action, reason: NotSentReason) {
        self.emit(action, ActionOutcome::NotSent { reason });
        self.set_notice(&action.card_key, notice_for(reason)).await;
    }
    async fn set_notice(&self, key: &str, n: Notice) {
        self.inner.lock().unwrap().notices.insert(
            key.into(),
            (n, Instant::now() + self.opts.timing.notice_ttl),
        );
        self.wake();
    }
    async fn on_machine_event(&self, key: &str, instance: u64, event: conn::MachineEvent) {
        let mut inner = self.inner.lock().unwrap();
        let Some(m) = inner.machines.iter().find(|m| m.entry.key == key) else {
            return;
        };
        let current = m.handle.as_ref().map(|h| h.state().borrow().instance);
        if current != Some(instance) {
            return;
        }
        match event {
            conn::MachineEvent::ConnectionEnded { generation } => {
                for a in inner.actions.end_generation(key, generation) {
                self.emit(&a, ActionOutcome::NotSent { reason: NotSentReason::Offline });
                    inner.notices.insert(
                        a.card_key,
                        (
                            notice("Offline. Nothing was sent.", NoticeTone::Warning),
                            Instant::now() + self.opts.timing.notice_ttl,
                        ),
                    );
                }
            }
            conn::MachineEvent::Opened(req) => {
                let machine = inner.machines.iter().find(|m| m.entry.key == key).unwrap();
                let machine_name = machine.entry.name.clone();
                let state = machine.handle.as_ref().unwrap().state();
                let state = state.borrow();
                let machine_id = state
                    .session
                    .machine
                    .as_ref()
                    .map(|m| m.id.clone())
                    .unwrap_or_else(|| key.to_owned());
                let agent = state.session.agents.get(&req.agent_id);
                let hidden_subagent = !inner.settings.show_subagents
                    && agent.is_some_and(|a| a.role == acd_protocol::AgentRole::Subagent);
                let agent_title = agent
                    .map(|a| a.title.clone())
                    .unwrap_or_else(|| req.agent_id.clone());
                drop(state);
                if !hidden_subagent && inner.notified.insert((machine_id, req.id.clone())) {
                    let _ = self.events_tx.send(InboxEvent::RequestOpened {
                        machine_key: key.to_owned(),
                        machine_name,
                        agent_title,
                        request_title: req.title,
                        request_id: req.id,
                    });
                }
            }
            conn::MachineEvent::Resolved {
                agent_id,
                by,
                reason,
                ..
            } => {
                if let Some(n) = view::resolution_notice(by, reason) {
                    inner.notices.insert(
                        view::card_key(key, &agent_id),
                        (n, Instant::now() + self.opts.timing.notice_ttl),
                    );
                }
            }
            conn::MachineEvent::Welcomed { .. } | conn::MachineEvent::Ready { .. } => {}
        }
        drop(inner);
        self.wake();
    }
    async fn end_machine(&self, key: &str, instance: u64) {
        let mut inner = self.inner.lock().unwrap();
        let Some(machine) = inner.machines.iter().find(|m| m.entry.key == key) else {
            return;
        };
        if machine.handle.as_ref().map(|h| h.state().borrow().instance) != Some(instance) {
            return;
        }
        for a in inner.actions.end_machine(key) {
                self.emit(&a, ActionOutcome::NotSent { reason: NotSentReason::Offline });
            inner.notices.insert(
                a.card_key,
                (
                    notice("Offline. Nothing was sent.", NoticeTone::Warning),
                    Instant::now() + self.opts.timing.notice_ttl,
                ),
            );
        }
        drop(inner);
        self.wake();
    }
    fn pick_with(
        self: &Arc<Self>,
        origin: Origin<'_>,
        card_key: &str,
        request_id: &str,
        option_id: &str,
        revision: u64,
    ) -> Result<String, PickError> {
        let mut inner = self.inner.lock().unwrap();
        let (machine_key, handle, entry) = find_card(&inner, card_key)?;
        let state = handle.state().borrow().clone();
        guard(&inner.settings, &state, card_key, origin.expect)?;
        let current_view = self.view_tx.borrow().clone();
        let card = current_view
            .cards
            .iter()
            .find(|c| c.key == card_key)
            .cloned();
        if card.is_none() {
            return Err(PickError::UnknownCard);
        }
        if state.status != conn::MachineStatus::Online || !state.session.has("answer") {
            return Err(PickError::NotAnswerable);
        }
        let agent_id = card_key
            .split_once('/')
            .map(|(_, id)| id)
            .ok_or(PickError::UnknownCard)?;
        let agent = state
            .session
            .agents
            .get(agent_id)
            .ok_or(PickError::StaleRequest)?;
        let req = agent.pending.as_ref().ok_or(PickError::StaleRequest)?;
        if req.id != request_id || req.revision != revision {
            return Err(PickError::StaleRequest);
        }
        if !req.is_answerable() || req.status != acd_protocol::RequestStatus::Open {
            return Err(PickError::NotAnswerable);
        }
        if !req
            .options
            .iter()
            .any(|o| o.id == option_id && o.effect != Effect::Unknown)
        {
            return Err(PickError::UnknownOption);
        }
        let sem = Semantics::of(req, option_id).ok_or(PickError::UnknownOption)?;
        let label = sem.option.label.clone();
        let detail = (sem.option.effect == Effect::AllowAlways)
            .then(|| sem.option.description.clone())
            .flatten();
        let now = Instant::now();
        let id = uuid::Uuid::new_v4().to_string();
        let a = Action {
            id: id.clone(),
            owner: origin.owner,
            card_key: card_key.into(),
            machine_key,
            agent_id: agent.id.clone(),
            instance: state.instance,
            conn_gen: state.conn_gen,
            kind: ActionKind::Answer(sem),
            fires_at: now + self.opts.timing.undo,
            view: ActionView {
                id: id.clone(),
                card_key: card_key.into(),
                label,
                detail,
                fires_at_ms: now_ms() + self.opts.timing.undo.as_secs_f64() * 1000.,
            },
        };
        let _ = entry;
        inner.actions.add(a)?;
        drop(inner);
        self.wake();
        Ok(id)
    }
    fn answer_text_with(
        self: &Arc<Self>,
        origin: Origin<'_>,
        card_key: &str,
        request_id: &str,
        revision: u64,
        text: String,
    ) -> Result<String, PickError> {
        let text = text.replace("\r\n", "\n").trim().to_owned();
        if text.is_empty()
            || text.len() > ANSWER_TEXT_MAX_BYTES
            || acd_protocol::text::has_forbidden_chars(&text)
        {
            return Err(PickError::BadAnswerText);
        }
        let mut inner = self.inner.lock().unwrap();
        let (machine_key, handle, _) = find_card(&inner, card_key)?;
        let state = handle.state().borrow().clone();
        guard(&inner.settings, &state, card_key, origin.expect)?;
        if state.status != conn::MachineStatus::Online || !state.session.has("answer") {
            return Err(PickError::NotAnswerable);
        }
        let agent_id = card_key
            .split_once('/')
            .map(|(_, id)| id)
            .ok_or(PickError::UnknownCard)?;
        let agent = state
            .session
            .agents
            .get(agent_id)
            .ok_or(PickError::StaleRequest)?;
        let req = agent.pending.as_ref().ok_or(PickError::StaleRequest)?;
        if req.id != request_id || req.revision != revision {
            return Err(PickError::StaleRequest);
        }
        if !req.is_answerable() || !req.free_text || req.status != acd_protocol::RequestStatus::Open
        {
            return Err(PickError::NotAnswerable);
        }
        let now = Instant::now();
        let id = uuid::Uuid::new_v4().to_string();
        let detail: String = text.chars().take(80).collect();
        inner.actions.add(Action {
            id: id.clone(),
            owner: origin.owner,
            card_key: card_key.into(),
            machine_key,
            agent_id: agent.id.clone(),
            instance: state.instance,
            conn_gen: state.conn_gen,
            kind: ActionKind::AnswerText(TextSemantics::of(req, &text)),
            fires_at: now + self.opts.timing.undo,
            view: ActionView {
                id: id.clone(),
                card_key: card_key.into(),
                label: "Answer".into(),
                detail: Some(detail),
                fires_at_ms: now_ms() + self.opts.timing.undo.as_secs_f64() * 1000.,
            },
        })?;
        drop(inner);
        self.wake();
        Ok(id)
    }
    fn reply_with(self: &Arc<Self>, origin: Origin<'_>, card_key: &str, text: String) -> Result<String, PickError> {
        if text.trim().is_empty() || text.len() > 64 * 1024 {
            return Err(PickError::BadText);
        }
        let mut inner = self.inner.lock().unwrap();
        let (machine_key, handle, _) = find_card(&inner, card_key)?;
        let state = handle.state().borrow().clone();
        guard(&inner.settings, &state, card_key, origin.expect)?;
        let agent_id = match origin.expect {
            // Voice: live state only; no published card is needed (D7, B2).
            Some(g) => {
                let live = state.status == conn::MachineStatus::Online
                    && state.session.has("send")
                    && state.session.agents.get(&g.agent_id).is_some_and(|a| a.can_send);
                if !live {
                    return Err(PickError::CannotReply);
                }
                g.agent_id.clone()
            }
            // Keyboard: unchanged.
            None => {
                let current_view = self.view_tx.borrow().clone();
                let card = current_view
                    .cards
                    .iter()
                    .find(|c| c.key == card_key)
                    .ok_or(PickError::UnknownCard)?;
                if !card.can_reply {
                    return Err(PickError::CannotReply);
                }
                card.agent.id.clone()
            }
        };
        let id = uuid::Uuid::new_v4().to_string();
        let now = Instant::now();
        let detail: String = text.chars().take(80).collect();
        inner.actions.add(Action {
            id: id.clone(),
            owner: origin.owner,
            card_key: card_key.into(),
            machine_key,
            agent_id,
            instance: state.instance,
            conn_gen: state.conn_gen,
            kind: ActionKind::Reply { text },
            fires_at: now + self.opts.timing.undo,
            view: ActionView {
                id: id.clone(),
                card_key: card_key.into(),
                label: "Reply".into(),
                detail: Some(detail),
                fires_at_ms: now_ms() + self.opts.timing.undo.as_secs_f64() * 1000.,
            },
        })?;
        drop(inner);
        self.wake();
        Ok(id)
    }

    pub fn pick(self: &Arc<Self>, card_key: &str, request_id: &str, option_id: &str, revision: u64) -> Result<String, PickError> {
        self.pick_with(Origin::KEYS, card_key, request_id, option_id, revision)
    }
    /// `pick` for one owner (omarchy spec §6).
    pub fn pick_owned(self: &Arc<Self>, owner: ActionOwner, card_key: &str, request_id: &str, option_id: &str, revision: u64) -> Result<String, PickError> {
        self.pick_with(Origin::owned(owner), card_key, request_id, option_id, revision)
    }
    /// `pick` on the request `guard` names, refused unless connection,
    /// agent and request still match (voice spec §5.6).
    pub fn pick_guarded(self: &Arc<Self>, card_key: &str, option_id: &str, guard: &Guard) -> Result<String, PickError> {
        let Some((request_id, revision)) = guard.request.clone() else {
            return Err(PickError::StaleRequest);
        };
        self.pick_with(Origin { owner: ActionOwner::DEFAULT, expect: Some(guard) }, card_key, &request_id, option_id, revision)
    }
    /// A typed answer to a free-text request, with the same undo countdown as
    /// `pick`. The text is trimmed; blank, oversize (16 KiB) or control-char
    /// text is `BadAnswerText`.
    pub fn answer_text(self: &Arc<Self>, card_key: &str, request_id: &str, revision: u64, text: String) -> Result<String, PickError> {
        self.answer_text_with(Origin::KEYS, card_key, request_id, revision, text)
    }
    /// `answer_text` for one owner.
    pub fn answer_text_owned(self: &Arc<Self>, owner: ActionOwner, card_key: &str, request_id: &str, revision: u64, text: String) -> Result<String, PickError> {
        self.answer_text_with(Origin::owned(owner), card_key, request_id, revision, text)
    }
    pub fn answer_text_guarded(self: &Arc<Self>, card_key: &str, text: String, guard: &Guard) -> Result<String, PickError> {
        let Some((request_id, revision)) = guard.request.clone() else {
            return Err(PickError::StaleRequest);
        };
        self.answer_text_with(Origin { owner: ActionOwner::DEFAULT, expect: Some(guard) }, card_key, &request_id, revision, text)
    }
    pub fn reply(self: &Arc<Self>, card_key: &str, text: String) -> Result<String, PickError> {
        self.reply_with(Origin::KEYS, card_key, text)
    }
    /// `reply` for one owner.
    pub fn reply_owned(self: &Arc<Self>, owner: ActionOwner, card_key: &str, text: String) -> Result<String, PickError> {
        self.reply_with(Origin::owned(owner), card_key, text)
    }
    /// `reply`, refused unless connection, agent and request (including
    /// "no request") still match what the step was planned on.
    pub fn reply_guarded(self: &Arc<Self>, card_key: &str, text: String, guard: &Guard) -> Result<String, PickError> {
        self.reply_with(Origin { owner: ActionOwner::DEFAULT, expect: Some(guard) }, card_key, text)
    }
    pub fn cancel(&self, id: &str) -> bool {
        let removed = self.inner.lock().unwrap().actions.cancel(id);
        let Some(action) = removed else {
            return false;
        };
        self.emit(&action, ActionOutcome::Cancelled);
        self.wake();
        true
    }
    /// Cancels the newest unsent action of any owner (the Mac panel's Esc).
    pub fn escape(&self) -> bool {
        let removed = self.inner.lock().unwrap().actions.cancel_latest();
        let Some(action) = removed else {
            return false;
        };
        self.emit(&action, ActionOutcome::Cancelled);
        self.wake();
        true
    }
    /// Cancels `owner`'s newest unsent action (Esc in one view).
    pub fn escape_owned(&self, owner: ActionOwner) -> bool {
        let removed = self.inner.lock().unwrap().actions.cancel_latest_of(owner);
        let Some(action) = removed else {
            return false;
        };
        self.emit(&action, ActionOutcome::Cancelled);
        self.wake();
        true
    }
    /// Cancels every unsent action of `owner` (a view disconnected); each
    /// ends `Cancelled`. Returns how many.
    pub fn cancel_owned(&self, owner: ActionOwner) -> usize {
        let removed = self.inner.lock().unwrap().actions.cancel_owned(owner);
        for action in &removed {
            self.emit(action, ActionOutcome::Cancelled);
        }
        if !removed.is_empty() {
            self.wake();
        }
        removed.len()
    }
    pub fn mark_seen(self: &Arc<Self>, card_key: &str) -> Result<(), PickError> {
        let inner = self.inner.lock().unwrap();
        let (machine_key, h, _) = find_card(&inner, card_key)?;
        let current_view = self.view_tx.borrow().clone();
        let c = current_view
            .cards
            .iter()
            .find(|c| c.key == card_key)
            .ok_or(PickError::UnknownCard)?;
        if !c.can_mark_seen {
            return Err(PickError::CannotReply);
        }
        let agent = c.agent.id.clone();
        drop(inner);
        let ib = self.clone();
        tokio::spawn(async move {
            let o = h
                .submit(
                    Command::MarkSeen {
                        agent_id: agent.clone(),
                    },
                    None,
                )
                .await;
            if let Some(n) = view::notice_for_outcome(&o) {
                ib.set_notice(&format!("{machine_key}/{agent}"), n).await
            }
        });
        Ok(())
    }
    pub fn set_show_idle(&self, on: bool) {
        let mut inner = self.inner.lock().unwrap();
        inner.settings.show_idle = on;
        for a in drop_hidden_actions(&mut inner) {
            self.emit(&a, ActionOutcome::NotSent { reason: NotSentReason::Hidden });
        }
        drop(inner);
        self.wake();
    }
}
/// Cancels the countdown of a card the user just chose to hide (subagents,
/// idle or hook-only agents turned off), so nothing fires unseen and Esc
/// never cancels an action nobody can see. Not run on rebuild: an agent that
/// turns idle on its own keeps its card while a countdown runs. No notice:
/// the card is gone.
fn drop_hidden_actions(inner: &mut Inner) -> Vec<Action> {
    let (show_subagents, show_idle, show_hook_only) = (
        inner.settings.show_subagents,
        inner.settings.show_idle,
        inner.settings.show_hook_only,
    );
    let states: Vec<(String, MachineState)> = inner
        .machines
        .iter()
        .filter_map(|m| {
            m.handle
                .as_ref()
                .map(|h| (m.entry.key.clone(), h.state().borrow().clone()))
        })
        .collect();
    let mut dropped = Vec::new();
    for (key, state) in &states {
        dropped.extend(inner.actions.remove_where(|a| {
            a.machine_key == *key
                && state.session.agents.get(&a.agent_id).is_some_and(|agent| {
                    (!show_subagents && agent.role == acd_protocol::AgentRole::Subagent)
                        || (!show_idle && view::group(agent) == view::CardGroup::Idle)
                        || view::hook_only_hidden(agent, show_hook_only)
                })
        }));
    }
    dropped
}
fn notice(text: &str, tone: NoticeTone) -> Notice {
    Notice {
        text: text.into(),
        tone,
    }
}
/// Refuses an operation whose plan no longer matches the machine (voice
/// spec §5.6): connection, agent, and pending request or its absence.
/// Checked on the live machine state and the live settings, under the lock
/// that records the action, never on the published view.
fn guard(settings: &Settings, state: &MachineState, card_key: &str, expect: Option<&Guard>) -> Result<(), PickError> {
    let Some(g) = expect else {
        return Ok(());
    };
    if (state.instance, state.conn_gen) != (g.conn.instance, g.conn.conn_gen) {
        return Err(PickError::ConnectionChanged);
    }
    if card_key.split_once('/').map(|(_, a)| a) != Some(g.agent_id.as_str()) {
        return Err(PickError::UnknownCard);
    }
    let agent = state.session.agents.get(&g.agent_id).ok_or(PickError::StaleRequest)?;
    let live = agent.pending.as_ref().map(|r| (r.id.clone(), r.revision));
    if live != g.request {
        return Err(PickError::StaleRequest);
    }
    // The user may have hidden the agent since the step was planned. A
    // hidden subagent or hook-only agent takes nothing; a hidden idle agent
    // takes only a send that named it (D7).
    let hidden_role = !settings.show_subagents && agent.role == acd_protocol::AgentRole::Subagent;
    let hidden_idle = !settings.show_idle && view::group(agent) == view::CardGroup::Idle;
    if hidden_role || view::hook_only_hidden(agent, settings.show_hook_only) || (hidden_idle && !g.named_send_only) {
        return Err(PickError::Hidden);
    }
    Ok(())
}
fn find_card(i: &Inner, key: &str) -> Result<(String, MachineHandle, MachineEntry), PickError> {
    let (mk, aid) = key.split_once('/').ok_or(PickError::UnknownCard)?;
    let m = i
        .machines
        .iter()
        .find(|m| m.entry.key == mk)
        .ok_or(PickError::UnknownCard)?;
    let h = m.handle.clone().ok_or(PickError::UnknownCard)?;
    if !h.state().borrow().session.agents.contains_key(aid) {
        return Err(PickError::UnknownCard);
    }
    Ok((mk.into(), h, m.entry.clone()))
}
/// Where and how to connect to `entry`. Reads the SecretStore for a TCP
/// machine, so never call it while holding `inner`.
fn connect_info(entry: &MachineEntry, secrets: &Arc<dyn SecretStore>) -> Option<(Endpoint, conn::Credential)> {
    match &entry.endpoint {
        EndpointConfig::Local => {
            let dir = crate::local::default_config_dir()?;
            Some((Endpoint::Local { config_dir: dir.clone() }, conn::Credential::LocalFile(dir)))
        }
        EndpointConfig::Tcp { host, port } => {
            let raw = secrets.get(&entry.key).ok()??;
            let secret = MachineSecret::from_json(&raw).ok()?;
            Some(tcp_info(host, *port, &secret))
        }
    }
}

fn tcp_info(host: &str, port: u16, secret: &MachineSecret) -> (Endpoint, conn::Credential) {
    (
        Endpoint::Tcp { host: host.to_owned(), port, pin: secret.pin.clone() },
        conn::Credential::Token(secret.token.clone()),
    )
}

fn spawn_with(
    entry: &MachineEntry,
    (endpoint, credential): (Endpoint, conn::Credential),
    opts: &InboxOptions,
    instance: u64,
    client_id: &str,
) -> MachineHandle {
    conn::spawn_machine(conn::MachineConfig {
        key: entry.key.clone(),
        instance,
        endpoint,
        credential,
        client: acd_protocol::ClientInfo {
            id: client_id.to_owned(),
            name: opts.client_name.clone(),
            version: opts.client_version.clone(),
        },
        timing: opts.timing.clone(),
    })
}
