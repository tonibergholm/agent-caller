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

/** Mirrors the app's voice::settings::VoiceSettings. */
export type VoiceSettings = { enabled: boolean; debug_input: boolean };

export const VOICE_OFF: VoiceSettings = { enabled: false, debug_input: false };
