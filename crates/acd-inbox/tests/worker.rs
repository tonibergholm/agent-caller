use std::time::{Duration, Instant};

use acd_inbox::secrets::fake::{FakeBackend, Hang, UnlockBehaviour};
use acd_inbox::secrets::{BackendError, SecretWorker, WorkerError, WorkerTimeouts};

const QUICK: WorkerTimeouts = WorkerTimeouts { op: Duration::from_millis(200), unlock: Duration::from_millis(600) };

fn calls(b: &FakeBackend) -> Vec<String> {
    b.0.lock().unwrap().calls.clone()
}

#[test]
fn operations_reach_the_backend() {
    let b = FakeBackend::default();
    let w = SecretWorker::spawn(b.clone(), QUICK);
    w.write("k", "v").unwrap();
    assert_eq!(w.read("k").unwrap().as_deref(), Some("v"));
    w.delete("k").unwrap();
    assert_eq!(w.read("k").unwrap(), None);
    assert_eq!(calls(&b), ["write k", "read k", "delete k", "read k"]);
}

#[test]
fn a_job_queued_past_its_deadline_never_runs() {
    let b = FakeBackend::default();
    b.0.lock().unwrap().unlock = UnlockBehaviour::Hang;
    let w = std::sync::Arc::new(SecretWorker::spawn(b.clone(), QUICK));
    let unlocking = {
        let w = w.clone();
        std::thread::spawn(move || w.unlock())
    };
    while !calls(&b).contains(&"unlock".to_owned()) {
        std::thread::yield_now();
    }
    let started = Instant::now();
    assert_eq!(w.write("k", "v"), Err(WorkerError::Expired), "queued behind the prompt past its deadline");
    assert!(started.elapsed() < QUICK.unlock + Duration::from_secs(1));
    assert_eq!(unlocking.join().unwrap(), Err(WorkerError::Backend(BackendError::Timeout)));
    assert!(!calls(&b).iter().any(|c| c.starts_with("write")), "the expired write never ran");
    assert!(b.0.lock().unwrap().items.is_empty());
}

/// Lets an `UntilReleased` change finish, on drop too.
struct Release(FakeBackend);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.0.lock().unwrap().released = true;
    }
}

#[test]
fn a_write_past_its_deadline_settles_before_the_next_job_runs() {
    let b = FakeBackend::default();
    b.0.lock().unwrap().hang_writes = Hang::UntilReleased;
    let release = Release(b.clone());
    let w = std::sync::Arc::new(SecretWorker::spawn(b.clone(), WorkerTimeouts { op: Duration::from_millis(300), ..QUICK }));
    assert_eq!(w.write("k", "v"), Err(WorkerError::Ambiguous), "the caller stopped waiting; the write still runs");
    let reading = {
        let w = w.clone();
        std::thread::spawn(move || w.read("k"))
    };
    while w.pending_for_test() == 0 {
        std::thread::yield_now();
    }
    drop(release);
    assert_eq!(reading.join().unwrap().unwrap().as_deref(), Some("v"), "the read ran after the write settled");
    assert_eq!(calls(&b), ["write k", "read k"]);
}

#[test]
fn a_hung_write_makes_every_later_job_fail_without_running() {
    let b = FakeBackend::default();
    b.0.lock().unwrap().hang_writes = Hang::Forever;
    let w = SecretWorker::spawn(b.clone(), QUICK);
    assert_eq!(w.write("k", "v"), Err(WorkerError::Ambiguous));
    assert_eq!(w.read("k"), Err(WorkerError::Backend(BackendError::Timeout)));
    assert_eq!(w.delete("k"), Err(WorkerError::Ambiguous));
    assert_eq!(calls(&b), ["write k"], "nothing ran behind the hung write");
}

#[test]
fn a_change_that_needs_a_prompt_is_dismissed_and_reported_locked() {
    let b = FakeBackend::default();
    b.0.lock().unwrap().prompts = true;
    let w = SecretWorker::spawn(b.clone(), QUICK);
    assert_eq!(w.write("k", "v"), Err(WorkerError::Backend(BackendError::Locked)));
    assert_eq!(w.delete("k"), Err(WorkerError::Backend(BackendError::Locked)));
    assert!(calls(&b).contains(&"dismiss write k".to_owned()) && calls(&b).contains(&"dismiss delete k".to_owned()));
    assert!(b.0.lock().unwrap().items.is_empty());
}

#[test]
fn a_slow_unlock_is_a_plain_timeout_and_reads_never_prompt() {
    let b = FakeBackend::default();
    b.0.lock().unwrap().unlock = UnlockBehaviour::Hang;
    let w = SecretWorker::spawn(b.clone(), QUICK);
    assert_eq!(w.unlock(), Err(WorkerError::Backend(BackendError::Timeout)));
    b.0.lock().unwrap().unlock = UnlockBehaviour::Dismiss;
    assert_eq!(w.unlock(), Err(WorkerError::Backend(BackendError::Dismissed)));
    b.0.lock().unwrap().locked = true;
    assert_eq!(w.read("k"), Err(WorkerError::Backend(BackendError::Locked)));
    assert_eq!(calls(&b).iter().filter(|c| *c == "unlock").count(), 2, "the locked read did not prompt");
}
