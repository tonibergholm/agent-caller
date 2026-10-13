/** Mirrors acd_voice::snapshot::Presentation (voice spec §4.3). */
export type Presentation = {
  epoch: string;
  version: number;
  selected: string | null;
  /** Card keys on screen, in display order. */
  visible: string[];
  machine_filter: string | null;
  done_collapsed: boolean;
};

/** Mirrors the app's voice::settings::HoldKey. */
export type HoldKey = "right_option" | "left_option" | "right_command" | "right_control" | "fn" | "off";

/** Mirrors the app's voice::settings::VoiceSettings. */
export type VoiceSettings = {
  enabled: boolean;
  debug_input: boolean;
  hold_key: HoldKey;
  /** null turns the toggle shortcut off. */
  toggle_shortcut: string | null;
};

export const DEFAULT_VOICE_TOGGLE = "Control+Alt+V";

export const VOICE_OFF: VoiceSettings = {
  enabled: false,
  debug_input: false,
  hold_key: "right_option",
  toggle_shortcut: DEFAULT_VOICE_TOGGLE,
};

/** Mirrors the app's voice::permissions::Permission. */
export type PermissionKind = "microphone" | "speech" | "input_monitoring";

/** Mirrors voice::permissions::PermissionState. */
export type PermissionState = "not_determined" | "granted" | "denied" | "restricted";

/** Mirrors acd_voice::listen::Unavailable. */
export type Unavailable =
  | "checking"
  | "not_mac"
  | "os_too_old"
  | "sdk_too_old"
  | "transcriber_unsupported"
  | "locale_unsupported"
  | "assets_missing";

/** Mirrors acd_voice::router::model::ModelUnavailable. */
export type ModelUnavailable =
  | "not_mac"
  | "os_too_old"
  | "sdk_too_old"
  | "device_not_eligible"
  | "intelligence_off"
  | "not_ready"
  | "unknown";

/** Mirrors acd_voice::router::model::ModelState. */
export type ModelState = { state: "checking" } | { state: "available" } | { state: "unavailable"; reason: ModelUnavailable };

/** Mirrors voice::status::VoiceStatus. */
export type VoiceStatus = {
  unavailable: Unavailable | null;
  streams_partials: boolean;
  locale: string | null;
  microphone: PermissionState;
  /** null when Speech Recognition is not needed. */
  speech: PermissionState | null;
  input_monitoring: PermissionState;
  hold_key_active: boolean;
  relaunch_for_hold_key: boolean;
  /** Why the voice shortcut could not be registered; the old one stays live. */
  toggle_error: string | null;
  /** The on-device intent model; free-form commands need it (spec §9). */
  model: ModelState;
};

/** Spec §9: "Not available on this Mac" with the reason. */
export const unavailableText: Record<Unavailable, string> = {
  checking: "Checking speech recognition on this Mac…",
  not_mac: "Voice needs macOS.",
  os_too_old: "Not available on this Mac: voice needs macOS 26 or later.",
  sdk_too_old: "Not available in this build: it was built without the macOS 26 SDK.",
  transcriber_unsupported: "Not available on this Mac: on-device speech recognition is not supported.",
  locale_unsupported: "Not available on this Mac: English speech recognition is not supported.",
  assets_missing: "The English speech model is not installed.",
};

export const HOLD_KEYS: [HoldKey, string][] = [
  ["right_option", "Right Option"],
  ["left_option", "Left Option"],
  ["right_command", "Right Command"],
  ["right_control", "Right Control"],
  ["fn", "Fn"],
  ["off", "Off"],
];

const modelReason: Record<ModelUnavailable, string> = {
  not_mac: "Free-form commands need macOS.",
  os_too_old: "Free-form commands need macOS 26 or later. Simple commands still work.",
  sdk_too_old: "Free-form commands are not in this build: it was built without the macOS 26 SDK. Simple commands still work.",
  device_not_eligible: "This Mac can't run Apple Intelligence. Simple commands still work.",
  intelligence_off: "Turn on Apple Intelligence in System Settings for free-form commands. Simple commands still work.",
  not_ready: "Apple Intelligence is still getting ready. Simple commands work meanwhile.",
  unknown: "Apple Intelligence is not available. Simple commands still work.",
};

/** Spec §9: the model's availability, with the reason. */
export function modelText(m: ModelState): string {
  switch (m.state) {
    case "checking":
      return "Checking Apple Intelligence…";
    case "available":
      return "Free-form commands use Apple Intelligence on this Mac.";
    case "unavailable":
      return modelReason[m.reason];
  }
}
