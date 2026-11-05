//! Fake agents for the real `acd`: external producers on `hook.sock`
//! (docs/superpowers/specs/2026-10-13-external-source-design.md). Each
//! slot is one agent that opens a request, waits for a client's answer,
//! acks it and opens the next one under a fresh key. At most 8 producers
//! of 32 slots (the daemon's own limits), so 256 concurrent agents.
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::{Notify, watch};

use crate::http;
use crate::metrics::Metrics;
use crate::rng::{Rng, now_us, seed_from_clock};

#[derive(Clone)]
pub struct ProducerCfg {
    pub sock: PathBuf,
    pub token: String,
    pub producers: usize,
    pub slots: usize,
    /// Total request opens per second across all producers.
    pub rate: f64,
    /// A request nobody answered in this long is withdrawn (answered at
    /// the terminal, from the source's point of view).
    pub withdraw_after_s: f64,
    /// Chance per open that the slot becomes a new agent (agent churn).
    pub turnover: f64,
    /// Chance that an ack is `rejected` or `failed` instead of `applied`.
    pub p_odd_ack: f64,
    /// Keep each agent listed between requests (a read-only placeholder item), so
    /// agents live for the whole run and only their request keys change.
    pub sticky: bool,
    pub run_id: String,
    /// Bumped by the orchestrator: every producer opens all free slots.
    pub burst: Arc<AtomicU64>,
}

struct Item {
    key: String,
    t0_us: u64,
    opened: Instant,
    answered: bool,
    /// The daemon went away since the item was sent: re-key it.
    lost: bool,
}

struct Slot {
    incarnation: u32,
    item: Option<Item>,
    /// Key of the placeholder shown while the agent has no request (sticky mode).
    ph_key: Option<String>,
    started: bool,
}

struct Ack {
    answer_id: String,
    outcome: &'static str,
}

struct Shared {
    slots: Vec<Slot>,
    acks: Vec<Ack>,
    seen_answers: HashSet<String>,
    seen_order: VecDeque<String>,
    counter: u64,
}

pub fn spawn_producers(cfg: &ProducerCfg, m: Arc<Metrics>, stop: watch::Receiver<bool>) {
    let n = cfg.producers.min(8);
    for p in 0..n {
        let shared = Arc::new(Mutex::new(Shared {
            slots: (0..cfg.slots.min(32)).map(|_| Slot { incarnation: 0, item: None, ph_key: None, started: false }).collect(),
            acks: Vec::new(),
            seen_answers: HashSet::new(),
            seen_order: VecDeque::new(),
            counter: 0,
        }));
        let wake = Arc::new(Notify::new());
        let pid = format!("lt{p}");
        tokio::spawn(main_loop(cfg.clone(), pid.clone(), n, shared.clone(), wake.clone(), m.clone(), stop.clone()));
        tokio::spawn(poll_loop(cfg.clone(), pid, shared, wake, stop.clone()));
    }
}

fn take_answers(sh: &mut Shared, v: &Value, rng: &mut Rng, p_odd: f64) -> bool {
    let mut any = false;
    for a in v["answers"].as_array().into_iter().flatten() {
        let (Some(id), Some(key)) = (a["answer_id"].as_str(), a["key"].as_str()) else { continue };
        if !sh.seen_answers.insert(id.to_owned()) {
            continue;
        }
        sh.seen_order.push_back(id.to_owned());
        if sh.seen_order.len() > 4096
            && let Some(old) = sh.seen_order.pop_front()
        {
            sh.seen_answers.remove(&old);
        }
        let outcome = if rng.chance(p_odd) {
            if rng.chance(0.5) { "rejected" } else { "failed" }
        } else {
            "applied"
        };
        sh.acks.push(Ack { answer_id: id.to_owned(), outcome });
        for s in &mut sh.slots {
            if let Some(it) = &mut s.item
                && it.key == key
            {
                it.answered = true;
            }
        }
        any = true;
    }
    any
}

async fn poll_loop(
    cfg: ProducerCfg,
    pid: String,
    sh: Arc<Mutex<Shared>>,
    wake: Arc<Notify>,
    mut stop: watch::Receiver<bool>,
) {
    let mut rng = Rng::new(seed_from_clock() ^ 0xA5);
    let path = format!("/external/v1/producers/{pid}/answers?wait_ms=20000");
    loop {
        if *stop.borrow() {
            return;
        }
        let r = tokio::select! {
            r = http::request(&cfg.sock, "GET", &path, &cfg.token, "", Duration::from_secs(30)) => r,
            _ = stop.changed() => return,
        };
        match r {
            Ok((200, body)) => {
                if let Ok(v) = serde_json::from_str::<Value>(&body)
                    && take_answers(&mut sh.lock().unwrap(), &v, &mut rng, cfg.p_odd_ack)
                {
                    wake.notify_one();
                }
                // An empty reply is a returned long-poll; a producer with no
                // lease answers at once, so do not spin.
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
}

async fn main_loop(
    cfg: ProducerCfg,
    pid: String,
    nprod: usize,
    sh: Arc<Mutex<Shared>>,
    wake: Arc<Notify>,
    m: Arc<Metrics>,
    mut stop: watch::Receiver<bool>,
) {
    let mut rng = Rng::new(seed_from_clock() ^ pid.len() as u64 ^ now_us());
    let mean_gap = nprod as f64 / cfg.rate.max(0.001);
    let mut next_open = Instant::now() + Duration::from_secs_f64(rng.exp(mean_gap));
    let mut last_burst = cfg.burst.load(Relaxed);
    let path = format!("/external/v1/producers/{pid}/state");
    let mut put_failed;
    loop {
        let mut burst_now = false;
        let b = cfg.burst.load(Relaxed);
        if b != last_burst {
            last_burst = b;
            burst_now = true;
        }
        let (body, n_acks) = {
            let mut s = sh.lock().unwrap();
            let now = Instant::now();
            // Withdraw requests nobody answered.
            for slot in &mut s.slots {
                if slot.item.as_ref().is_some_and(|i| !i.answered && i.opened.elapsed().as_secs_f64() > cfg.withdraw_after_s) {
                    slot.item = None;
                    Metrics::inc(&m.withdrawn);
                }
            }
            // A finished (answered) item leaves the list with this call's ack.
            let mut opens = 0;
            if burst_now {
                opens = s.slots.len();
            } else {
                while now >= next_open {
                    opens += 1;
                    next_open += Duration::from_secs_f64(rng.exp(mean_gap));
                }
                if next_open < now {
                    next_open = now;
                }
            }
            for _ in 0..opens {
                let free: Vec<usize> = s.slots.iter().enumerate().filter(|(_, x)| x.item.is_none()).map(|(i, _)| i).collect();
                if free.is_empty() {
                    break;
                }
                let i = free[rng.below(free.len())];
                s.counter += 1;
                let key = format!("{}-{}-{}", cfg.run_id, pid, s.counter);
                if rng.chance(cfg.turnover) {
                    s.slots[i].incarnation += 1;
                }
                s.slots[i].started = true;
                s.slots[i].ph_key = None;
                s.slots[i].item = Some(Item { key, t0_us: 0, opened: Instant::now(), answered: false, lost: false });
                Metrics::inc(&m.opened_at_source);
            }
            let t0 = now_us();
            let mut items = Vec::new();
            let mut keys = Vec::new();
            let counter = s.counter;
            for (i, slot) in s.slots.iter_mut().enumerate() {
                let incarnation = slot.incarnation;
                let placeholder = cfg.sticky && slot.started;
                if placeholder && slot.item.as_ref().is_none_or(|it| it.answered) && slot.ph_key.is_none() {
                    slot.ph_key = Some(format!("{}-{}-ph{}", cfg.run_id, pid, now_us()));
                }
                let Some(it) = &mut slot.item else {
                    if let (true, Some(k)) = (placeholder, &slot.ph_key) {
                        items.push(json!({"key": k, "agent_key": format!("a{i}-{incarnation}"), "agent": {"title": format!("agent {i}"), "project": "loadtest"}, "title": "Idle"}));
                    }
                    continue;
                };
                if it.answered {
                    if let (true, Some(k)) = (placeholder, &slot.ph_key) {
                        items.push(json!({"key": k, "agent_key": format!("a{i}-{incarnation}"), "agent": {"title": format!("agent {i}"), "project": "loadtest"}, "title": "Idle"}));
                    }
                    continue;
                }
                if it.lost {
                    it.key = format!("{}-{}-r{}", cfg.run_id, pid, counter + i as u64 + now_us());
                    it.lost = false;
                    it.t0_us = 0;
                }
                if it.t0_us == 0 {
                    it.t0_us = t0;
                    it.opened = Instant::now();
                }
                keys.push(it.key.clone());
                items.push(json!({
                    "key": it.key,
                    "agent_key": format!("a{i}-{incarnation}"),
                    "agent": {"title": format!("agent {i}"), "project": "loadtest"},
                    "title": "Run command?",
                    "detail": format!("t0={};k={}", it.t0_us, it.key),
                    "options": [
                        {"id": "approve", "label": "Approve"},
                        {"id": "deny", "label": "Deny"},
                    ],
                }));
            }
            let acks: Vec<Value> = s
                .acks
                .iter()
                .map(|a| json!({"answer_id": a.answer_id, "outcome": a.outcome, "message": if a.outcome == "applied" { Value::Null } else { json!("loadtest") }}))
                .collect();
            let _ = keys;
            (json!({"lease_ms": 30000, "items": items, "acks": acks}).to_string(), s.acks.len())
        };
        let r = http::request(&cfg.sock, "PUT", &path, &cfg.token, &body, Duration::from_secs(15)).await;
        match r {
            Ok((200, text)) => {
                Metrics::inc(&m.producer_puts);
                put_failed = false;
                let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                let mut s = sh.lock().unwrap();
                let n = n_acks.min(s.acks.len());
                s.acks.drain(..n);
                // Items that are done (answered) are dropped once acked.
                for slot in &mut s.slots {
                    if slot.item.as_ref().is_some_and(|i| i.answered) {
                        slot.item = None;
                    }
                }
                for r in v["refused"].as_array().into_iter().flatten() {
                    if let Some(k) = r["key"].as_str() {
                        for slot in &mut s.slots {
                            if slot.item.as_ref().is_some_and(|i| i.key == k) {
                                slot.item = None;
                            }
                        }
                    }
                }
                take_answers(&mut s, &v, &mut rng, cfg.p_odd_ack);
            }
            Ok((code, _)) => {
                m.err(format!("put:{code}"));
                Metrics::inc(&m.producer_errors);
                put_failed = true;
            }
            Err(e) => {
                m.err(format!("put:{}", e.split(':').next().unwrap_or("err")));
                Metrics::inc(&m.producer_errors);
                put_failed = true;
            }
        }
        if put_failed {
            // The daemon may be gone: every open item must be re-sent under a new key.
            for slot in &mut sh.lock().unwrap().slots {
                if let Some(it) = &mut slot.item {
                    it.lost = true;
                }
            }
        }
        let wait = if put_failed {
            Duration::from_millis(500)
        } else {
            let to_open = next_open.saturating_duration_since(Instant::now());
            to_open.min(Duration::from_secs(5)).max(Duration::from_millis(5))
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = wake.notified() => {}
            _ = stop.changed() => { return; }
        }
    }
}
