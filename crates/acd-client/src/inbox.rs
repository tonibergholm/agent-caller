use crate::{
    Timing,
    actions::{Action, ActionKind, Actions, Semantics, TextSemantics, check},
    commands::Command,
    conn::{self, MachineHandle, MachineState, now_ms},
    endpoint::Endpoint,
    settings::{EndpointConfig, MachineEntry, MachineSecret, SecretStore, Settings},
    view::{
        self, ActionView, InboxView, MachineEntryView, MachineInput, Notice, NoticeTone,
        PairingState,
    },
};
use acd_protocol::{Effect, pairing::Pairing};
use std::sync::Mutex;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
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
    #[error("the answer is empty, too long, or has control characters")]
    BadAnswerText,
    #[error("replies are not possible for this agent right now")]
    CannotReply,
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
pub struct Inbox {
    inner: Mutex<Inner>,
    secrets: Arc<dyn SecretStore>,
    opts: InboxOptions,
    view_tx: watch::Sender<InboxView>,
    events_tx: broadcast::Sender<InboxEvent>,
    wake: Notify,
    next_instance: AtomicU64,
    started: std::sync::atomic::AtomicBool,
}
impl Inbox {
    pub fn new(settings: Settings, secrets: Arc<dyn SecretStore>, opts: InboxOptions) -> Arc<Self> {
        let (tx, _) = watch::channel(InboxView {
            machines: vec![],
            cards: vec![],
            hidden_idle: 0,
            show_idle: settings.show_idle,
            tray: view::TraySummary {
                color: view::TrayColor::Grey,
                needs_you: 0,
            },
            actions: vec![],
        });
        let (events_tx, _) = broadcast::channel(64);
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
            secrets,
            opts,
            view_tx: tx,
            events_tx,
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
    pub fn machines(&self) -> Vec<MachineEntryView> {
        let inner = self.inner.lock().unwrap();
        inner
            .settings
            .machines
            .iter()
            .map(|entry| {
                let pairing = match &entry.endpoint {
                    EndpointConfig::Local => PairingState::Paired,
                    EndpointConfig::Tcp { .. } => match self.secrets.get(&entry.key) {
                        Ok(None) => PairingState::NotPaired,
                        Err(_) => PairingState::Unreadable,
                        Ok(Some(raw)) => {
                            if MachineSecret::from_json(&raw).is_ok() {
                                PairingState::Paired
                            } else {
                                PairingState::Unreadable
                            }
                        }
                    },
                };
                MachineEntryView {
                    key: entry.key.clone(),
                    name: entry.name.clone(),
                    endpoint: entry.endpoint.clone(),
                    pairing,
                }
            })
            .collect()
    }
    pub fn import_pairing(self: &Arc<Self>, line: &str) -> Result<MachineEntry, String> {
        let pairing = Pairing::parse(line).map_err(|_| "That is not an acd pairing line. Run `acd pair` on the machine and copy its output.".to_owned())?;
        let mut inner = self.inner.lock().unwrap();
        let key_match = inner.settings.machines.iter().find(|m| matches!(&m.endpoint, EndpointConfig::Tcp { host, port } if host == &pairing.host && *port == pairing.port)).cloned();
        let entry = key_match.clone().unwrap_or_else(|| MachineEntry {
            key: uuid::Uuid::new_v4().to_string(),
            name: default_machine_name(&pairing.host),
            endpoint: EndpointConfig::Tcp {
                host: pairing.host.clone(),
                port: pairing.port,
            },
        });
        let secret_json = MachineSecret {
            token: pairing.token,
            pin: pairing.pin,
        }
        .to_json();
        let previous_secret = self
            .secrets
            .get(&entry.key)
            .map_err(|_| "Could not read machine credentials".to_owned())?;
        self.secrets
            .set(&entry.key, &secret_json)
            .map_err(|_| "Could not save machine credentials".to_owned())?;
        let mut candidate = inner.settings.clone();
        candidate.machines.retain(|m| m.key != entry.key);
        candidate.machines.push(entry.clone());
        if let Some(path) = &self.opts.settings_path
            && let Err(error) = candidate.save(path)
        {
            let recovery = match previous_secret {
                Some(value) => self.secrets.set(&entry.key, &value),
                None => self.secrets.delete(&entry.key),
            };
            if recovery.is_err() {
                if let Some(machine) = inner.machines.iter_mut().find(|m| m.entry.key == entry.key)
                    && let Some(handle) = machine.handle.take()
                {
                    handle.cancel();
                }
                eprintln!("machine pairing recovery failed");
                return Err("Machine pairing recovery failed".to_owned());
            }
            return Err(error.to_string());
        }
        inner.settings = candidate;
        if let Some(old) = inner.machines.iter().position(|m| m.entry.key == entry.key) {
            for action in inner.actions.end_machine(&entry.key) {
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
        let handle = spawn_for(&entry, &self.secrets, &self.opts, instance, &client_id);
        inner.machines.push(Machine {
            entry: entry.clone(),
            handle: handle.clone(),
        });
        drop(inner);
        if let Some(handle) = handle {
            self.watch_machine(entry.key.clone(), handle);
        }
        self.wake();
        Ok(entry)
    }
    pub fn rename_machine(&self, key: &str, name: &str) -> Result<(), String> {
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
        let mut inner = self.inner.lock().unwrap();
        let pos = inner
            .settings
            .machines
            .iter()
            .position(|m| m.key == key)
            .ok_or("Unknown machine")?;
        self.secrets
            .get(key)
            .map_err(|_| "Could not read machine credentials".to_owned())?;
        let old_settings = inner.settings.clone();
        let mut candidate = old_settings.clone();
        candidate.machines.remove(pos);
        if let Some(path) = &self.opts.settings_path {
            candidate.save(path).map_err(|e| e.to_string())?;
        }
        if let Err(error) = self.secrets.delete(key) {
            if let Some(path) = &self.opts.settings_path
                && old_settings.save(path).is_err()
            {
                if let Some(machine) = inner.machines.iter_mut().find(|m| m.entry.key == key)
                    && let Some(handle) = machine.handle.take()
                {
                    handle.cancel();
                }
                eprintln!("machine removal recovery failed");
                return Err("Machine removal recovery failed".to_owned());
            }
            return Err(error.to_string());
        }
        inner.settings = candidate;
        for machine in inner.machines.iter().filter(|m| m.entry.key == key) {
            if let Some(handle) = &machine.handle {
                handle.cancel();
            }
        }
        inner.machines.retain(|m| m.entry.key != key);
        for action in inner.actions.end_machine(key) {
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
    pub fn settings(&self) -> Settings {
        self.inner.lock().unwrap().settings.clone()
    }
    pub fn set_hotkey(&self, hotkey: &str) -> Result<(), String> {
        if hotkey.trim().is_empty() {
            return Err("Hotkey cannot be empty".into());
        }
        let mut inner = self.inner.lock().unwrap();
        let mut candidate = inner.settings.clone();
        candidate.hotkey = hotkey.to_owned();
        if let Some(path) = &self.opts.settings_path {
            candidate.save(path).map_err(|e| e.to_string())?;
        }
        inner.settings = candidate;
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
        let handles = {
            let mut inner = self.inner.lock().unwrap();
            if self.started.swap(true, Ordering::AcqRel) {
                return;
            }
            let client_id = inner.settings.client_id.clone();
            for machine in &mut inner.machines {
                if machine.handle.is_some() {
                    continue;
                }
                let instance = self.next_instance.fetch_add(1, Ordering::Relaxed);
                machine.handle = spawn_for(
                    &machine.entry,
                    &self.secrets,
                    &self.opts,
                    instance,
                    &client_id,
                );
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
            let invalid = inner.actions.remove_invalid(key, |a| check(a, state).err());
            for (card_key, n) in invalid {
                inner
                    .notices
                    .insert(card_key, (n, now + self.opts.timing.notice_ttl));
            }
        }
        let inputs = inner
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
        let v = view::build_view(&view::ViewInput {
            machines: inputs,
            actions: &action_views,
            busy: &busy,
            notices: &notices,
            show_idle: inner.settings.show_idle,
        });
        self.view_tx.send_replace(v);
    }
    async fn fire_due(self: &Arc<Self>) {
        let due = { self.inner.lock().unwrap().actions.due(Instant::now()) };
        for action in due {
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
                self.set_notice(
                    &action.card_key,
                    notice("Offline. Nothing was sent.", NoticeTone::Warning),
                )
                .await;
                continue;
            };
            if let Err(n) = check(&action, &state) {
                self.set_notice(&action.card_key, n).await;
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
                let out = match check(&validation_action, &latest) {
                    Ok(()) => Some(handle.submit(cmd, Some(action.conn_gen)).await),
                    Err(n) => {
                        let mut inner = ib.inner.lock().unwrap();
                        inner.notices.insert(
                            validation_action.card_key.clone(),
                            (n, Instant::now() + ib.opts.timing.notice_ttl),
                        );
                        None
                    }
                };
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
                let agent_title = state
                    .session
                    .agents
                    .get(&req.agent_id)
                    .map(|a| a.title.clone())
                    .unwrap_or_else(|| req.agent_id.clone());
                drop(state);
                if inner.notified.insert((machine_id, req.id.clone())) {
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
    pub fn pick(
        self: &Arc<Self>,
        card_key: &str,
        request_id: &str,
        option_id: &str,
        revision: u64,
    ) -> Result<String, PickError> {
        let mut inner = self.inner.lock().unwrap();
        let (machine_key, handle, entry) = find_card(&inner, card_key)?;
        let state = handle.state().borrow().clone();
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
    /// A typed answer to a free-text request, with the same undo countdown as
    /// `pick`. The text is trimmed; blank, oversize (16 KiB) or control-char
    /// text is `BadAnswerText`.
    pub fn answer_text(
        self: &Arc<Self>,
        card_key: &str,
        request_id: &str,
        revision: u64,
        text: String,
    ) -> Result<String, PickError> {
        let text = text.trim().to_owned();
        if text.is_empty()
            || text.len() > ANSWER_TEXT_MAX_BYTES
            || text
                .chars()
                .any(|c| c.is_control() && c != '\n' && c != '\t')
        {
            return Err(PickError::BadAnswerText);
        }
        let mut inner = self.inner.lock().unwrap();
        let (machine_key, handle, _) = find_card(&inner, card_key)?;
        let state = handle.state().borrow().clone();
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
    pub fn reply(self: &Arc<Self>, card_key: &str, text: String) -> Result<String, PickError> {
        if text.trim().is_empty() || text.len() > 64 * 1024 {
            return Err(PickError::BadText);
        }
        let mut inner = self.inner.lock().unwrap();
        let (machine_key, handle, _) = find_card(&inner, card_key)?;
        let state = handle.state().borrow().clone();
        let current_view = self.view_tx.borrow().clone();
        let card = current_view
            .cards
            .iter()
            .find(|c| c.key == card_key)
            .ok_or(PickError::UnknownCard)?;
        if !card.can_reply {
            return Err(PickError::CannotReply);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now = Instant::now();
        let detail: String = text.chars().take(80).collect();
        inner.actions.add(Action {
            id: id.clone(),
            card_key: card_key.into(),
            machine_key,
            agent_id: card.agent.id.clone(),
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
    pub fn cancel(&self, id: &str) -> bool {
        let removed = self.inner.lock().unwrap().actions.cancel(id).is_some();
        if removed {
            self.wake()
        }
        removed
    }
    pub fn escape(&self) -> bool {
        let removed = self.inner.lock().unwrap().actions.cancel_latest().is_some();
        if removed {
            self.wake()
        }
        removed
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
        drop(inner);
        self.wake();
    }
}
fn notice(text: &str, tone: NoticeTone) -> Notice {
    Notice {
        text: text.into(),
        tone,
    }
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
fn spawn_for(
    entry: &MachineEntry,
    secrets: &Arc<dyn SecretStore>,
    opts: &InboxOptions,
    instance: u64,
    client_id: &str,
) -> Option<MachineHandle> {
    let (endpoint, credential) = match &entry.endpoint {
        EndpointConfig::Local => {
            let dir = crate::local::default_config_dir()?;
            (
                Endpoint::Local {
                    config_dir: dir.clone(),
                },
                conn::Credential::LocalFile(dir),
            )
        }
        EndpointConfig::Tcp { host, port } => {
            let raw = secrets.get(&entry.key).ok()??;
            let secret = MachineSecret::from_json(&raw).ok()?;
            (
                Endpoint::Tcp {
                    host: host.clone(),
                    port: *port,
                    pin: secret.pin,
                },
                conn::Credential::Token(secret.token),
            )
        }
    };
    Some(conn::spawn_machine(conn::MachineConfig {
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
    }))
}
