//! Counters and latency histograms shared by clients and producers.
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use crate::hist::Hist;

#[derive(Default)]
pub struct Metrics {
    /// Request open at the source to the client seeing `request.opened`.
    pub open_visible: Mutex<Hist>,
    /// A client submitting an answer to it seeing `request.resolved`.
    pub answer_resolved: Mutex<Hist>,
    /// A client submitting an answer to the command result.
    pub command_rtt: Mutex<Hist>,
    pub opened_at_source: AtomicU64,
    pub opened_seen: AtomicU64,
    pub answers_sent: AtomicU64,
    pub answers_ok: AtomicU64,
    pub resolved_seen: AtomicU64,
    pub withdrawn: AtomicU64,
    pub drops_injected: AtomicU64,
    pub welcomed_same: AtomicU64,
    pub welcomed_new: AtomicU64,
    pub connection_ends: AtomicU64,
    pub lagged: AtomicU64,
    pub restarts: AtomicU64,
    pub producer_errors: AtomicU64,
    pub producer_puts: AtomicU64,
    pub errors: Mutex<BTreeMap<String, u64>>,
    pub resolved_by: Mutex<BTreeMap<String, u64>>,
}

impl Metrics {
    pub fn inc(c: &AtomicU64) {
        c.fetch_add(1, Relaxed);
    }
    pub fn get(c: &AtomicU64) -> u64 {
        c.load(Relaxed)
    }
    pub fn err(&self, what: impl Into<String>) {
        *self.errors.lock().unwrap().entry(what.into()).or_default() += 1;
    }
    pub fn by(&self, what: impl Into<String>) {
        *self.resolved_by.lock().unwrap().entry(what.into()).or_default() += 1;
    }
    pub fn record(h: &Mutex<Hist>, us: u64) {
        h.lock().unwrap().record(us);
    }
}

/// Windowed deltas: the sampler takes a window every interval and folds it
/// into the totals.
#[derive(Default)]
pub struct Windows {
    pub open_visible: Hist,
    pub answer_resolved: Hist,
    pub command_rtt: Hist,
    pub total_open_visible: Hist,
    pub total_answer_resolved: Hist,
    pub total_command_rtt: Hist,
}

impl Windows {
    /// Swaps the shared histograms out, returns this window's, and adds it
    /// to the run totals.
    pub fn take(&mut self, m: &Metrics) {
        self.open_visible = std::mem::take(&mut *m.open_visible.lock().unwrap());
        self.answer_resolved = std::mem::take(&mut *m.answer_resolved.lock().unwrap());
        self.command_rtt = std::mem::take(&mut *m.command_rtt.lock().unwrap());
        self.total_open_visible.merge(&self.open_visible);
        self.total_answer_resolved.merge(&self.answer_resolved);
        self.total_command_rtt.merge(&self.command_rtt);
    }
}
