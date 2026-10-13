import type { Effect } from "./bindings/Effect";
import type { PermissionKind } from "./voice";

// Mirrors acd_voice::hud and acd_voice::refusal (serde shapes pinned by
// the_hud_json_shape_is_the_frontend_contract).

export type Refusal =
  | "nothing_to_answer"
  | "nothing_to_read"
  | "no_target"
  | "no_such_target"
  | "no_such_option"
  | "ambiguous_option"
  | "read_only"
  | "offline"
  | "cannot_reply"
  | "subagent_hidden"
  | "out_of_view"
  | "filter_hidden_not_named"
  | "target_mismatch"
  | "bad_text"
  | "did_not_understand"
  | "model_unavailable"
  | "model_failed"
  | "model_off"
  | "model_not_ready"
  | "snapshot_changed"
  | "already_counting"
  | "nothing_to_cancel"
  | "already_sent"
  | "not_permitted"
  | "display_failed"
  | "hidden"
  | "still_sending"
  | "microphone_denied"
  | "microphone_unavailable"
  | "speech_denied"
  | "speech_unavailable"
  | "speech_assets_missing"
  | "recording_too_long"
  | "hold_key_off";

export type NotSentReason =
  | "offline"
  | "not_supported"
  | "agent_gone"
  | "answered_elsewhere"
  | "changed"
  | "agent_busy"
  | "hidden";

export type Provenance = "named" | "selected" | "only_pending" | "inferred";
export type Level = "none" | "countdown" | "confirm";

export type TargetView = {
  card: string;
  machine: string;
  agent: string;
  title: string;
  request: string | null;
  off_screen: boolean;
  /** Only when another card would look the same: its project or card key. */
  disambiguator: string | null;
};

export type ProposalView = {
  action: "answer" | "answer_text" | "send";
  target: TargetView;
  option: string | null;
  effect: Effect | null;
  rule: string | null;
  question: string | null;
  text: string | null;
  provenance: Provenance | null;
  level: Level;
};

export type OutcomeView =
  | { kind: "delivered" }
  | { kind: "rejected"; code: string }
  | { kind: "uncertain" }
  | { kind: "cancelled" }
  | { kind: "not_sent"; reason: NotSentReason }
  | { kind: "selected" };

export type Hud =
  | { state: "hidden" }
  | { state: "listening"; id: number; partial: string | null; over: Hud | null }
  | { state: "thinking"; transcript: string; id: number }
  | { state: "confirm"; proposal: ProposalView; proposal_id: number }
  | { state: "which_one"; candidates: TargetView[]; transcript: string; choice_id: number }
  | { state: "countdown"; proposal: ProposalView; proposal_id: number; action_id: string | null }
  | { state: "sending"; proposal: ProposalView; too_late_to_cancel: boolean }
  | { state: "done"; proposal: ProposalView | null; outcome: OutcomeView }
  | { state: "error"; refusal: Refusal; transcript: string | null }
  | { state: "speaking"; text: string; audio: boolean };

/** `id` is the input id of the shown state (`Hud::input_id`): sent back with Esc, a click or a digit. */
export type HudEvent = { seq: number; id: number | null; hud: Hud };

/** Classified messages (spec §9): never a provider body or model text. */
export const refusalText: Record<Refusal, string> = {
  nothing_to_answer: "Nothing to answer.",
  nothing_to_read: "Nothing to read.",
  no_target: "Say which agent.",
  no_such_target: "No agent by that name can take this.",
  no_such_option: "That option is not there.",
  ambiguous_option: "Say the option number.",
  read_only: "This request can't be answered here.",
  offline: "The machine is offline.",
  cannot_reply: "This agent can't take a message now.",
  subagent_hidden: "Subagent threads are hidden.",
  out_of_view: "That agent is out of view. Only a message that names it can reach it.",
  filter_hidden_not_named: "That agent is filtered out. Say its machine.",
  target_mismatch: "Not sure which agent. Nothing was sent.",
  bad_text: "That text can't be sent.",
  did_not_understand: "Didn't understand.",
  model_unavailable: "Model unavailable. Simple commands still work.",
  model_failed: "The model did not answer. Simple commands still work.",
  model_off: "Apple Intelligence is off. Simple commands still work.",
  model_not_ready: "The on-device model is not ready yet. Simple commands still work.",
  snapshot_changed: "The request changed. Nothing was sent.",
  already_counting: "A countdown is already running for this card.",
  nothing_to_cancel: "Nothing to cancel.",
  already_sent: "Already sent. It can't be undone.",
  not_permitted: "Voice can't do that.",
  display_failed: "The HUD could not show the step. Nothing was sent.",
  hidden: "That agent is hidden now. Nothing was sent.",
  still_sending: "Still sending the last step.",
  microphone_denied: "Microphone access is off for Agent Caller.",
  microphone_unavailable: "Microphone unavailable.",
  speech_denied: "Speech Recognition is off for Agent Caller.",
  speech_unavailable: "Speech recognition is not available. Nothing was sent.",
  speech_assets_missing: "The English speech model is not installed. Download it in Settings.",
  recording_too_long: "Recording stopped after 60 seconds. Nothing was sent.",
  hold_key_off: "The hold key stopped working. The voice shortcut still works.",
};

const notSentText: Record<NotSentReason, string> = {
  offline: "Offline. Nothing was sent.",
  not_supported: "Not supported for this agent.",
  agent_gone: "The agent is gone.",
  answered_elsewhere: "Answered elsewhere.",
  changed: "The request changed. Nothing was sent.",
  agent_busy: "The agent is busy. Nothing was sent.",
  hidden: "Hidden. Nothing was sent.",
};

export function outcomeText(o: OutcomeView): string {
  switch (o.kind) {
    case "delivered":
      return "Sent.";
    case "rejected":
      return `Not sent: ${o.code.replaceAll("_", " ")}.`;
    case "uncertain":
      return "Check the terminal.";
    case "cancelled":
      return "Cancelled. Nothing was sent.";
    case "not_sent":
      return notSentText[o.reason];
    case "selected":
      return "Selected.";
  }
}

/**
 * States whose keys the HUD acts on when it has key: those waiting for input,
 * and thinking and listening, which Esc cancels after a click on the HUD
 * (spec §4.4, plan S25). Rust's `Hud::wants_key` stays false for listening.
 */
export const wantsKey = (h: Hud): boolean =>
  h.state === "thinking" || h.state === "listening" || h.state === "confirm" || h.state === "which_one" || h.state === "countdown";

/** The proposal the HUD must acknowledge once it is on screen (D22). */
export function awaitingDisplay(h: Hud): number | null {
  if (h.state === "confirm") return h.proposal_id;
  if (h.state === "countdown" && h.action_id === null) return h.proposal_id;
  return null;
}

/** The System Settings pane that fixes a refusal, if one does (plan S19). */
export function privacyPane(r: Refusal): PermissionKind | null {
  switch (r) {
    case "microphone_denied":
      return "microphone";
    case "speech_denied":
      return "speech";
    case "hold_key_off":
      return "input_monitoring";
    default:
      return null;
  }
}
