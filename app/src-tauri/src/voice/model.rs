//! The on-device intent model (voice spec §5.4, plan V1 to V6): Apple
//! Foundation Models behind the Swift bridge. Prompt and reply cross as
//! text and are never logged; the log line has the outcome, the reply's
//! length and the time.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use acd_voice::router::model::{IntentModel, ModelError, ModelState, ModelUnavailable};
use acd_voice::router::prompt::{INSTRUCTIONS, Prompt};
use acd_voice::text::VoiceText;
use async_trait::async_trait;
use tokio::sync::oneshot;

use super::bridge::{VB_ERR_NOT_INITIALIZED, VB_OK, ffi};

/// How long a plan may take (plan V5; owner decision 1).
pub const MODEL_TIMEOUT: Duration = Duration::from_secs(5);

/// The C calls, behind a trait so tests run without Swift. Every call
/// returns at once.
pub trait ModelPort: Send + Sync {
    fn plan(&self, id: u64, instructions: &str, input: &str, timeout_ms: u32) -> i32;
    fn cancel(&self, id: u64);
    fn cancel_all(&self);
    fn prewarm(&self, instructions: &str);
    /// `force`: report even if unchanged (after an overflow only).
    fn query(&self, force: bool);
}

pub struct SwiftModelPort;

impl ModelPort for SwiftModelPort {
    fn plan(&self, id: u64, instructions: &str, input: &str, timeout_ms: u32) -> i32 {
        // SAFETY: both buffers live for the call; Swift copies them before
        // returning (header contract).
        unsafe { ffi::vb_model_plan(id, instructions.as_ptr(), instructions.len(), input.as_ptr(), input.len(), timeout_ms) }
    }
    fn cancel(&self, id: u64) {
        // SAFETY: a plain integer; returns at once.
        unsafe { ffi::vb_model_cancel(id) }
    }
    fn cancel_all(&self) {
        // SAFETY: no arguments; returns at once.
        unsafe { ffi::vb_model_cancel_all() }
    }
    fn prewarm(&self, instructions: &str) {
        // SAFETY: the buffer lives for the call; Swift copies it.
        unsafe { ffi::vb_model_prewarm(instructions.as_ptr(), instructions.len()) }
    }
    fn query(&self, force: bool) {
        // SAFETY: a plain integer; returns at once.
        unsafe { ffi::vb_model_query(u32::from(force)) }
    }
}

type Reply = Result<VoiceText, ModelError>;

pub struct MacIntentModel {
    port: Arc<dyn ModelPort>,
    next: AtomicU64,
    waiting: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    state: Mutex<ModelState>,
    timeout: Duration,
}

impl MacIntentModel {
    pub fn new(port: Arc<dyn ModelPort>, timeout: Duration) -> Self {
        Self { port, next: AtomicU64::new(0), waiting: Mutex::new(HashMap::new()), state: Mutex::new(ModelState::Checking), timeout }
    }

    /// For Settings. Never used to refuse a plan: Swift checks every time (V6).
    pub fn state(&self) -> ModelState {
        *self.state.lock().unwrap()
    }

    pub fn set_state(&self, state: ModelState) {
        *self.state.lock().unwrap() = state;
    }

    /// Asks Swift for availability; a change arrives as `ModelCaps`.
    pub fn query(&self) {
        self.port.query(false);
    }

    /// The bridge dropped events: the last `ModelCaps` may be among them,
    /// so ask for a report even if nothing changed (review item 6).
    pub fn overflowed(&self) {
        self.port.query(true);
    }

    /// A reply from Swift. False if nothing waits for `id` any more
    /// (cancelled, timed out or discarded): the reply is dropped.
    pub fn deliver(&self, id: u64, reply: Reply) -> bool {
        match self.waiting.lock().unwrap().remove(&id) {
            Some(tx) => tx.send(reply).is_ok(),
            None => false,
        }
    }
}

/// Cancels the Swift request unless it replied (plan V5).
struct Pending<'a> {
    model: &'a MacIntentModel,
    id: u64,
    answered: bool,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.model.waiting.lock().unwrap().remove(&self.id);
        if !self.answered {
            self.model.port.cancel(self.id);
        }
    }
}

fn outcome(reply: &Reply) -> &'static str {
    match reply {
        Ok(_) => "ok",
        Err(ModelError::Unavailable(_)) => "unavailable",
        Err(ModelError::Timeout) => "timeout",
        Err(ModelError::Failed) => "failed",
    }
}

#[async_trait]
impl IntentModel for MacIntentModel {
    async fn plan(&self, prompt: &Prompt) -> Reply {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().unwrap().insert(id, tx);
        let mut pending = Pending { model: self, id, answered: false };
        let started = Instant::now();
        let input = prompt.render_input();
        let timeout_ms = u32::try_from(self.timeout.as_millis()).unwrap_or(u32::MAX);
        let code = self.port.plan(id, INSTRUCTIONS, input.expose(), timeout_ms);
        let reply = if code != VB_OK {
            pending.answered = true; // nothing started in Swift
            Err(if code == VB_ERR_NOT_INITIALIZED { ModelError::Unavailable(ModelUnavailable::Unknown) } else { ModelError::Failed })
        } else {
            match tokio::time::timeout(self.timeout, rx).await {
                Ok(Ok(reply)) => {
                    pending.answered = true;
                    reply
                }
                // Discarded (voice off, lock, overflow). Not `answered`: a
                // discard can land after the id was registered and before
                // Swift started the request, and then Swift's `cancelAll`
                // missed it. The extra cancel is harmless (Swift ignores
                // unknown ids).
                Ok(Err(_)) => Err(ModelError::Failed),
                Err(_) => Err(ModelError::Timeout),
            }
        };
        drop(pending);
        tracing::info!(
            target: "acd_voice",
            outcome = outcome(&reply),
            reply_chars = reply.as_ref().map(|t| t.char_len()).unwrap_or(0),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "model plan"
        );
        reply
    }

    fn receives_details(&self) -> bool {
        true
    }

    fn prewarm(&self) {
        self.port.prewarm(INSTRUCTIONS);
    }

    fn discard(&self) {
        self.waiting.lock().unwrap().clear();
        self.port.cancel_all();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Every test that runs `plan` takes this, so the log-capture test is the
    /// only one touching the `model plan` log callsite while its subscriber is
    /// installed (tracing caches a callsite's interest per process; a parallel
    /// test hitting it without a subscriber made the capture flaky).
    static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[derive(Default)]
    struct FakePort {
        code: Mutex<i32>,
        plans: Mutex<Vec<(u64, String, String, u32)>>,
        cancels: Mutex<Vec<u64>>,
        cancel_alls: AtomicUsize,
        prewarms: AtomicUsize,
        queries: Mutex<Vec<bool>>,
        /// Runs inside `plan`, to model a discard that lands before Swift starts the request.
        during_plan: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    }

    impl ModelPort for FakePort {
        fn plan(&self, id: u64, instructions: &str, input: &str, timeout_ms: u32) -> i32 {
            self.plans.lock().unwrap().push((id, instructions.to_owned(), input.to_owned(), timeout_ms));
            if let Some(hook) = self.during_plan.lock().unwrap().as_ref() {
                hook();
            }
            *self.code.lock().unwrap()
        }
        fn cancel(&self, id: u64) {
            self.cancels.lock().unwrap().push(id);
        }
        fn cancel_all(&self) {
            self.cancel_alls.fetch_add(1, Ordering::SeqCst);
        }
        fn prewarm(&self, _instructions: &str) {
            self.prewarms.fetch_add(1, Ordering::SeqCst);
        }
        fn query(&self, force: bool) {
            self.queries.lock().unwrap().push(force);
        }
    }

    /// Review item 6: Settings refreshes never force (no loop, V16); an
    /// overflow forces, because it may have dropped the last report.
    #[test]
    fn settings_queries_do_not_force_and_an_overflow_does() {
        let (model, port) = rig(Duration::from_secs(5));
        model.query();
        model.overflowed();
        assert_eq!(*port.queries.lock().unwrap(), [false, true]);
    }

    fn prompt(said: &str) -> Prompt {
        Prompt { data: "{\"agents\":[]}".into(), transcript: VoiceText::new(said) }
    }

    fn rig(timeout: Duration) -> (Arc<MacIntentModel>, Arc<FakePort>) {
        let port = Arc::new(FakePort::default());
        (Arc::new(MacIntentModel::new(port.clone(), timeout)), port)
    }

    async fn sent(port: &FakePort) -> u64 {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(p) = port.plans.lock().unwrap().last() {
                    return p.0;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("the plan reached the port")
    }

    /// Plan V2: instructions and untrusted input cross separately.
    #[tokio::test]
    async fn a_reply_reaches_the_waiting_plan_and_the_prompt_crosses_in_two_parts() {
        let _serial = SERIAL.lock().await;
        let (model, port) = rig(Duration::from_secs(5));
        let p = prompt("approve");
        let m = model.clone();
        let task = tokio::spawn(async move { m.plan(&prompt("approve")).await });
        let id = sent(&port).await;
        assert!(model.deliver(id, Ok(VoiceText::new("{\"step\":\"none\"}"))));
        assert_eq!(task.await.unwrap().unwrap().expose(), "{\"step\":\"none\"}");
        let (_, instructions, input, timeout_ms) = port.plans.lock().unwrap()[0].clone();
        assert_eq!(instructions, INSTRUCTIONS);
        assert_eq!(input, p.render_input().expose());
        assert_eq!(timeout_ms, 5_000);
        assert!(port.cancels.lock().unwrap().is_empty(), "a reply needs no cancel");
        assert!(model.receives_details(), "spec §5.4: on-device");
    }

    /// Review Focus 1.
    #[tokio::test]
    async fn a_timeout_cancels_the_swift_request_and_a_late_reply_is_dropped() {
        let _serial = SERIAL.lock().await;
        let (model, port) = rig(Duration::from_millis(50));
        assert_eq!(model.plan(&prompt("x")).await, Err(ModelError::Timeout));
        let id = port.plans.lock().unwrap()[0].0;
        assert_eq!(*port.cancels.lock().unwrap(), [id]);
        assert!(!model.deliver(id, Ok(VoiceText::new("{}"))), "a late reply is dropped");
    }

    /// Review Focus 1, plan V9: the controller drops the route future.
    #[tokio::test]
    async fn dropping_the_plan_cancels_the_swift_request() {
        let _serial = SERIAL.lock().await;
        let (model, port) = rig(Duration::from_secs(5));
        let m = model.clone();
        let task = tokio::spawn(async move { m.plan(&prompt("x")).await });
        let id = sent(&port).await;
        task.abort();
        let _ = task.await;
        assert_eq!(*port.cancels.lock().unwrap(), [id]);
        assert!(!model.deliver(id, Ok(VoiceText::new("{}"))));
    }

    #[tokio::test]
    async fn a_refused_start_is_classified() {
        let _serial = SERIAL.lock().await;
        let (model, port) = rig(Duration::from_secs(5));
        *port.code.lock().unwrap() = super::super::bridge::VB_ERR_NOT_INITIALIZED;
        assert_eq!(model.plan(&prompt("x")).await, Err(ModelError::Unavailable(ModelUnavailable::Unknown)));
        *port.code.lock().unwrap() = super::super::bridge::VB_ERR_FAILED;
        assert_eq!(model.plan(&prompt("x")).await, Err(ModelError::Failed));
        assert!(port.cancels.lock().unwrap().is_empty());
    }

    /// Review: a discard between registering the id and the Swift start must
    /// still cancel that request, or it runs on after voice off or a lock.
    #[tokio::test]
    async fn a_discard_before_swift_started_the_request_still_cancels_it() {
        let _serial = SERIAL.lock().await;
        let (model, port) = rig(Duration::from_secs(5));
        let m = model.clone();
        *port.during_plan.lock().unwrap() = Some(Box::new(move || m.discard()));
        assert_eq!(model.plan(&prompt("x")).await, Err(ModelError::Failed));
        let id = port.plans.lock().unwrap()[0].0;
        assert_eq!(*port.cancels.lock().unwrap(), [id], "the request is cancelled in Swift");
    }

    #[tokio::test]
    async fn discard_cancels_everything_and_ends_waiting_plans() {
        let _serial = SERIAL.lock().await;
        let (model, port) = rig(Duration::from_secs(5));
        let m = model.clone();
        let task = tokio::spawn(async move { m.plan(&prompt("x")).await });
        sent(&port).await;
        model.discard();
        assert_eq!(task.await.unwrap(), Err(ModelError::Failed));
        assert_eq!(port.cancel_alls.load(Ordering::SeqCst), 1);
        model.prewarm();
        assert_eq!(port.prewarms.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_state_starts_checking_and_follows_swift() {
        let (model, _) = rig(Duration::from_secs(5));
        assert_eq!(model.state(), ModelState::Checking);
        model.set_state(ModelState::Unavailable(ModelUnavailable::IntelligenceOff));
        assert_eq!(model.state(), ModelState::Unavailable(ModelUnavailable::IntelligenceOff));
    }

    /// Spec §3.7, §6.3: the transcript, the data and the model's reply never
    /// reach the logs; the log line has the outcome, the length and the time.
    #[tokio::test(flavor = "current_thread")]
    async fn model_text_never_reaches_the_logs() {
        let _serial = SERIAL.lock().await;
        #[derive(Clone, Default)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let (model, port) = rig(Duration::from_secs(5));
        let m = model.clone();
        let task = tokio::spawn(async move {
            let p = Prompt { data: "{\"title\":\"SECRET-DATA\"}".into(), transcript: VoiceText::new("SECRET-SAID") };
            m.plan(&p).await
        });
        let id = sent(&port).await;
        model.deliver(id, Ok(VoiceText::new("{\"step\":\"send\",\"text\":\"SECRET-REPLY\"}")));
        task.await.unwrap().unwrap();
        let logs = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("model plan") && logs.contains("outcome=\"ok\""), "{logs}");
        assert!(!logs.contains("SECRET"), "{logs}");
    }

    #[cfg(feature = "model-contract")]
    mod contract {
        use super::super::*;
        use crate::voice::bridge::{self, BridgeEvent};
        use acd_protocol::{AgentKind, Effect, RequestKind};
        use acd_voice::router::model::ModelPlan;
        use acd_voice::router::prompt::PromptOptions;
        use acd_voice::router::{RouteOptions, Routed, Step, route};
        use acd_voice::router::target::Provenance;
        use acd_voice::safety::Level;
        use acd_voice::snapshot::Snapshot;
        use acd_voice::snapshot::fixtures::{option, pending, snapshot, working};
        use std::sync::atomic::AtomicBool;

        /// Records whether each reply parsed as a `ModelPlan`; never keeps it.
        struct Parsing {
            inner: Arc<MacIntentModel>,
            parsed: AtomicBool,
            /// `ModelPlan`'s own `Debug`: the step, which fields are set, and
            /// the text's length. Never the model's words (spec §6.3).
            shape: Mutex<String>,
        }

        #[async_trait]
        impl IntentModel for Parsing {
            async fn plan(&self, prompt: &Prompt) -> Result<VoiceText, ModelError> {
                let reply = self.inner.plan(prompt).await;
                let plan = reply.as_ref().ok().and_then(|r| serde_json::from_str::<ModelPlan>(r.expose()).ok());
                self.parsed.store(plan.is_some(), Ordering::SeqCst);
                *self.shape.lock().unwrap() = plan.map(|p| format!("{p:?}")).unwrap_or_default();
                reply
            }
            fn receives_details(&self) -> bool {
                true
            }
        }

        fn fixture() -> Snapshot {
            let mut fix = pending("minos", "fix");
            fix.title = "Fix the build".into();
            let mut docs = working("minos", "docs");
            docs.kind = AgentKind::Codex;
            docs.title = "Update the docs".into();
            let mut deploy = pending("studio", "deploy");
            deploy.title = "Deploy the site".into();
            let r = deploy.pending.as_mut().unwrap();
            r.kind = RequestKind::Question;
            r.title = "Where to deploy?".into();
            r.question = Some("Deploy to staging or production?".into());
            r.free_text = true;
            r.options = vec![option("1", "Staging", Effect::Choose), option("2", "Production", Effect::Choose)];
            snapshot(vec![fix, docs, deploy])
        }

        fn confirmed_on(r: &Routed, card: &str) -> bool {
            matches!(r, Routed::Proposal(p) if p.level == Level::Confirm && p.step.card() == Some(card))
        }
        fn sends_to(r: &Routed, card: &str) -> bool {
            confirmed_on(r, card) && matches!(r, Routed::Proposal(p) if matches!(p.step, Step::Send { .. }))
        }
        fn is_step(r: &Routed, step: Step) -> bool {
            matches!(r, Routed::Proposal(p) if p.step == step)
        }
        fn answers(r: &Routed, card: &str) -> bool {
            matches!(r, Routed::Proposal(p) if p.step.card() == Some(card) && matches!(p.step, Step::Answer { .. }))
        }
        fn refused(r: &Routed) -> bool {
            matches!(r, Routed::Refused(_))
        }
        fn inferred_send(r: &Routed) -> bool {
            matches!(r, Routed::Proposal(p) if p.level == Level::Confirm
                && p.provenance == Some(Provenance::Inferred)
                && matches!(p.step, Step::Send { .. }))
        }

        type Check = Box<dyn Fn(&Routed) -> bool>;

        fn phrases() -> Vec<(&'static str, Check)> {
            vec![
                ("tell the minos codex to update the readme", Box::new(|r| sends_to(r, "minos/docs"))),
                ("ask the codex to run clippy", Box::new(|r| sends_to(r, "minos/docs"))),
                ("send run the tests to the codex on minos", Box::new(|r| sends_to(r, "minos/docs"))),
                ("tell the build agent on minos to try again", Box::new(|r| confirmed_on(r, "minos/fix"))),
                ("answer the deploy question with staging", Box::new(|r| confirmed_on(r, "studio/deploy"))),
                ("for the deploy, choose production", Box::new(|r| confirmed_on(r, "studio/deploy"))),
                ("tell studio to deploy to staging", Box::new(|r| confirmed_on(r, "studio/deploy"))),
                ("approve the build on minos", Box::new(|r| answers(r, "minos/fix"))),
                ("deny the minos request", Box::new(|r| answers(r, "minos/fix"))),
                ("read the minos request", Box::new(|r| is_step(r, Step::Read { card: "minos/fix".into() }))),
                ("what is the deploy question", Box::new(|r| is_step(r, Step::Read { card: "studio/deploy".into() }))),
                ("summarize minos", Box::new(|r| is_step(r, Step::Summarize { machine: Some("minos".into()) }))),
                ("what's waiting on studio", Box::new(|r| is_step(r, Step::Summarize { machine: Some("studio".into()) }))),
                ("give me a summary of everything", Box::new(|r| is_step(r, Step::Summarize { machine: None }))),
                ("show the deploy agent", Box::new(|r| is_step(r, Step::Select { card: "studio/deploy".into() }))),
                ("go to the codex", Box::new(|r| is_step(r, Step::Select { card: "minos/docs".into() }))),
                ("tell claude to keep going", Box::new(|r| matches!(r, Routed::WhichOne(_)) || refused(r))),
                ("what's the weather like", Box::new(refused)),
                ("play some music", Box::new(refused)),
                // No card is on "omarchy", and Rust finds no name in the
                // transcript, so the model will likely pick some card; Rust
                // then proposes a send to it with an inferred target, which
                // always needs Confirm (review item 12). A refusal is fine too.
                // Never a countdown.
                ("tell the omarchy agent to stop", Box::new(|r| refused(r) || inferred_send(r))),
            ]
        }

        /// Spec §10, plan V18. Run by hand on a Mac with Apple Intelligence on.
        /// The feature keeps the Swift bridge out of the normal test binary; the
        /// rpath lets this one find the Swift runtime (`@rpath` in debug builds):
        /// RUSTFLAGS="-C link-arg=-Wl,-rpath,/usr/lib/swift" cargo test --manifest-path app/src-tauri/Cargo.toml --locked --features model-contract model_contract -- --ignored --nocapture --test-threads=1
        /// Prints phrase numbers, tags and timings, never model output.
        #[tokio::test(flavor = "multi_thread")]
        #[ignore = "needs Apple Intelligence; run by hand (plan Task 5)"]
        async fn model_contract() {
            let (link, events) = bridge::start().expect("the bridge starts");
            let model = Arc::new(MacIntentModel::new(Arc::new(SwiftModelPort), MODEL_TIMEOUT));
            let m = model.clone();
            tokio::spawn(bridge::pump(link, events, move |ev| match ev {
                BridgeEvent::ModelReply { id, reply } => {
                    m.deliver(id, reply);
                }
                BridgeEvent::ModelCaps(state) => m.set_state(state),
                _ => {}
            }));
            model.query();
            for _ in 0..300 {
                if model.state() != ModelState::Checking {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(model.state(), ModelState::Available, "Apple Intelligence must be on");
            let s = fixture();
            let opts = RouteOptions { prompt: PromptOptions { include_details: true }, ..RouteOptions::default() };
            let (mut passed, mut parsed, mut times) = (0, 0, Vec::new());
            for (n, (said, check)) in phrases().iter().enumerate() {
                let probe = Parsing { inner: model.clone(), parsed: AtomicBool::new(false), shape: Mutex::new(String::new()) };
                let started = Instant::now();
                let routed = route(&s, &VoiceText::new(*said), Some(&probe), &opts).await;
                let ms = started.elapsed().as_millis() as u64;
                let (ok, valid) = (check(&routed), probe.parsed.load(Ordering::SeqCst));
                passed += usize::from(ok);
                parsed += usize::from(valid);
                times.push(ms);
                // Tags only: the refusal is a classified variant, the plan shape has no text.
                let why = if let Routed::Refused(r) = &routed { format!(" refusal={r:?}") } else { String::new() };
                println!(
                    "{:>2} {} schema_valid={valid} route={}{why} model={} {ms} ms",
                    n + 1,
                    if ok { "pass" } else { "FAIL" },
                    routed.tag(),
                    probe.shape.lock().unwrap()
                );
            }
            // Review item 9: the first call is cold (no prewarm, model not
            // loaded) and the user meets it after every idle period, so it is
            // reported on its own; p50 and p95 cover the warm calls 2 to 20.
            let first = times[0];
            let mut warm = times[1..].to_vec();
            warm.sort_unstable();
            let pct = |p: usize| warm[(warm.len() * p / 100).min(warm.len() - 1)];
            println!(
                "passed {passed}/20, schema-valid {parsed}/20, first (cold) {first} ms, warm p50 {} ms, warm p95 {} ms",
                pct(50),
                pct(95)
            );
            assert_eq!(parsed, 20, "every reply is a schema-valid plan");
            assert!(passed >= 18, "at least 18 of 20 route as expected");
        }
    }
}
