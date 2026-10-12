//! The socket protocol (omarchy spec §5): JSON, one object per line, UTF-8.
use acd_client::{
    inbox::PickError,
    view::{CardGroup, InboxView, MachineEntryView},
    voice::{ActionEvent, ActionOutcome, NotSentReason},
};
use serde::Serialize;
use serde_json::{Value, json};

pub const PROTOCOL: u32 = 1;
/// The longest line either side may send, newline excluded.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// The longest command id.
pub const MAX_ID: usize = 64;
/// Appended to agent text the service cut so a view fits in a line.
pub const TRUNCATED: &str = "…[truncated]";

/// Fixed reply codes (plan D8). Views map them to text; the service never
/// sends an error's message.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Stale,
    UnknownCard,
    NotAnswerable,
    UnknownOption,
    AlreadyCounting,
    BadText,
    CannotReply,
    CannotMarkSeen,
    Hidden,
    UnknownMachine,
    NotPaired,
    BadName,
    BadPairing,
    BadRequest,
    SecretStoreLocked,
    SecretStoreTimeout,
    SecretStoreFailed,
    SettingsFailed,
    DuplicateId,
    Unsupported,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Stale => "stale",
            ErrorCode::UnknownCard => "unknown_card",
            ErrorCode::NotAnswerable => "not_answerable",
            ErrorCode::UnknownOption => "unknown_option",
            ErrorCode::AlreadyCounting => "already_counting",
            ErrorCode::BadText => "bad_text",
            ErrorCode::CannotReply => "cannot_reply",
            ErrorCode::CannotMarkSeen => "cannot_mark_seen",
            ErrorCode::Hidden => "hidden",
            ErrorCode::UnknownMachine => "unknown_machine",
            ErrorCode::NotPaired => "not_paired",
            ErrorCode::BadName => "bad_name",
            ErrorCode::BadPairing => "bad_pairing",
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::SecretStoreLocked => "secret_store_locked",
            ErrorCode::SecretStoreTimeout => "secret_store_timeout",
            ErrorCode::SecretStoreFailed => "secret_store_failed",
            ErrorCode::SettingsFailed => "settings_failed",
            ErrorCode::DuplicateId => "duplicate_id",
            ErrorCode::Unsupported => "unsupported",
        }
    }
}

pub fn pick_error_code(e: &PickError) -> ErrorCode {
    match e {
        PickError::UnknownCard => ErrorCode::UnknownCard,
        PickError::NotAnswerable => ErrorCode::NotAnswerable,
        PickError::StaleRequest | PickError::ConnectionChanged => ErrorCode::Stale,
        PickError::UnknownOption => ErrorCode::UnknownOption,
        PickError::AlreadyCounting => ErrorCode::AlreadyCounting,
        PickError::BadText | PickError::BadAnswerText => ErrorCode::BadText,
        PickError::CannotReply => ErrorCode::CannotReply,
        PickError::Hidden => ErrorCode::Hidden,
    }
}

/// The `settings` message (omarchy spec §5.4, plan S1). No tokens or pins:
/// `MachineEntryView` has none.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename = "settings")]
pub struct SettingsSnapshot {
    pub machines: Vec<MachineEntryView>,
    pub notifications: bool,
    pub show_idle: bool,
    pub show_subagents: bool,
    pub show_hook_only: bool,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotSentCode {
    Offline,
    NotSupported,
    AgentGone,
    AnsweredElsewhere,
    Changed,
    AgentBusy,
    Hidden,
}

impl From<NotSentReason> for NotSentCode {
    fn from(r: NotSentReason) -> Self {
        match r {
            NotSentReason::Offline => NotSentCode::Offline,
            NotSentReason::NotSupported => NotSentCode::NotSupported,
            NotSentReason::AgentGone => NotSentCode::AgentGone,
            NotSentReason::AnsweredElsewhere => NotSentCode::AnsweredElsewhere,
            NotSentReason::Changed => NotSentCode::Changed,
            NotSentReason::AgentBusy => NotSentCode::AgentBusy,
            NotSentReason::Hidden => NotSentCode::Hidden,
        }
    }
}

/// `R` in an `outcome` message (omarchy spec §5.3).
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutcomeResult {
    Fired,
    Delivered,
    Rejected { code: acd_protocol::ErrorCode },
    Uncertain,
    Cancelled,
    NotSent { reason: NotSentCode },
}

impl From<ActionOutcome> for OutcomeResult {
    fn from(o: ActionOutcome) -> Self {
        match o {
            ActionOutcome::Fired => OutcomeResult::Fired,
            ActionOutcome::Delivered => OutcomeResult::Delivered,
            ActionOutcome::Rejected { code } => OutcomeResult::Rejected { code },
            ActionOutcome::Uncertain => OutcomeResult::Uncertain,
            ActionOutcome::Cancelled => OutcomeResult::Cancelled,
            ActionOutcome::NotSent { reason } => OutcomeResult::NotSent { reason: reason.into() },
        }
    }
}

pub fn hello_line(version: &str) -> String {
    json!({"type": "hello", "protocol": PROTOCOL, "service": version}).to_string()
}

/// A `settings` line under `max` bytes. Machine entries are small (names
/// at most 64 characters), so the cut below is for robustness: entries are
/// left out from the end and counted in `omitted_machines`, which is always
/// the number of entries removed from `s.machines`.
///
/// Fail closed: when the line is still at or over `max` with no machines
/// left (or none to begin with), the machine-less line is returned anyway.
/// It is the smallest settings line there is, since nothing else in it
/// varies in size, and the `warn` below records that it does not fit.
pub fn settings_line(s: &SettingsSnapshot, max: usize) -> String {
    let whole = serde_json::to_string(s).expect("serializable");
    if whole.len() < max {
        return whole;
    }
    let original = s.machines.len();
    let mut cut = s.clone();
    loop {
        cut.machines.pop();
        let omitted = original - cut.machines.len();
        let mut v = serde_json::to_value(&cut).expect("serializable");
        if omitted > 0 {
            v["omitted_machines"] = omitted.into();
        }
        let line = v.to_string();
        if line.len() < max || cut.machines.is_empty() {
            tracing::warn!(omitted, "settings too large; machines left out");
            return line;
        }
    }
}

pub fn outcome_line(e: &ActionEvent) -> String {
    json!({
        "type": "outcome",
        "action": e.action_id,
        "card": e.card_key,
        "result": OutcomeResult::from(e.outcome),
    })
    .to_string()
}

/// Sent instead of a view that cannot fit even with Needs-you cards only
/// (plan D9): the panel shows an error state with the count.
pub fn view_overflow_line(needs_you: usize) -> String {
    json!({"type": "view_overflow", "needs_you": needs_you}).to_string()
}

/// The view line, or the overflow line when the view cannot fit.
pub fn view_or_overflow_line(view: &InboxView, max: usize) -> String {
    view_line(view, max).unwrap_or_else(|| {
        view_overflow_line(view.cards.iter().filter(|c| c.group == CardGroup::Request).count())
    })
}

pub fn outcomes_lost_line() -> String {
    json!({"type": "outcomes_lost"}).to_string()
}

pub fn ok_line(id: &str, result: Value) -> String {
    json!({"type": "ok", "id": id, "result": result}).to_string()
}

pub fn error_line(id: &str, code: ErrorCode) -> String {
    json!({"type": "error", "id": id, "code": code}).to_string()
}

/// Lets a log line through at most once per `OVERSIZE_LOG_EVERY`: a view that
/// stays too large is re-encoded on every change, and each would warn.
pub(crate) struct LogGate(std::sync::Mutex<Option<std::time::Instant>>);

const OVERSIZE_LOG_EVERY: std::time::Duration = std::time::Duration::from_secs(60);
static OVERSIZE_WARN: LogGate = LogGate::new();
static OVERSIZE_ERROR: LogGate = LogGate::new();

impl LogGate {
    pub(crate) const fn new() -> Self {
        LogGate(std::sync::Mutex::new(None))
    }

    pub(crate) fn allow(&self, now: std::time::Instant) -> bool {
        let mut last = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| now.saturating_duration_since(t) < OVERSIZE_LOG_EVERY) {
            return false;
        }
        *last = Some(now);
        true
    }
}

/// Keys inside an agent whose strings are ids, enums or times: never cut.
const KEEP: &[&str] = &[
    "id", "machine_id", "agent_id", "source", "kind", "state", "status", "answer_path", "effect",
    "state_since", "opened_at", "role",
];
/// Agent text is cut to no less than this before any card is left out.
const FIRST_CAP: usize = 256;
/// The shortest agent text field is ever cut to.
const MIN_CAP: usize = 32;

/// Cards a `view` line left out to fit (plan D9). Always present.
#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Omitted {
    pub done: usize,
    pub working: usize,
}

/// A `view` line under `max` bytes, newline included (plan D9, spec §5
/// as amended in Step 6):
/// 1. the whole view;
/// 2. agent text cut, longest first, to no less than `FIRST_CAP`;
/// 3. then Done and idle cards left out, then Working and unknown ones,
///    except cards with a countdown, counted in `omitted`;
/// 4. then the remaining (Needs-you) text cut down to `MIN_CAP`.
///
/// `None` when even that does not fit: the caller sends `view_overflow`
/// instead. Needs-you cards are never left out.
pub fn view_line(view: &InboxView, max: usize) -> Option<String> {
    let encode = |v: &InboxView, o: Omitted| json!({"type": "view", "view": v, "omitted": o});
    let whole = encode(view, Omitted::default()).to_string();
    if whole.len() < max {
        return Some(whole);
    }
    if let Some(s) = fit_text(&encode(view, Omitted::default()), FIRST_CAP, max) {
        return Some(s);
    }
    let mut v = view.clone();
    let mut omitted = Omitted::default();
    let stages: [(&[CardGroup], bool); 2] =
        [(&[CardGroup::Done, CardGroup::Idle], true), (&[CardGroup::Working, CardGroup::Unknown], false)];
    for (groups, done) in stages {
        let before = v.cards.len();
        v.cards.retain(|c| !groups.contains(&c.group) || c.action.is_some());
        let n = before - v.cards.len();
        if done {
            omitted.done = n;
        } else {
            omitted.working = n;
        }
        if let Some(s) = fit_text(&encode(&v, omitted), FIRST_CAP, max) {
            if OVERSIZE_WARN.allow(std::time::Instant::now()) {
                tracing::warn!(done = omitted.done, working = omitted.working, "view too large; cards left out");
            }
            return Some(s);
        }
    }
    let s = fit_text(&encode(&v, omitted), MIN_CAP, max);
    if s.is_none() && OVERSIZE_ERROR.allow(std::time::Instant::now()) {
        tracing::error!(cards = v.cards.len(), "view too large even with requests only; not sent");
    }
    s
}

/// The largest cap, at least `floor`, at which `v` fits under `max`.
fn fit_text(v: &Value, floor: usize, max: usize) -> Option<String> {
    let fits = |cap: usize| {
        let mut c = v.clone();
        cap_text(&mut c, cap);
        let s = c.to_string();
        (s.len() < max).then_some(s)
    };
    let mut best = fits(floor)?;
    let (mut lo, mut hi) = (floor, longest_text(v).max(floor));
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        match fits(mid) {
            Some(s) => {
                lo = mid;
                best = s;
            }
            None => hi = mid - 1,
        }
    }
    Some(best)
}

fn texts_mut<'a>(v: &'a mut Value, out: &mut Vec<&'a mut String>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(items) => {
            for item in items {
                texts_mut(item, out);
            }
        }
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if !KEEP.contains(&key.as_str()) {
                    texts_mut(value, out);
                }
            }
        }
        _ => {}
    }
}

/// Every cuttable string under `view.cards[*].agent`.
fn agent_texts(v: &mut Value) -> Vec<&mut String> {
    let mut out = Vec::new();
    if let Some(cards) = v["view"]["cards"].as_array_mut() {
        for card in cards {
            if let Some(agent) = card.get_mut("agent") {
                texts_mut(agent, &mut out);
            }
        }
    }
    out
}

fn cap_text(v: &mut Value, cap: usize) {
    for s in agent_texts(v) {
        if s.chars().count() > cap {
            let mut cut: String = s.chars().take(cap).collect();
            cut.push_str(TRUNCATED);
            *s = cut;
        }
    }
}

fn longest_text(v: &Value) -> usize {
    let mut copy = v.clone();
    agent_texts(&mut copy).iter().map(|s| s.chars().count()).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn the_oversize_log_gate_lets_one_line_through_per_interval() {
        let g = super::LogGate::new();
        let t = std::time::Instant::now();
        assert!(g.allow(t));
        assert!(!g.allow(t + std::time::Duration::from_secs(59)));
        assert!(g.allow(t + std::time::Duration::from_secs(61)));
    }
    use acd_client::view::{CardGroup, CardView, TrayColor, TraySummary};
    use acd_client::voice::{ActionEvent, ActionOutcome, NotSentReason};
    use serde_json::{Value, json};

    fn agent(id: &str, title: &str, screen: Option<&str>) -> acd_protocol::Agent {
        serde_json::from_value(json!({"id":id,"machine_id":"m","source":"herdr","kind":"claude","title":title,"cwd":null,"project":null,"state":"needs_you","state_since":"2026-10-08T00:00:00Z","failed":false,"last_message":null,"can_send":true,"pending":{"id":format!("r-{id}"),"revision":1,"agent_id":id,"kind":"permission","status":"open","answer_path":"hook","title":"Bash","detail":"npm test","options":[{"id":"allow","label":"Yes","description":null,"effect":"allow"}],"questions":null,"free_text":false,"raw_screen":screen,"opened_at":"2026-10-08T00:00:00Z"}})).unwrap()
    }

    fn card(a: acd_protocol::Agent) -> CardView {
        CardView {
            key: format!("m1/{}", a.id),
            machine_key: "m1".into(),
            machine_name: "Box".into(),
            agent: a,
            group: CardGroup::Request,
            live: true,
            offline_since_ms: None,
            answerable: true,
            free_text_answerable: false,
            selectable_options: vec!["allow".into()],
            can_reply: false,
            can_mark_seen: false,
            busy: false,
            notice: None,
            action: None,
        }
    }

    fn view(cards: Vec<CardView>) -> InboxView {
        InboxView {
            machines: vec![],
            cards,
            hidden_idle: 0,
            show_idle: false,
            show_subagents: false,
            show_hook_only: false,
            tray: TraySummary { color: TrayColor::Amber, needs_you: 1 },
            actions: vec![],
        }
    }

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    #[test]
    fn hello_settings_and_reply_shapes() {
        assert_eq!(parse(&hello_line("0.1.0")), json!({"type":"hello","protocol":1,"service":"0.1.0"}));
        let s = SettingsSnapshot { machines: vec![], notifications: true, show_idle: false, show_subagents: true, show_hook_only: false };
        assert_eq!(
            parse(&settings_line(&s, MAX_LINE)),
            json!({"type":"settings","machines":[],"notifications":true,"show_idle":false,"show_subagents":true,"show_hook_only":false})
        );
        assert_eq!(parse(&ok_line("c1", json!("a-1"))), json!({"type":"ok","id":"c1","result":"a-1"}));
        assert_eq!(parse(&error_line("c2", ErrorCode::Stale)), json!({"type":"error","id":"c2","code":"stale"}));
        assert_eq!(parse(&outcomes_lost_line()), json!({"type":"outcomes_lost"}));
    }

    #[test]
    fn every_error_code_is_its_fixed_string() {
        use ErrorCode::*;
        for code in [
            Stale, UnknownCard, NotAnswerable, UnknownOption, AlreadyCounting, BadText, CannotReply,
            CannotMarkSeen, Hidden, UnknownMachine, NotPaired, BadName, BadPairing, BadRequest,
            SecretStoreLocked, SecretStoreTimeout, SecretStoreFailed, SettingsFailed, DuplicateId, Unsupported,
        ] {
            assert_eq!(serde_json::to_value(code).unwrap(), code.as_str());
        }
        assert_eq!(pick_error_code(&PickError::AlreadyCounting).as_str(), "already_counting");
        assert_eq!(pick_error_code(&PickError::BadAnswerText).as_str(), "bad_text");
        assert_eq!(pick_error_code(&PickError::ConnectionChanged).as_str(), "stale");
    }

    #[test]
    fn outcomes_map_one_to_one() {
        let line = |outcome| {
            parse(&outcome_line(&ActionEvent { action_id: "a".into(), card_key: "m1/a1".into(), outcome }))
        };
        assert_eq!(line(ActionOutcome::Fired), json!({"type":"outcome","action":"a","card":"m1/a1","result":{"kind":"fired"}}));
        assert_eq!(line(ActionOutcome::Delivered)["result"], json!({"kind":"delivered"}));
        assert_eq!(line(ActionOutcome::Uncertain)["result"], json!({"kind":"uncertain"}));
        assert_eq!(line(ActionOutcome::Cancelled)["result"], json!({"kind":"cancelled"}));
        assert_eq!(
            line(ActionOutcome::Rejected { code: acd_protocol::ErrorCode::RequestGone })["result"],
            json!({"kind":"rejected","code":"request_gone"})
        );
        for (reason, text) in [
            (NotSentReason::Offline, "offline"),
            (NotSentReason::NotSupported, "not_supported"),
            (NotSentReason::AgentGone, "agent_gone"),
            (NotSentReason::AnsweredElsewhere, "answered_elsewhere"),
            (NotSentReason::Changed, "changed"),
            (NotSentReason::AgentBusy, "agent_busy"),
            (NotSentReason::Hidden, "hidden"),
        ] {
            assert_eq!(line(ActionOutcome::NotSent { reason })["result"], json!({"kind":"not_sent","reason":text}));
        }
    }

    /// A Done (or Working) card without a request.
    fn quiet(id: &str, state: &str, group: CardGroup) -> CardView {
        let mut a = agent(id, "quiet agent", None);
        a.pending = None;
        a.state = serde_json::from_value(json!(state)).unwrap();
        CardView { group, answerable: false, selectable_options: vec![], ..card(a) }
    }

    fn line(v: &InboxView, max: usize) -> Value {
        let l = view_line(v, max).expect("fits");
        assert!(l.len() < max, "{} bytes for a {max} limit", l.len());
        parse(&l)
    }

    fn keys(out: &Value) -> Vec<String> {
        out["view"]["cards"].as_array().unwrap().iter().map(|c| c["key"].as_str().unwrap().to_owned()).collect()
    }

    #[test]
    fn a_small_view_is_sent_whole() {
        let v = view(vec![card(agent("a1", "fix tests", None))]);
        assert_eq!(
            line(&v, MAX_LINE),
            json!({"type":"view","view":serde_json::to_value(&v).unwrap(),"omitted":{"done":0,"working":0}})
        );
    }

    #[test]
    fn a_5_mib_screen_is_cut_with_a_marker_and_the_request_survives() {
        let screen = "é".repeat(5 * 1024 * 1024 / 2 + 7);
        let v = view(vec![card(agent("a1", "fix tests", Some(&screen)))]);
        let out = line(&v, MAX_LINE);
        let pending = &out["view"]["cards"][0]["agent"]["pending"];
        assert!(pending["raw_screen"].as_str().unwrap().ends_with(TRUNCATED));
        assert_eq!(pending["id"], "r-a1");
        assert_eq!(out["view"]["cards"][0]["agent"]["title"], "fix tests", "short text is not cut");
        assert_eq!(out["omitted"], json!({"done":0,"working":0}));
    }

    #[test]
    fn text_is_cut_before_any_card_is_omitted() {
        let title = "t".repeat(3000);
        let cards: Vec<CardView> = (0..200).map(|i| card(agent(&format!("a{i}"), &title, None))).collect();
        let v = view(cards);
        let at_256 = {
            let mut short = v.clone();
            for c in &mut short.cards {
                c.agent.title = format!("{}{TRUNCATED}", "t".repeat(256));
            }
            view_line(&short, usize::MAX).unwrap().len()
        };
        let out = line(&v, at_256 + 1);
        assert_eq!(keys(&out).len(), 200);
        assert!(out["view"]["cards"][199]["agent"]["title"].as_str().unwrap().ends_with(TRUNCATED));
    }

    #[test]
    fn done_cards_go_first_then_working_cards_and_requests_never() {
        let mut cards = vec![card(agent("r1", "needs you", None))];
        cards.extend((0..100).map(|i| quiet(&format!("w{i}"), "working", CardGroup::Working)));
        cards.extend((0..100).map(|i| quiet(&format!("d{i}"), "done", CardGroup::Done)));
        let v = view(cards);
        let size = |keep: &dyn Fn(&CardView) -> bool| {
            let mut x = v.clone();
            x.cards.retain(|c| keep(c));
            view_line(&x, usize::MAX).unwrap().len()
        };
        let without_done = size(&|c| c.group != CardGroup::Done);
        let requests_only = size(&|c| c.group == CardGroup::Request);
        let out = line(&v, without_done + 32);
        assert_eq!(out["omitted"], json!({"done":100,"working":0}));
        assert!(keys(&out).iter().all(|k| !k.starts_with("m1/d")));
        let out = line(&v, requests_only + 32);
        assert_eq!(out["omitted"], json!({"done":100,"working":100}));
        assert_eq!(keys(&out), ["m1/r1"]);
    }

    #[test]
    fn a_card_with_a_countdown_is_never_omitted() {
        let mut counting = quiet("d0", "done", CardGroup::Done);
        counting.action = Some(acd_client::view::ActionView {
            id: "x".into(),
            card_key: "m1/d0".into(),
            label: "Reply".into(),
            detail: None,
            fires_at_ms: 0.,
        });
        let mut cards = vec![card(agent("r1", "needs you", None)), counting];
        cards.extend((1..100).map(|i| quiet(&format!("d{i}"), "done", CardGroup::Done)));
        let v = view(cards);
        let mut two = v.clone();
        two.cards.truncate(2);
        let out = line(&v, view_line(&two, usize::MAX).unwrap().len() + 32);
        assert_eq!(keys(&out), ["m1/r1", "m1/d0"]);
        assert_eq!(out["omitted"]["done"], 99);
    }

    #[test]
    fn needs_you_text_is_cut_further_but_no_request_is_dropped() {
        let cards: Vec<CardView> = (0..50).map(|i| card(agent(&format!("a{i}"), &"t".repeat(1000), None))).collect();
        let v = view(cards);
        let mut at_100 = v.clone();
        for c in &mut at_100.cards {
            c.agent.title = format!("{}{TRUNCATED}", "t".repeat(100));
        }
        let out = line(&v, view_line(&at_100, usize::MAX).unwrap().len() + 1);
        assert_eq!(keys(&out).len(), 50);
        let title = out["view"]["cards"][0]["agent"]["title"].as_str().unwrap();
        assert!(title.ends_with(TRUNCATED) && title.chars().count() <= 100 + TRUNCATED.chars().count());
        assert!(view_line(&v, 200).is_none(), "nothing is sent rather than dropping a request");
        assert_eq!(parse(&view_or_overflow_line(&v, 200)), json!({"type":"view_overflow","needs_you":50}));
    }

    #[test]
    fn ids_and_enums_are_never_cut() {
        let long_id = "x".repeat(500);
        let v = view(vec![card(agent(&long_id, &"t".repeat(5000), None))]);
        let out = line(&v, 4000);
        assert_eq!(out["view"]["cards"][0]["agent"]["id"].as_str().unwrap(), long_id);
        assert_eq!(out["view"]["cards"][0]["agent"]["state"], "needs_you");
    }

    #[test]
    fn settings_are_bounded_too() {
        use acd_client::settings::{EndpointConfig, SecretStatus};
        use acd_client::view::{MachineEntryView, PairingState};
        let machines = (0..100)
            .map(|i| MachineEntryView {
                key: format!("{i:08}-0000-4000-8000-000000000000"),
                name: "n".repeat(64),
                endpoint: EndpointConfig::Tcp { host: "h".repeat(200), port: 1 },
                pairing: PairingState::Paired,
                secret: SecretStatus::Ok,
            })
            .collect();
        let s = SettingsSnapshot { machines, notifications: true, show_idle: false, show_subagents: false, show_hook_only: false };
        let whole = settings_line(&s, usize::MAX);
        assert!(parse(&whole).get("omitted_machines").is_none());
        let cut = settings_line(&s, whole.len() / 2);
        assert!(cut.len() < whole.len() / 2);
        let out = parse(&cut);
        let kept = out["machines"].as_array().unwrap().len();
        assert_eq!(kept + out["omitted_machines"].as_u64().unwrap() as usize, 100);
    }

    fn machine_views(n: usize) -> Vec<acd_client::view::MachineEntryView> {
        use acd_client::settings::{EndpointConfig, SecretStatus};
        use acd_client::view::{MachineEntryView, PairingState};
        (0..n)
            .map(|i| MachineEntryView {
                key: format!("{i:08}-0000-4000-8000-000000000000"),
                name: "n".repeat(64),
                endpoint: EndpointConfig::Tcp { host: "h".repeat(200), port: 1 },
                pairing: PairingState::Paired,
                secret: SecretStatus::Ok,
            })
            .collect()
    }

    #[test]
    fn settings_over_the_limit_with_no_machines_left_report_the_real_count() {
        let snap = |machines| SettingsSnapshot { machines, notifications: true, show_idle: false, show_subagents: false, show_hook_only: false };
        // No machines to begin with: nothing was omitted, whatever the limit.
        let none = parse(&settings_line(&snap(vec![]), 10));
        assert!(none.get("omitted_machines").is_none(), "{none}");
        assert_eq!(none["machines"].as_array().unwrap().len(), 0);
        // Too small for any machine: all are left out and all are counted.
        let all = parse(&settings_line(&snap(machine_views(3)), 10));
        assert_eq!(all["machines"].as_array().unwrap().len(), 0);
        assert_eq!(all["omitted_machines"], 3);
    }

    #[test]
    fn every_agent_string_key_is_kept_or_named_as_cuttable_text() {
        // A new string field on Agent, Request, Option or Question fails here
        // until it is put in KEEP (never cut) or CUTTABLE (cut to fit).
        const CUTTABLE: &[&str] = &[
            "title", "cwd", "project", "last_message", "parent_title", "detail", "label", "description",
            "raw_screen", "header", "text",
        ];
        let a: acd_protocol::Agent = serde_json::from_value(json!({
            "id":"a","machine_id":"m","source":"herdr","kind":"claude","title":"t","cwd":"/c","project":"p",
            "state":"needs_you","state_since":"2026-10-08T00:00:00Z","failed":false,"last_message":"lm",
            "can_send":true,"role":"subagent","parent_title":"pt",
            "pending":{"id":"r","revision":1,"agent_id":"a","kind":"question","status":"open","answer_path":"hook",
                "title":"T","detail":"d",
                "options":[{"id":"o","label":"l","description":"ds","effect":"allow"}],
                "questions":[{"id":"q","header":"h","text":"qq","multi_select":false,"options":[{"id":"o","label":"l","description":"ds","effect":"choose"}]}],
                "free_text":true,"raw_screen":"rs","opened_at":"2026-10-08T00:00:00Z"}
        })).unwrap();
        fn keys(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(m) => {
                    for (k, v) in m {
                        if v.is_string() {
                            out.push(k.clone());
                        }
                        keys(v, out);
                    }
                }
                Value::Array(items) => items.iter().for_each(|i| keys(i, out)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        keys(&serde_json::to_value(&a).unwrap(), &mut found);
        assert!(found.len() > 15, "the sample agent is not fully populated: {found:?}");
        for k in found {
            assert!(KEEP.contains(&k.as_str()) || CUTTABLE.contains(&k.as_str()), "agent string key {k:?} is neither KEEP nor CUTTABLE");
        }
    }
}
