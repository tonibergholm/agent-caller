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
