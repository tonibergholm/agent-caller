use crate::{
    Timing,
    actions::{Action, ActionKind, Actions, Semantics, check},
    commands::Command,
    conn::{self, MachineHandle, MachineState, now_ms},
    endpoint::Endpoint,
    settings::{EndpointConfig, MachineEntry, MachineSecret, SecretStore, Settings},
    view::{self, ActionView, InboxView, MachineInput, Notice, NoticeTone},
};
use acd_protocol::Effect;
use std::sync::Mutex;
use std::time::Instant;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{Notify, broadcast, watch};

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
    #[error("the request changed")]
    StaleRequest,
    #[error("unknown or unselectable option")]
    UnknownOption,
    #[error("a countdown is already running for this card")]
    AlreadyCounting,
    #[error("the reply is empty or longer than 64 KiB")]
    BadText,
    #[error("replies are not possible for this agent right now")]
    CannotReply,
}
struct Machine {
    entry: MachineEntry,
    handle: Option<MachineHandle>,
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
#[derive(Clone, Debug)]
pub enum InboxEvent {
    Updated,
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
    pub fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            {
                let mut inner = this.inner.lock().unwrap();
                let client_id = inner.settings.client_id.clone();
                for machine in &mut inner.machines {
                    let instance = this.next_instance.fetch_add(1, Ordering::Relaxed);
                    machine.handle = spawn_for(
                        &machine.entry,
                        &this.secrets,
                        &this.opts,
                        instance,
                        &client_id,
                    );
                }
            }
            let handles = {
                let inner = this.inner.lock().unwrap();
                inner
                    .machines
                    .iter()
                    .filter_map(|m| m.handle.clone().map(|h| (m.entry.key.clone(), h)))
                    .collect::<Vec<_>>()
            };
            for (key, handle) in handles {
                let ib = this.clone();
                tokio::spawn(async move {
                    let mut state = handle.state();
                    let mut events = handle.events();
                    loop {
                        tokio::select! {r=state.changed()=>{if r.is_err(){break} ib.wake.notify_one();}, e=events.recv()=>match e {Ok((instance,event))=>ib.on_machine_event(&key,instance,event).await,Err(broadcast::error::RecvError::Lagged(_))=>{let instance = { handle.state().borrow().instance }; ib.end_machine(&key, instance).await;},Err(_)=>break}}
                    }
                });
            }
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
        let _ = self.events_tx.send(InboxEvent::Updated);
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
                    option_id: s.option.id,
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
        if let conn::MachineEvent::ConnectionEnded { generation } = event {
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
        if req.id != request_id {
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
