//! What acd keeps from one thread's projection (gate 3 §3, §7, §8).
use std::collections::{HashMap, HashSet, VecDeque};

use serde::Deserialize;
use serde_json::Value;

use super::wire::{NativeRef, ProviderThread, RuntimeRequest, ThreadEvent, TurnItem, turn_item};

#[derive(Clone, Debug, Default)]
pub struct ThreadDetail {
    pub active_provider_thread: Option<String>,
    /// Provider thread id -> native id (null until the first turn).
    natives: HashMap<String, Option<String>>,
    pub requests: HashMap<String, RuntimeRequest>,
    /// Only approval, user-input and command-execution items, by item id.
    items: HashMap<String, TurnItem>,
    /// Command-execution item ids, oldest first.
    command_order: VecDeque<String>,
    /// Provider thread ids, oldest first.
    provider_order: VecDeque<String>,
}

pub const MAX_COMMAND_ITEMS: usize = 64;

#[derive(Clone, Debug)]
pub struct PendingView {
    pub request: RuntimeRequest,
    pub item: Option<TurnItem>,
    /// `input` of the `command_execution` item that shares the approval's
    /// `nativeItemRef.nativeId` (D7).
    pub command: Option<String>,
}

impl ThreadDetail {
    pub fn from_projection(p: &Value) -> (ThreadDetail, usize) {
        let mut d = ThreadDetail {
            active_provider_thread: p["thread"]["activeProviderThreadId"]
                .as_str()
                .map(str::to_owned),
            ..ThreadDetail::default()
        };
        let mut skipped = 0;
        let arr = |k: &str| p[k].as_array().cloned().unwrap_or_default();
        for v in arr("providerThreads") {
            match ProviderThread::deserialize(&v) {
                Ok(t) => d.put_provider_thread(t),
                Err(_) => skipped += 1,
            }
        }
        for v in arr("runtimeRequests") {
            match RuntimeRequest::deserialize(&v) {
                Ok(r) => {
                    d.requests.insert(r.id.clone(), r);
                }
                Err(_) => skipped += 1,
            }
        }
        for v in arr("turnItems") {
            match turn_item(&v) {
                Ok(i) => d.put_item(i),
                Err(_) => skipped += 1,
            }
        }
        (d, skipped)
    }

    pub fn apply(&mut self, ev: &ThreadEvent) -> Result<(), serde_json::Error> {
        match ev.kind.as_str() {
            "runtime-request.updated" => {
                let r = RuntimeRequest::deserialize(&ev.payload)?;
                self.requests.insert(r.id.clone(), r);
            }
            "turn-item.updated" => self.put_item(turn_item(&ev.payload)?),
            "provider-thread.updated" => {
                self.put_provider_thread(ProviderThread::deserialize(&ev.payload)?)
            }
            "thread.metadata-updated" => {
                if let Some(v) = ev.payload.get("activeProviderThreadId") {
                    self.active_provider_thread = v.as_str().map(str::to_owned);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn put_provider_thread(&mut self, t: ProviderThread) {
        let native = t.native_thread_ref.and_then(|r| r.native_id);
        self.provider_order.retain(|i| *i != t.id);
        self.provider_order.push_back(t.id.clone());
        self.natives.insert(t.id, native);
    }

    /// Drops history the reconciler no longer needs (see Interfaces).
    pub fn prune(&mut self, keep: &[&str]) {
        self.requests
            .retain(|id, r| r.status == "pending" || keep.contains(&id.as_str()));
        let requests = &self.requests;
        let mut joined: HashSet<String> = HashSet::new();
        self.items.retain(|_, i| match i {
            TurnItem::ApprovalRequest {
                request_id,
                native_item_ref,
                ..
            } => {
                let kept = requests.contains_key(request_id);
                if kept && let Some(n) = native_item_ref.as_ref().and_then(|r| r.native_id.clone())
                {
                    joined.insert(n);
                }
                kept
            }
            TurnItem::UserInputRequest { request_id, .. } => requests.contains_key(request_id),
            _ => true,
        });
        let terminal = |s: &Option<String>| {
            matches!(
                s.as_deref(),
                Some("completed" | "failed" | "declined" | "cancelled")
            )
        };
        let items = &mut self.items;
        self.command_order.retain(|id| match items.get(id) {
            Some(TurnItem::CommandExecution {
                status,
                native_item_ref,
                ..
            }) => {
                let referenced = native_item_ref
                    .as_ref()
                    .and_then(|r| r.native_id.as_ref())
                    .is_some_and(|n| joined.contains(n));
                let keep = referenced || !terminal(status);
                if !keep {
                    items.remove(id);
                }
                keep
            }
            _ => false,
        });
        // Cap only the unreferenced ones; a live approval's command survives.
        let unreferenced: Vec<String> = self.command_order.iter().filter(|id| !matches!(self.items.get(*id),
            Some(TurnItem::CommandExecution { native_item_ref: Some(NativeRef { native_id: Some(n) }), .. }) if joined.contains(n)))
            .cloned().collect();
        for old in unreferenced
            .iter()
            .take(unreferenced.len().saturating_sub(MAX_COMMAND_ITEMS))
        {
            self.items.remove(old);
            self.command_order.retain(|i| i != old);
        }
        let active = self.active_provider_thread.clone();
        while self.provider_order.len() > 8 {
            let Some(pos) = self
                .provider_order
                .iter()
                .position(|i| Some(i) != active.as_ref())
            else {
                break;
            };
            if let Some(old) = self.provider_order.remove(pos) {
                self.natives.remove(&old);
            }
        }
    }

    pub fn retained(&self) -> (usize, usize) {
        (self.requests.len(), self.items.len())
    }

    fn put_item(&mut self, i: TurnItem) {
        let id = match &i {
            TurnItem::ApprovalRequest { id, .. } | TurnItem::UserInputRequest { id, .. } => {
                id.clone()
            }
            TurnItem::CommandExecution { id, .. } => {
                if !self.items.contains_key(id) {
                    self.command_order.push_back(id.clone());
                }
                id.clone()
            }
            TurnItem::Other => return,
        };
        self.items.insert(id, i);
    }

    /// The provider session id of the active provider thread (gate 3 §8).
    pub fn native_session_id(&self) -> Option<&str> {
        let active = self.active_provider_thread.as_ref()?;
        self.natives.get(active)?.as_deref()
    }

    pub fn request_status(&self, id: &str) -> Option<&str> {
        self.requests.get(id).map(|r| r.status.as_str())
    }

    pub fn pending_view(&self, id: &str) -> Option<PendingView> {
        let request = self
            .requests
            .get(id)
            .filter(|r| r.status == "pending")?
            .clone();
        let item = self
            .items
            .values()
            .find(|i| match i {
                TurnItem::ApprovalRequest { request_id, .. }
                | TurnItem::UserInputRequest { request_id, .. } => request_id == id,
                _ => false,
            })
            .cloned();
        let native = match &item {
            Some(TurnItem::ApprovalRequest {
                native_item_ref: Some(NativeRef { native_id: Some(n) }),
                ..
            }) => Some(n.clone()),
            _ => None,
        };
        let command = native.and_then(|n| {
            self.items.values().find_map(|i| match i {
                TurnItem::CommandExecution {
                    native_item_ref: Some(NativeRef { native_id: Some(m) }),
                    input,
                    ..
                } if *m == n => input.clone(),
                _ => None,
            })
        });
        Some(PendingView {
            request,
            item,
            command,
        })
    }
}
