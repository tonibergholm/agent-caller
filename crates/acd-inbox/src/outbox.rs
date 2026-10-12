//! One connection's outgoing lines (omarchy spec §5.1, plan D12): hello,
//! settings and view first; then replies, outcomes (bounded, never
//! coalesced) and the newest settings and view (coalesced), taking turns.
use std::collections::VecDeque;
use std::sync::Mutex;

use tokio::sync::Notify;

use crate::protocol;

pub const OUTCOME_QUEUE: usize = 256;
/// Replies a view may have unread before it is disconnected.
pub const REPLY_QUEUE: usize = 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Hello,
    Settings,
    View,
    Open,
}

struct State {
    phase: Phase,
    hello: Option<String>,
    replies: VecDeque<String>,
    outcomes: VecDeque<String>,
    lost: bool,
    settings: Option<String>,
    view: Option<String>,
    /// The class served last: 0 replies, 1 outcomes, 2 settings, 3 view.
    last: usize,
    closed: bool,
}

pub struct Outbox {
    state: Mutex<State>,
    ready: Notify,
    outcome_cap: usize,
    reply_cap: usize,
}

impl Outbox {
    pub fn new(hello: String, outcome_cap: usize, reply_cap: usize) -> Outbox {
        Outbox {
            state: Mutex::new(State {
                phase: Phase::Hello,
                hello: Some(hello),
                replies: VecDeque::new(),
                outcomes: VecDeque::new(),
                lost: false,
                settings: None,
                view: None,
                last: 3,
                closed: false,
            }),
            ready: Notify::new(),
            outcome_cap,
            reply_cap,
        }
    }

    fn update(&self, f: impl FnOnce(&mut State)) {
        let mut s = self.state.lock().unwrap();
        if s.closed {
            return;
        }
        f(&mut s);
        drop(s);
        self.ready.notify_one();
    }

    /// Queues a reply. False when the queue is full: the caller closes the
    /// connection. A closed outbox drops the reply and returns true.
    pub fn reply(&self, line: String) -> bool {
        let mut s = self.state.lock().unwrap();
        if s.closed {
            return true;
        }
        if s.replies.len() >= self.reply_cap {
            return false;
        }
        s.replies.push_back(line);
        drop(s);
        self.ready.notify_one();
        true
    }

    /// Queues an outcome. A full queue is dropped and one `outcomes_lost`
    /// becomes due instead (omarchy spec §5.1).
    pub fn outcome(&self, line: String) {
        let cap = self.outcome_cap;
        self.update(|s| {
            if s.outcomes.len() >= cap {
                s.outcomes.clear();
                s.lost = true;
            } else {
                s.outcomes.push_back(line);
            }
        });
    }

    /// The service's own outcome subscription lagged.
    pub fn outcomes_lost(&self) {
        self.update(|s| {
            s.outcomes.clear();
            s.lost = true;
        });
    }

    pub fn set_settings(&self, line: String) {
        self.update(|s| s.settings = Some(line));
    }

    pub fn set_view(&self, line: String) {
        self.update(|s| s.view = Some(line));
    }

    pub fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.ready.notify_one();
    }

    pub fn is_closed(&self) -> bool {
        self.state.lock().unwrap().closed
    }

    /// The next line to write, waiting until one is due; `None` once closed.
    /// One writer task owns this: a second concurrent caller would share
    /// one wake-up (`notify_one`) and could leave a line unwritten.
    pub async fn next(&self) -> Option<String> {
        loop {
            let notified = self.ready.notified();
            if let Some(next) = self.take() {
                return next;
            }
            notified.await;
        }
    }

    /// Test hook: the next due line without waiting.
    #[cfg(any(test, feature = "testing"))]
    pub fn try_next_for_test(&self) -> Option<String> {
        self.take().flatten()
    }

    /// `Some(None)`: closed. `None`: nothing due yet.
    fn take(&self) -> Option<Option<String>> {
        let mut s = self.state.lock().unwrap();
        if s.closed {
            return Some(None);
        }
        match s.phase {
            Phase::Hello => {
                s.phase = Phase::Settings;
                return Some(s.hello.take());
            }
            Phase::Settings => {
                let line = s.settings.take()?;
                s.phase = Phase::View;
                return Some(Some(line));
            }
            Phase::View => {
                let line = s.view.take()?;
                s.phase = Phase::Open;
                return Some(Some(line));
            }
            Phase::Open => {}
        }
        for step in 1..=4 {
            let mut class = (s.last + step) % 4;
            // A view never precedes the settings it was built from: when
            // both are pending, the settings go first whatever the turn.
            if class == 3 && s.settings.is_some() {
                class = 2;
            }
            let line = match class {
                0 => s.replies.pop_front(),
                1 if s.lost => {
                    s.lost = false;
                    Some(protocol::outcomes_lost_line())
                }
                1 => s.outcomes.pop_front(),
                2 => s.settings.take(),
                _ => s.view.take(),
            };
            if let Some(line) = line {
                s.last = class;
                return Some(Some(line));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn next(o: &Outbox) -> String {
        tokio::time::timeout(Duration::from_secs(1), o.next()).await.expect("a line is due").expect("open")
    }

    /// Nothing is due: `next` stays pending. Checked with a poll, not a wait.
    fn nothing_due(o: &Outbox) {
        assert_eq!(o.take(), None);
    }

    async fn greeted(outcome_cap: usize, reply_cap: usize) -> Outbox {
        let o = Outbox::new("hello".into(), outcome_cap, reply_cap);
        o.set_settings("s0".into());
        o.set_view("v0".into());
        for want in ["hello", "s0", "v0"] {
            assert_eq!(next(&o).await, want);
        }
        o
    }

    #[tokio::test]
    async fn hello_settings_and_view_come_first_in_that_order() {
        let o = Outbox::new("hello".into(), OUTCOME_QUEUE, REPLY_QUEUE);
        o.set_view("v1".into());
        o.outcome("o1".into());
        assert!(o.reply("r1".into()));
        assert_eq!(next(&o).await, "hello");
        nothing_due(&o);
        o.set_settings("s1".into());
        assert_eq!(next(&o).await, "s1");
        assert_eq!(next(&o).await, "v1");
        let mut rest = vec![next(&o).await, next(&o).await];
        rest.sort();
        assert_eq!(rest, ["o1", "r1"]);
    }

    #[tokio::test]
    async fn views_and_settings_coalesce_to_the_newest() {
        let o = greeted(OUTCOME_QUEUE, REPLY_QUEUE).await;
        o.set_view("a".into());
        o.set_settings("x".into());
        o.set_view("b".into());
        o.set_settings("y".into());
        let mut got = vec![next(&o).await, next(&o).await];
        got.sort();
        assert_eq!(got, ["b", "y"]);
        nothing_due(&o);
    }

    #[tokio::test]
    async fn classes_take_turns_so_replies_cannot_starve_outcomes() {
        let o = greeted(OUTCOME_QUEUE, REPLY_QUEUE).await;
        for i in 1..=3 {
            assert!(o.reply(format!("r{i}")));
            o.outcome(format!("o{i}"));
        }
        o.set_settings("s".into());
        o.set_view("v".into());
        let got: Vec<String> = [(); 8].iter().map(|_| o.take().unwrap().unwrap()).collect();
        assert_eq!(got, ["r1", "o1", "s", "v", "r2", "o2", "r3", "o3"]);
    }

    #[tokio::test]
    async fn outcomes_are_never_coalesced_and_overflow_sends_one_loss() {
        let o = greeted(3, REPLY_QUEUE).await;
        o.outcome("o1".into());
        o.outcome("o2".into());
        assert_eq!(next(&o).await, "o1");
        assert_eq!(next(&o).await, "o2");
        for i in 0..5 {
            o.outcome(format!("x{i}"));
        }
        assert_eq!(next(&o).await, crate::protocol::outcomes_lost_line());
        assert_eq!(next(&o).await, "x4", "outcomes after the overflow queue again");
        nothing_due(&o);
    }

    #[tokio::test]
    async fn an_upstream_lag_drops_the_queue_and_reports_once() {
        let o = greeted(OUTCOME_QUEUE, REPLY_QUEUE).await;
        o.outcome("o1".into());
        o.outcomes_lost();
        o.outcomes_lost();
        assert_eq!(next(&o).await, crate::protocol::outcomes_lost_line());
        nothing_due(&o);
    }

    #[tokio::test]
    async fn new_settings_precede_a_pending_view_whatever_the_turn() {
        let o = greeted(OUTCOME_QUEUE, REPLY_QUEUE).await;
        o.set_settings("s1".into());
        assert_eq!(next(&o).await, "s1");
        o.set_view("v1".into());
        o.set_settings("s2".into());
        assert_eq!(next(&o).await, "s2");
        assert_eq!(next(&o).await, "v1");
        nothing_due(&o);
    }

    #[tokio::test]
    async fn a_second_overflow_before_the_loss_is_read_still_sends_one_loss() {
        let o = greeted(2, REPLY_QUEUE).await;
        for i in 0..3 {
            o.outcome(format!("a{i}")); // a2 overflows
        }
        o.outcome("b0".into());
        o.outcome("b1".into());
        o.outcome("b2".into()); // overflows again, the loss still unread
        o.outcome("c0".into());
        assert_eq!(next(&o).await, crate::protocol::outcomes_lost_line());
        assert_eq!(next(&o).await, "c0", "only outcomes queued after the last clear survive");
        nothing_due(&o);
    }

    #[tokio::test]
    async fn a_task_parked_in_next_is_woken_by_a_push_from_another_task() {
        let o = std::sync::Arc::new(greeted(OUTCOME_QUEUE, REPLY_QUEUE).await);
        let parked = tokio::spawn({
            let o = o.clone();
            async move { o.next().await }
        });
        tokio::task::yield_now().await;
        let pusher = tokio::spawn({
            let o = o.clone();
            async move { o.outcome("late".into()) }
        });
        pusher.await.unwrap();
        let got = tokio::time::timeout(Duration::from_secs(1), parked).await.expect("woken").unwrap();
        assert_eq!(got.as_deref(), Some("late"));
    }

    #[tokio::test]
    async fn a_full_reply_queue_refuses_more() {
        let o = greeted(OUTCOME_QUEUE, 2).await;
        assert!(o.reply("r1".into()));
        assert!(o.reply("r2".into()));
        assert!(!o.reply("r3".into()));
    }

    #[tokio::test]
    async fn closing_ends_the_stream() {
        let o = greeted(OUTCOME_QUEUE, REPLY_QUEUE).await;
        assert!(o.reply("r".into()));
        o.close();
        assert!(o.is_closed());
        assert_eq!(o.next().await, None);
        assert!(o.reply("ignored".into()), "a reply to a closed view is dropped, not an overflow");
        assert_eq!(o.next().await, None);
    }
}
