//! M subscribed clients built on the `acd-client` library. Each sees every
//! `request.opened`, one designated client answers after a random delay
//! (a few others race it), and a churn task drops connections at random so
//! clients resume.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use acd_client::commands::{Command, Outcome};
use acd_client::conn::{Credential, MachineConfig, MachineEvent, MachineHandle, MachineStatus, spawn_machine};
use acd_client::endpoint::Endpoint;
use acd_client::session::EpochChange;
use acd_protocol::{ClientInfo, ResolvedBy};
use tokio::sync::{broadcast, watch};

use crate::metrics::Metrics;
use crate::rng::{Rng, now_us, seed_from_clock};

#[derive(Clone)]
pub struct ClientCfg {
    pub dir: PathBuf,
    pub count: usize,
    pub answer_min_s: f64,
    pub answer_max_s: f64,
    /// Chance that the designated client answers at all (the rest are
    /// left for the source to withdraw, as if answered at the terminal).
    pub p_answer: f64,
    /// Chance that a non-designated client also answers (a race).
    pub p_dup: f64,
    pub churn_every_s: f64,
    pub churn_frac: f64,
}

/// `t0=<unix microseconds>` at the start of a request's detail.
pub fn parse_t0(detail: &str) -> Option<u64> {
    detail.strip_prefix("t0=")?.split([';', ' ']).next()?.parse().ok()
}

fn fnv(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

pub fn spawn_clients(cfg: &ClientCfg, m: Arc<Metrics>, stop: watch::Receiver<bool>) -> Vec<MachineHandle> {
    let mut timing = acd_client::Timing::default();
    timing.backoff_max = Duration::from_secs(3);
    let mut handles = Vec::new();
    for idx in 0..cfg.count {
        let h = spawn_machine(MachineConfig {
            key: "lt".into(),
            instance: idx as u64 + 1,
            endpoint: Endpoint::Local { config_dir: cfg.dir.clone() },
            credential: Credential::LocalFile(cfg.dir.clone()),
            client: ClientInfo { id: format!("lt-{idx}"), name: "loadtest".into(), version: "0".into() },
            timing: timing.clone(),
        });
        let events = h.events();
        tokio::spawn(client_loop(idx, cfg.clone(), h.clone(), events, m.clone(), stop.clone()));
        handles.push(h);
    }
    if cfg.churn_every_s > 0.0 && cfg.churn_frac > 0.0 {
        tokio::spawn(churn_loop(cfg.clone(), handles.clone(), m, stop));
    }
    handles
}

async fn churn_loop(cfg: ClientCfg, handles: Vec<MachineHandle>, m: Arc<Metrics>, mut stop: watch::Receiver<bool>) {
    let mut rng = Rng::new(seed_from_clock());
    loop {
        let wait = rng.between(0.5, 1.5) * cfg.churn_every_s;
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs_f64(wait)) => {}
            _ = stop.changed() => return,
        }
        for h in &handles {
            if rng.chance(cfg.churn_frac) {
                h.drop_connection();
                Metrics::inc(&m.drops_injected);
            }
        }
    }
}

async fn client_loop(
    idx: usize,
    cfg: ClientCfg,
    h: MachineHandle,
    mut events: broadcast::Receiver<(u64, MachineEvent)>,
    m: Arc<Metrics>,
    mut stop: watch::Receiver<bool>,
) {
    let pending: Arc<Mutex<HashMap<String, Instant>>> = Arc::default();
    let mut rng = Rng::new(seed_from_clock() ^ (idx as u64 + 1) * 0x9E37);
    let mut last_prune = Instant::now();
    loop {
        let ev = tokio::select! {
            ev = events.recv() => ev,
            _ = stop.changed() => return,
        };
        let ev = match ev {
            Ok((_, ev)) => ev,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                m.lagged.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        match ev {
            MachineEvent::Opened(req) => {
                Metrics::inc(&m.opened_seen);
                if let Some(t0) = parse_t0(&req.detail) {
                    Metrics::record(&m.open_visible, now_us().saturating_sub(t0));
                }
                let designated = (fnv(&req.id) % cfg.count as u64) as usize == idx;
                let go = if designated { rng.chance(cfg.p_answer) } else { rng.chance(cfg.p_dup) };
                if !go {
                    continue;
                }
                let Some(opt) = req.options.first().map(|o| o.id.clone()) else { continue };
                let delay = Duration::from_secs_f64(rng.between(cfg.answer_min_s, cfg.answer_max_s));
                let (h, m, pending) = (h.clone(), m.clone(), pending.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    let t = Instant::now();
                    pending.lock().unwrap().insert(req.id.clone(), t);
                    Metrics::inc(&m.answers_sent);
                    let out = h
                        .submit(Command::Answer { request_id: req.id.clone(), revision: req.revision, option_id: opt }, None)
                        .await;
                    Metrics::record(&m.command_rtt, t.elapsed().as_micros() as u64);
                    match out {
                        Outcome::Delivered | Outcome::Done => Metrics::inc(&m.answers_ok),
                        Outcome::Failed { code, .. } => m.err(format!("answer:{code:?}")),
                        Outcome::Uncertain => m.err("answer:uncertain"),
                        Outcome::NotSent(why) => m.err(format!("answer:not_sent:{why:?}")),
                    }
                });
            }
            MachineEvent::Resolved { request_id, by, .. } => {
                Metrics::inc(&m.resolved_seen);
                if let Some(t) = pending.lock().unwrap().remove(&request_id) {
                    if by == ResolvedBy::Inbox {
                        Metrics::record(&m.answer_resolved, t.elapsed().as_micros() as u64);
                    }
                    m.by(format!("{by:?}"));
                }
            }
            MachineEvent::Welcomed { change, .. } => match change {
                EpochChange::Same => Metrics::inc(&m.welcomed_same),
                EpochChange::New => Metrics::inc(&m.welcomed_new),
                EpochChange::First => {}
            },
            MachineEvent::ConnectionEnded { .. } => Metrics::inc(&m.connection_ends),
            MachineEvent::Ready { .. } => {}
        }
        if last_prune.elapsed() > Duration::from_secs(30) {
            last_prune = Instant::now();
            pending.lock().unwrap().retain(|_, t| t.elapsed() < Duration::from_secs(120));
        }
    }
}

pub fn online(handles: &[MachineHandle]) -> u64 {
    handles.iter().filter(|h| matches!(h.state().borrow().status, MachineStatus::Online)).count() as u64
}
