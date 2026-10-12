//! Notifications (omarchy spec §8, plan D15 to D17). Views are reconciled into
//! show and close commands; the D-Bus side sits behind `NotifyBackend`.
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acd_client::view::{CardView, InboxView};
use acd_protocol::{RequestKind, RequestStatus};
use tokio::sync::{mpsc, watch};

use crate::prefs::Prefs;

pub const OPEN_ACTION: &str = "open";
pub const PLUGIN_ID: &str = "tonibergholm.agentcaller";
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(5);
const TITLE_MAX: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NoteKey {
    pub card: String,
    pub request: String,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NoteCmd {
    Show { key: NoteKey, summary: String, body: String },
    Close { key: NoteKey },
}

/// Which requests were considered and which have a notification on screen.
#[derive(Default)]
pub struct Reconciler {
    seen: HashSet<NoteKey>,
    shown: HashSet<NoteKey>,
}

impl Reconciler {
    /// A request is considered once, when it first appears (plan D16).
    pub fn reconcile(&mut self, view: &InboxView, enabled: bool, panel_open: bool) -> Vec<NoteCmd> {
        let current: HashMap<NoteKey, &CardView> = view
            .cards
            .iter()
            .filter_map(|c| {
                let p = c.agent.pending.as_ref()?;
                (p.status == RequestStatus::Open)
                    .then(|| (NoteKey { card: c.key.clone(), request: p.id.clone(), revision: p.revision }, c))
            })
            .collect();
        let mut out = Vec::new();
        let gone: Vec<NoteKey> = self.shown.iter().filter(|k| !current.contains_key(*k)).cloned().collect();
        for key in gone {
            self.shown.remove(&key);
            out.push(NoteCmd::Close { key });
        }
        self.seen.retain(|k| current.contains_key(k));
        let mut fresh: Vec<(&NoteKey, &&CardView)> = current.iter().filter(|(k, _)| !self.seen.contains(*k)).collect();
        fresh.sort_by(|a, b| a.0.card.cmp(&b.0.card));
        for (key, card) in fresh {
            self.seen.insert(key.clone());
            if !enabled || panel_open || card.action.is_some() {
                continue;
            }
            if let Some((summary, body)) = text(card) {
                self.shown.insert(key.clone());
                out.push(NoteCmd::Show { key: key.clone(), summary, body });
            }
        }
        out
    }
}

/// Agent text made safe to show: whitespace, control, bidi and separator
/// characters become single spaces, the ends are trimmed, and it is cut to
/// `max` characters ending in "…".
pub fn clean(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in s.chars() {
        if c.is_whitespace() || acd_protocol::text::is_forbidden_control(c) || acd_protocol::text::is_forbidden_format(c) {
            gap = !out.is_empty();
            continue;
        }
        if gap {
            out.push(' ');
            gap = false;
        }
        out.push(c);
    }
    if out.chars().count() <= max {
        return out;
    }
    let mut cut: String = out.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The panel's type tag in lower case (`panel.ts` `typeTag`).
pub fn request_label(kind: RequestKind, title: &str) -> String {
    let tool = {
        let mut chars = title.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && title.len() <= 40
            && chars.all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
    };
    match kind {
        RequestKind::Permission if tool => format!("{} approval", title.to_lowercase()),
        RequestKind::Permission => "approval".into(),
        RequestKind::Question => "question".into(),
        RequestKind::BlockedUnparsed => "blocked".into(),
        RequestKind::Unknown => "request".into(),
    }
}

/// Summary and body (plan D15). Never the request detail or screen.
pub fn text(card: &CardView) -> Option<(String, String)> {
    let p = card.agent.pending.as_ref()?;
    Some((
        format!("{} on {} needs you", clean(&card.agent.title, TITLE_MAX), clean(&card.machine_name, TITLE_MAX)),
        request_label(p.kind, &p.title),
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyError {
    Unavailable,
    Failed,
}

#[async_trait::async_trait]
pub trait NotifyBackend: Send + Sync + 'static {
    async fn show(&self, summary: &str, body: &str, open_action: bool) -> Result<u32, NotifyError>;
    async fn close(&self, id: u32);
    /// The unique name that owns `org.freedesktop.Notifications` now.
    async fn owner(&self) -> Option<String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionSignal {
    pub id: u32,
    pub action: String,
    pub sender: String,
}

pub trait Opener: Send + Sync + 'static {
    fn open(&self, card: &str);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenResult {
    Exited,
    TimedOut,
    Failed,
}

/// Runs `omarchy-shell tonibergholm.agentcaller openCard <key>` (§8).
pub struct ShellOpener {
    pub shell: PathBuf,
    pub timeout: Duration,
}

impl ShellOpener {
    /// `omarchy-shell` on `path_var`, else in `fallback` (`/usr/bin`, §4.2).
    pub fn find(path_var: Option<&OsStr>, fallback: &Path) -> Option<ShellOpener> {
        let on_path = path_var.and_then(|p| std::env::split_paths(p).map(|d| d.join("omarchy-shell")).find(|c| c.is_file()));
        let shell = on_path.or_else(|| Some(fallback.join("omarchy-shell")).filter(|c| c.is_file()))?;
        Some(ShellOpener { shell, timeout: OPEN_TIMEOUT })
    }

    /// An argv, no shell; output discarded; killed if dropped.
    pub fn command(&self, card: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&self.shell);
        cmd.arg(PLUGIN_ID)
            .arg("openCard")
            .arg(card)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        cmd
    }

    pub async fn run(&self, card: &str) -> OpenResult {
        let Ok(mut child) = self.command(card).spawn() else {
            tracing::warn!("open action could not start omarchy-shell");
            return OpenResult::Failed;
        };
        match tokio::time::timeout(self.timeout, child.wait()).await {
            Ok(_) => OpenResult::Exited,
            Err(_) => {
                let _ = child.kill().await;
                tracing::warn!("open action timed out");
                OpenResult::TimedOut
            }
        }
    }
}

impl Opener for ShellOpener {
    fn open(&self, card: &str) {
        let me = ShellOpener { shell: self.shell.clone(), timeout: self.timeout };
        let card = card.to_owned();
        tokio::spawn(async move {
            me.run(&card).await;
        });
    }
}

struct State {
    reconciler: Reconciler,
    ids: HashMap<NoteKey, u32>,
    /// Notification id → card and the server owner that showed it.
    by_id: HashMap<u32, (String, Option<String>)>,
    available: bool,
}

pub struct Notifier {
    backend: Arc<dyn NotifyBackend>,
    opener: Option<Arc<dyn Opener>>,
    state: Mutex<State>,
}

impl Notifier {
    pub fn new(backend: Arc<dyn NotifyBackend>, opener: Option<Arc<dyn Opener>>) -> Arc<Notifier> {
        Arc::new(Notifier {
            backend,
            opener,
            state: Mutex::new(State {
                reconciler: Reconciler::default(),
                ids: HashMap::new(),
                by_id: HashMap::new(),
                available: true,
            }),
        })
    }

    /// One reconcile pass; shows and closes notifications.
    pub async fn apply(&self, view: &InboxView, enabled: bool, panel_open: bool) {
        let cmds = {
            let mut s = self.state.lock().unwrap();
            let on = enabled && s.available;
            s.reconciler.reconcile(view, on, panel_open)
        };
        let owner = if cmds.iter().any(|c| matches!(c, NoteCmd::Show { .. })) { self.backend.owner().await } else { None };
        for cmd in cmds {
            match cmd {
                NoteCmd::Show { key, summary, body } => {
                    match self.backend.show(&summary, &body, self.opener.is_some()).await {
                        Ok(id) => {
                            // A daemon started by this very call had no owner
                            // before it; record the one that answered. A known
                            // owner stays as read before `show`, so a daemon
                            // replaced since then is not trusted.
                            let recorded = match &owner {
                                Some(_) => owner.clone(),
                                None => self.backend.owner().await,
                            };
                            let mut s = self.state.lock().unwrap();
                            s.by_id.insert(id, (key.card.clone(), recorded));
                            s.ids.insert(key, id);
                        }
                        Err(e) => {
                            // D28: off for the session; the rest of this batch is dropped.
                            tracing::warn!(error = ?e, "notification failed; notifications are off for this session");
                            self.state.lock().unwrap().available = false;
                            return;
                        }
                    }
                }
                NoteCmd::Close { key } => {
                    let id = {
                        let mut s = self.state.lock().unwrap();
                        let id = s.ids.remove(&key);
                        if let Some(id) = id {
                            s.by_id.remove(&id);
                        }
                        id
                    };
                    if let Some(id) = id {
                        self.backend.close(id).await;
                    }
                }
            }
        }
    }

    /// Opens the card only for an id this notifier created, sent by the
    /// current owner of the notification name, unchanged since then.
    pub async fn on_action(&self, signal: &ActionSignal) -> bool {
        if signal.action != OPEN_ACTION {
            return false;
        }
        let Some(opener) = &self.opener else { return false };
        let created = self.state.lock().unwrap().by_id.get(&signal.id).cloned();
        let Some((card, owner_then)) = created else { return false };
        let now = self.backend.owner().await;
        if now.is_none() || now != owner_then || now.as_deref() != Some(signal.sender.as_str()) {
            tracing::warn!("notification action from an unexpected sender ignored");
            return false;
        }
        opener.open(&card);
        true
    }

    /// Runs until the view channel closes.
    pub async fn run(
        self: Arc<Self>,
        mut views: watch::Receiver<InboxView>,
        mut prefs: watch::Receiver<Prefs>,
        mut panel: watch::Receiver<bool>,
        mut actions: mpsc::Receiver<ActionSignal>,
    ) {
        loop {
            let view = views.borrow_and_update().clone();
            let enabled = prefs.borrow_and_update().notifications;
            let open = *panel.borrow_and_update();
            self.apply(&view, enabled, open).await;
            tokio::select! {
                r = views.changed() => if r.is_err() { return },
                r = prefs.changed() => if r.is_err() { return },
                r = panel.changed() => if r.is_err() { return },
                Some(signal) = actions.recv() => { self.on_action(&signal).await; }
            }
        }
    }
}
