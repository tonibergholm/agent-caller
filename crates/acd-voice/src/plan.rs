//! One voice step, from proposal to outcome (spec §5.6). The app supplies
//! the inbox through `InboxPort`, so acd-voice stays independent of
//! acd-client.
use acd_protocol::{Effect, ErrorCode};
use serde::Serialize;

use crate::fast::{self, Locale, Reply};
use crate::hud::{Hud, OutcomeView, ProposalAction, ProposalView, TargetView};
use crate::refusal::Refusal;
use crate::router::target::Provenance;
use crate::router::{self, Choice, Proposal, Routed, Step};
use crate::safety::Level;
use crate::snapshot::{CardSnap, ConnStamp, Snapshot, Visibility};
use crate::speech;
use crate::text::{VoiceText, clean};

/// Why an action ended without being sent (D1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NotSentReason {
    Offline,
    NotSupported,
    AgentGone,
    AnsweredElsewhere,
    Changed,
    AgentBusy,
    Hidden,
}

/// acd-client's action outcomes in acd-voice's terms (spec §5.6, D1–D3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceOutcome {
    Fired,
    Delivered,
    Rejected { code: ErrorCode },
    Uncertain,
    Cancelled,
    NotSent { reason: NotSentReason },
}

/// What a step was planned on (spec §5.6): the connection, the agent, and
/// its pending request (`None`: there was none). acd-client checks all of
/// it against the live machine under the lock that records the action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Guard {
    pub conn: ConnStamp,
    pub agent_id: String,
    pub request: Option<(String, u64)>,
    /// A send whose target the transcript named while it was send-only
    /// (D7): it may still reach an idle agent the panel hides.
    pub named_send_only: bool,
}

/// One client operation with its guard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Enqueue {
    Pick { card: String, option_id: String, guard: Guard },
    AnswerText { card: String, text: VoiceText, guard: Guard },
    Reply { card: String, text: VoiceText, guard: Guard },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EnqueueError {
    #[error("the request changed")]
    Stale,
    #[error("the request cannot be answered here")]
    NotAnswerable,
    #[error("a countdown is already running for this card")]
    AlreadyCounting,
    #[error("the text was refused")]
    BadText,
    #[error("replies are not possible for this agent now")]
    CannotReply,
    #[error("the connection changed")]
    ConnectionChanged,
    #[error("unknown card")]
    UnknownCard,
    #[error("the agent is hidden now")]
    Hidden,
}

/// The inbox operations voice uses; acd-voice-client implements it over
/// acd-client. `enqueue` is the authoritative live check.
pub trait InboxPort: Send + Sync {
    /// Starts the normal 2 s countdown if the guard still holds; returns
    /// the action id.
    fn enqueue(&self, op: Enqueue) -> Result<String, EnqueueError>;
    /// Cancels by action id; false once the countdown has ended.
    fn cancel(&self, action_id: &str) -> bool;
}

/// What the app shows, and a card to select in the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reaction {
    pub hud: Hud,
    pub select: Option<String>,
}

fn show(hud: Hud) -> Reaction {
    Reaction { hud, select: None }
}

/// What the trigger did to a running voice countdown (D15).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Prior {
    #[default]
    Nothing,
    Cancelled,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Idle,
    /// `id` is what the HUD's Esc names (shares the proposal counter).
    Thinking(u64),
    /// A countdown step on its way to the screen; nothing is enqueued until
    /// the HUD acknowledges proposal `id` (D22).
    Presenting { proposal: Proposal, id: u64 },
    Confirm { proposal: Proposal, id: u64, shown: bool },
    WhichOne(Choice, u64),
    Counting { proposal: Proposal, action_id: String, id: u64 },
    Sending { proposal: Proposal, action_id: String },
    Shown,
}

#[derive(Debug, Default)]
pub struct Executor {
    /// Grows forever, also across `reset` (B4).
    utterance: u64,
    /// Grows forever, also across `reset` (B4).
    proposals: u64,
    state: State,
    snapshot: Option<Snapshot>,
    transcript: Option<VoiceText>,
    prior: Prior,
    /// The trigger cancelled a countdown (spec §4.4). Read by the next
    /// `begin`, so that utterance's "cancel" reports it, or by `resume`.
    trigger_cancelled: bool,
}

impl Executor {
    pub fn new() -> Self {
        Self::default()
    }

    /// The voice action whose outcomes matter now.
    pub fn current_action(&self) -> Option<&str> {
        match &self.state {
            State::Counting { action_id, .. } | State::Sending { action_id, .. } => Some(action_id),
            _ => None,
        }
    }

    /// A new utterance (the trigger, or the typed command). A running voice
    /// countdown is cancelled first (spec §4.4); the snapshot freezes.
    ///
    /// `Err` holds the HUD for a refused utterance, which is not routed.
    pub fn begin(
        &mut self,
        port: &dyn InboxPort,
        snapshot: Snapshot,
        transcript: VoiceText,
        locale: Locale,
    ) -> Result<(u64, Reaction), Box<Reaction>> {
        self.prior = if std::mem::take(&mut self.trigger_cancelled) { Prior::Cancelled } else { Prior::Nothing };
        match std::mem::take(&mut self.state) {
            State::Counting { action_id, .. } if port.cancel(&action_id) => self.prior = Prior::Cancelled,
            // A fired step waits for its result: refuse the utterance and
            // keep the step, its snapshot and transcript, so the late result
            // still shows (final review B4). A "cancel" is too late.
            State::Counting { proposal, action_id, .. } | State::Sending { proposal, action_id } => {
                self.state = State::Sending { proposal, action_id };
                let refusal = match fast::parse_reply(transcript.expose(), locale) {
                    Some(Reply::Cancel) => Refusal::AlreadySent,
                    _ => Refusal::StillSending,
                };
                return Err(Box::new(show(Hud::Error { refusal, transcript: None })));
            }
            _ => {}
        }
        self.utterance += 1;
        let id = self.next_proposal();
        self.state = State::Thinking(id);
        self.snapshot = Some(snapshot);
        self.transcript = Some(transcript.clone());
        Ok((self.utterance, show(Hud::Thinking { transcript, id })))
    }

    /// The router's result for utterance `n`; stale results are dropped.
    pub fn routed(&mut self, n: u64, routed: Routed) -> Option<Reaction> {
        if n != self.utterance || !matches!(self.state, State::Thinking(_)) {
            return None;
        }
        Some(self.take(routed))
    }

    /// While the HUD waits for input, an utterance answers it (D21):
    /// "confirm" confirms a shown proposal, an ordinal or number chooses,
    /// "cancel" cancels. Anything else leaves the waiting step as it is.
    /// `None` when nothing waits; the utterance is then a new step.
    pub fn reply(&mut self, port: &dyn InboxPort, transcript: &VoiceText, locale: Locale) -> Option<Reaction> {
        let which_one = match self.state {
            State::Confirm { .. } => false,
            State::WhichOne(..) => true,
            _ => return None,
        };
        let reaction = match fast::parse_reply(transcript.expose(), locale) {
            Some(Reply::Confirm) if !which_one => self.confirm(port, None),
            Some(Reply::Choose(i)) if which_one => self.choose(None, i),
            Some(Reply::Cancel) => self.cancel(port, None),
            _ => None,
        };
        Some(reaction.unwrap_or_else(|| self.waiting()))
    }

    /// The HUD rendered proposal `id` (D22). A countdown step starts its
    /// countdown only now; a confirm step may be confirmed from now on.
    pub fn shown(&mut self, port: &dyn InboxPort, id: u64) -> Option<Reaction> {
        match std::mem::take(&mut self.state) {
            State::Presenting { proposal, id: pid } if pid == id => Some(self.enqueue(port, proposal, id)),
            State::Confirm { proposal, id: pid, .. } if pid == id => {
                self.state = State::Confirm { proposal, id, shown: true };
                None
            }
            other => {
                self.state = other;
                None
            }
        }
    }

    /// The HUD did not show proposal `id`: the event failed, or no
    /// acknowledgement came in time. Fail closed (D22).
    pub fn display_failed(&mut self, id: u64) -> Option<Reaction> {
        let waiting = match &self.state {
            State::Presenting { id: pid, .. } => *pid == id,
            State::Confirm { id: pid, shown, .. } => *pid == id && !shown,
            _ => false,
        };
        waiting.then(|| self.error(Refusal::DisplayFailed))
    }

    /// Enter or a click (with the id the HUD shows), or a spoken "confirm"
    /// (`None`). Only a proposal the HUD acknowledged can be confirmed.
    pub fn confirm(&mut self, port: &dyn InboxPort, id: Option<u64>) -> Option<Reaction> {
        match std::mem::take(&mut self.state) {
            State::Confirm { proposal, id: pid, shown: true } if id.is_none_or(|i| i == pid) => {
                Some(self.enqueue(port, proposal, pid))
            }
            other => {
                self.state = other;
                None
            }
        }
    }

    /// A digit or a click on candidate `index` (zero-based). The pick is
    /// bound against the snapshot frozen at `begin` for this utterance and
    /// never a newer one, so the bound step names the request the user
    /// heard; `enqueue` then checks it against the live inbox.
    ///
    /// `id` is the list the HUD rendered (`choice_id`); a pick for a list
    /// since replaced is ignored. `None` (a spoken pick) names the current one.
    pub fn choose(&mut self, id: Option<u64>, index: usize) -> Option<Reaction> {
        let routed = match (&self.state, &self.snapshot) {
            (State::WhichOne(choice, cid), Some(snapshot))
                if index < choice.candidates.len() && id.is_none_or(|i| i == *cid) =>
            {
                router::bind(snapshot, choice, index)
            }
            _ => return None,
        };
        let thinking = self.next_proposal();
        self.state = State::Thinking(thinking);
        Some(self.take(routed))
    }

    /// Esc in the inbox panel: cancels the step only while it is thinking.
    /// Anything later waits on the HUD, which has key for it.
    pub fn cancel_thinking(&mut self, port: &dyn InboxPort) -> Option<Reaction> {
        match self.state {
            State::Thinking(id) => self.cancel(port, Some(id)),
            _ => None,
        }
    }

    /// The trigger was pressed (spec §4.4, step-2 plan S7). A voice
    /// countdown, or a countdown step not yet shown, is cancelled at once.
    /// A step still thinking is dropped. A step that waits for input
    /// (confirm, which-one) stays and is returned, to show under the
    /// Listening state. A fired step keeps waiting for its outcome.
    ///
    /// Returns the recording's id, from the counter every HUD input id
    /// comes from, so the HUD's Cancel can name it.
    pub fn listen(&mut self, port: &dyn InboxPort) -> (u64, Option<Hud>) {
        match std::mem::take(&mut self.state) {
            State::Counting { action_id, .. } if port.cancel(&action_id) => self.trigger_cancelled = true,
            State::Counting { proposal, action_id, .. } | State::Sending { proposal, action_id } => {
                self.state = State::Sending { proposal, action_id };
            }
            State::Presenting { .. } => self.trigger_cancelled = true,
            State::Thinking(_) => self.utterance += 1,
            other => self.state = other,
        }
        let over = match &self.state {
            State::Confirm { .. } | State::WhichOne(..) => Some(self.waiting().hud),
            _ => None,
        };
        (self.next_proposal(), over)
    }

    /// The recording ended with nothing to route: too short, silent,
    /// empty, or cancelled (plan S8).
    pub fn resume(&mut self) -> Reaction {
        if std::mem::take(&mut self.trigger_cancelled) {
            return self.done(None, OutcomeView::Cancelled);
        }
        match &self.state {
            State::Counting { proposal, .. } | State::Sending { proposal, .. } => {
                show(Hud::Sending { proposal: self.view(proposal), too_late_to_cancel: false })
            }
            _ => self.waiting(),
        }
    }

    /// Whether no step is active and a notice may take the HUD.
    pub fn is_idle(&self) -> bool {
        matches!(self.state, State::Idle | State::Shown)
    }

    /// The recording failed. A waiting step ends; a fired step keeps
    /// waiting for its outcome.
    pub fn listen_failed(&mut self, refusal: Refusal) -> Reaction {
        self.trigger_cancelled = false;
        if matches!(self.state, State::Counting { .. } | State::Sending { .. }) {
            return show(Hud::Error { refusal, transcript: None });
        }
        self.error(refusal)
    }

    /// Esc or a click on Cancel. A countdown is cancelled by its id (spec
    /// §5.6); a step already sent cannot be undone.
    ///
    /// `id` is the state the HUD rendered (its `proposal_id`, `choice_id`
    /// or thinking `id`); a cancel for a state since replaced is ignored.
    /// `None` (spoken, or global) cancels whatever is current.
    pub fn cancel(&mut self, port: &dyn InboxPort, id: Option<u64>) -> Option<Reaction> {
        if id.is_some() && id != self.input_id() {
            return None;
        }
        let reaction = match std::mem::take(&mut self.state) {
            State::Presenting { proposal, .. } | State::Confirm { proposal, .. } => {
                self.done(Some(&proposal), OutcomeView::Cancelled)
            }
            State::WhichOne(..) | State::Thinking(_) => self.done(None, OutcomeView::Cancelled),
            State::Counting { proposal, action_id, .. } if port.cancel(&action_id) => {
                self.done(Some(&proposal), OutcomeView::Cancelled)
            }
            State::Counting { proposal, action_id, .. } | State::Sending { proposal, action_id } => {
                let view = self.view(&proposal);
                self.state = State::Sending { proposal, action_id };
                show(Hud::Sending { proposal: view, too_late_to_cancel: true })
            }
            State::Idle | State::Shown => show(Hud::Hidden),
        };
        Some(reaction)
    }

    /// An outcome from acd-client's stream.
    pub fn outcome(&mut self, id: &str, outcome: VoiceOutcome) -> Option<Reaction> {
        if self.current_action() != Some(id) {
            return None;
        }
        let (proposal, action_id) = match std::mem::take(&mut self.state) {
            State::Counting { proposal, action_id, .. } | State::Sending { proposal, action_id } => (proposal, action_id),
            other => {
                self.state = other;
                return None;
            }
        };
        let view = match outcome {
            VoiceOutcome::Fired => {
                let view = self.view(&proposal);
                self.state = State::Sending { proposal, action_id };
                return Some(show(Hud::Sending { proposal: view, too_late_to_cancel: false }));
            }
            VoiceOutcome::Delivered => OutcomeView::Delivered,
            VoiceOutcome::Rejected { code } => OutcomeView::Rejected { code },
            VoiceOutcome::Uncertain => OutcomeView::Uncertain,
            VoiceOutcome::Cancelled => OutcomeView::Cancelled,
            VoiceOutcome::NotSent { reason } => OutcomeView::NotSent { reason },
        };
        Some(self.done(Some(&proposal), view))
    }

    /// The outcome stream lagged. A counting action is cancelled, since its
    /// `Fired` may be lost; if that is too late, or it already fired, its
    /// result is unknown.
    pub fn outcomes_lost(&mut self, port: &dyn InboxPort) -> Option<Reaction> {
        let id = self.current_action()?.to_owned();
        if matches!(self.state, State::Counting { .. }) && port.cancel(&id) {
            return self.outcome(&id, VoiceOutcome::Cancelled);
        }
        self.outcome(&id, VoiceOutcome::Uncertain)
    }

    /// Voice turned off: stop unsent work and drop every voice text held in
    /// memory (spec §6.3). Utterance and proposal ids keep growing, so a
    /// late response or acknowledgement from before matches nothing after.
    pub fn reset(&mut self, port: &dyn InboxPort) {
        if let State::Counting { action_id, .. } = &self.state {
            let _ = port.cancel(action_id);
        }
        self.state = State::Idle;
        self.snapshot = None;
        self.transcript = None;
        self.prior = Prior::Nothing;
        self.trigger_cancelled = false;
        self.utterance += 1;
    }

    fn take(&mut self, routed: Routed) -> Reaction {
        match routed {
            Routed::Refused(refusal) => self.error(refusal),
            Routed::WhichOne(choice) => {
                let id = self.next_proposal();
                let hud = self.which_one_hud(&choice, id);
                self.state = State::WhichOne(choice, id);
                show(hud)
            }
            Routed::Proposal(p) => match p.level {
                Level::None => self.immediate(p),
                Level::Countdown => {
                    let id = self.next_proposal();
                    let proposal = self.view(&p);
                    self.state = State::Presenting { proposal: p, id };
                    show(Hud::Countdown { proposal, proposal_id: id, action_id: None })
                }
                Level::Confirm => {
                    let id = self.next_proposal();
                    let proposal = self.view(&p);
                    self.state = State::Confirm { proposal: p, id, shown: false };
                    show(Hud::Confirm { proposal, proposal_id: id })
                }
            },
        }
    }

    /// The id of the state the HUD is showing, which its inputs carry back.
    fn input_id(&self) -> Option<u64> {
        match &self.state {
            State::Thinking(id) | State::WhichOne(_, id) => Some(*id),
            State::Presenting { id, .. } | State::Confirm { id, .. } | State::Counting { id, .. } => Some(*id),
            State::Idle | State::Sending { .. } | State::Shown => None,
        }
    }

    fn next_proposal(&mut self) -> u64 {
        self.proposals += 1;
        self.proposals
    }

    fn immediate(&mut self, p: Proposal) -> Reaction {
        match &p.step {
            Step::Cancel => {
                return match self.prior {
                    Prior::Cancelled => self.done(None, OutcomeView::Cancelled),
                    Prior::Nothing => self.error(Refusal::NothingToCancel),
                };
            }
            Step::Select { card } => {
                self.state = State::Shown;
                return Reaction {
                    hud: Hud::Done { proposal: None, outcome: OutcomeView::Selected },
                    select: Some(card.clone()),
                };
            }
            _ => {}
        }
        let text = self.snapshot.as_ref().and_then(|s| match &p.step {
            Step::Read { card } => s.card(card).and_then(speech::read),
            Step::Summarize { machine } => Some(speech::summarize(s, machine.as_deref())),
            _ => None,
        });
        self.state = State::Shown;
        match text {
            Some(text) => show(Hud::Speaking { text }),
            None => self.error(Refusal::NothingToRead),
        }
    }

    fn enqueue(&mut self, port: &dyn InboxPort, p: Proposal, id: u64) -> Reaction {
        let Some(op) = self.snapshot.as_ref().and_then(|s| operation(s, &p)) else {
            return self.error(Refusal::SnapshotChanged);
        };
        match port.enqueue(op) {
            Ok(action_id) => {
                let proposal = self.view(&p);
                self.state = State::Counting { proposal: p, action_id: action_id.clone(), id };
                show(Hud::Countdown { proposal, proposal_id: id, action_id: Some(action_id) })
            }
            Err(e) => self.error(refusal_for(e)),
        }
    }

    /// The HUD for the step that still waits.
    fn waiting(&self) -> Reaction {
        match &self.state {
            State::Confirm { proposal, id, .. } => show(Hud::Confirm { proposal: self.view(proposal), proposal_id: *id }),
            State::WhichOne(choice, id) => show(self.which_one_hud(choice, *id)),
            _ => show(Hud::Hidden),
        }
    }

    fn which_one_hud(&self, choice: &Choice, choice_id: u64) -> Hud {
        let candidates = match &self.snapshot {
            Some(s) => choice
                .candidates
                .iter()
                .filter_map(|k| s.card(k))
                .map(|c| target_view(s, c))
                .collect(),
            None => Vec::new(),
        };
        Hud::WhichOne { candidates, transcript: self.transcript.clone().unwrap_or_default(), choice_id }
    }

    fn done(&mut self, proposal: Option<&Proposal>, outcome: OutcomeView) -> Reaction {
        let proposal = proposal.map(|p| self.view(p));
        self.state = State::Shown;
        show(Hud::Done { proposal, outcome })
    }

    /// "Didn't understand" shows the transcript (spec §9); nothing else does.
    fn error(&mut self, refusal: Refusal) -> Reaction {
        self.state = State::Shown;
        let transcript = if refusal == Refusal::DidNotUnderstand { self.transcript.clone() } else { None };
        show(Hud::Error { refusal, transcript })
    }

    fn view(&self, p: &Proposal) -> ProposalView {
        let snapshot = self.snapshot.as_ref().expect("a proposal comes from a frozen snapshot");
        proposal_view(snapshot, p)
    }
}

/// The operation with its full guard (spec §5.6, B2).
fn operation(snapshot: &Snapshot, p: &Proposal) -> Option<Enqueue> {
    let frozen = snapshot.card(p.step.card()?)?;
    let guard = Guard {
        conn: frozen.conn?,
        agent_id: frozen.agent_id.clone(),
        request: frozen.pending.as_ref().map(|r| (r.id.clone(), r.revision)),
        named_send_only: matches!(p.step, Step::Send { .. })
            && p.provenance == Some(Provenance::Named)
            && snapshot.visibility(frozen) == Visibility::SendOnly,
    };
    match &p.step {
        Step::Answer { card, request_id, revision, option_id } => {
            let planned = Some((request_id.clone(), *revision));
            (guard.request == planned).then(|| Enqueue::Pick { card: card.clone(), option_id: option_id.clone(), guard })
        }
        Step::AnswerText { card, request_id, revision, text } => {
            let planned = Some((request_id.clone(), *revision));
            (guard.request == planned).then(|| Enqueue::AnswerText { card: card.clone(), text: text.clone(), guard })
        }
        Step::Send { card, text } => Some(Enqueue::Reply { card: card.clone(), text: text.clone(), guard }),
        Step::Cancel | Step::Read { .. } | Step::Summarize { .. } | Step::Select { .. } => None,
    }
}

fn refusal_for(e: EnqueueError) -> Refusal {
    match e {
        EnqueueError::Stale | EnqueueError::ConnectionChanged | EnqueueError::UnknownCard => Refusal::SnapshotChanged,
        EnqueueError::NotAnswerable => Refusal::ReadOnly,
        EnqueueError::AlreadyCounting => Refusal::AlreadyCounting,
        EnqueueError::BadText => Refusal::BadText,
        EnqueueError::CannotReply => Refusal::CannotReply,
        EnqueueError::Hidden => Refusal::Hidden,
    }
}

/// A machine name, title or disambiguator as the HUD shows it: cleaned but
/// never cut, so names that differ late still differ (final review B5).
/// The HUD wraps them.
fn shown(s: &str) -> String {
    clean(s, usize::MAX)
}

/// What identifies a target on the HUD: agent kind, machine and title.
fn shown_as(card: &CardSnap) -> (&'static str, String, String) {
    (speech::kind_name(card.kind), shown(&card.machine_name), shown(&card.title))
}

/// The project, or the card key when the project does not tell `card`
/// apart from the other cards that would look the same. `None` when no
/// other card looks the same.
fn disambiguator(snapshot: &Snapshot, card: &CardSnap) -> Option<String> {
    let me = shown_as(card);
    let twins: Vec<&CardSnap> = snapshot.cards.iter().filter(|c| c.key != card.key && shown_as(c) == me).collect();
    if twins.is_empty() {
        return None;
    }
    let project = |c: &CardSnap| c.project.as_deref().map(shown).filter(|p| !p.is_empty());
    match project(card) {
        Some(p) if twins.iter().all(|t| project(t).as_ref() != Some(&p)) => Some(p),
        _ => Some(shown(&card.key)),
    }
}

fn target_view(snapshot: &Snapshot, card: &CardSnap) -> TargetView {
    let (agent, machine, title) = shown_as(card);
    TargetView {
        card: card.key.clone(),
        machine,
        agent: agent.to_owned(),
        title,
        request: card.pending.as_ref().map(speech::request_label),
        off_screen: snapshot.visibility(card) != Visibility::Visible,
        disambiguator: disambiguator(snapshot, card),
    }
}

fn proposal_view(snapshot: &Snapshot, p: &Proposal) -> ProposalView {
    let card = p
        .step
        .card()
        .and_then(|k| snapshot.card(k))
        .expect("a confirmed or counted step has a card from this snapshot");
    let req = card.pending.as_ref();
    let (action, option, text) = match &p.step {
        Step::Answer { option_id, .. } => {
            (ProposalAction::Answer, req.and_then(|r| r.options.iter().find(|o| &o.id == option_id)), None)
        }
        Step::AnswerText { text, .. } => (ProposalAction::AnswerText, None, Some(text.clone())),
        Step::Send { text, .. } => (ProposalAction::Send, None, Some(text.clone())),
        Step::Cancel | Step::Read { .. } | Step::Summarize { .. } | Step::Select { .. } => {
            unreachable!("safety::level gives these level None, so they are never shown as proposals")
        }
    };
    ProposalView {
        action,
        target: target_view(snapshot, card),
        option: option.map(|o| o.label.clone()),
        effect: option.map(|o| o.effect),
        rule: option
            .filter(|o| o.effect == Effect::AllowAlways)
            .and_then(|o| o.description.clone()),
        question: match action {
            ProposalAction::Send => None,
            ProposalAction::Answer | ProposalAction::AnswerText => {
                req.map(|r| r.question.clone().unwrap_or_else(|| r.detail.clone()))
            }
        },
        text,
        provenance: p.provenance,
        level: p.level,
    }
}
