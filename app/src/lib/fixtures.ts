import type { Api } from "./ipc";
import type { CardView } from "./bindings/CardView";
import type { InboxView } from "./bindings/InboxView";
import type { MachineEntryView } from "./bindings/MachineEntryView";
import type { HudEvent } from "./hud";
import { VOICE_OFF, type Presentation, type VoiceSettings, type VoiceStatus } from "./voice";

export const STATUS_OK: VoiceStatus = {
  unavailable: null,
  streams_partials: true,
  locale: "en_US",
  microphone: "granted",
  speech: null,
  input_monitoring: "granted",
  hold_key_active: true,
  relaunch_for_hold_key: false,
  toggle_error: null,
};

export function card(overrides: Partial<CardView> = {}): CardView {
  const key = overrides.key ?? "m/a";
  const [machineKey, agentId] = [key.split("/")[0], key.slice(key.indexOf("/") + 1)];
  return {
    key,
    machine_key: machineKey,
    machine_name: "Studio",
    agent: {
      id: agentId,
      machine_id: "machine-1",
      source: "hooks",
      kind: "claude",
      title: "Fix the build",
      cwd: "/work/app",
      project: "app",
      state: "needs_you",
      state_since: "2026-10-08T10:00:00Z",
      failed: false,
      last_message: null,
      pending: {
        id: `req-${agentId}`,
        revision: 1,
        agent_id: agentId,
        kind: "permission",
        status: "open",
        answer_path: "hook",
        title: "Run npm test?",
        detail: "Bash(npm test)",
        options: [
          { id: "allow", label: "Yes", description: null, effect: "allow" },
          { id: "deny", label: "No", description: null, effect: "deny" },
          {
            id: "always",
            label: "Yes, always",
            description: "Bash(npm test:*)",
            effect: "allow_always",
          },
          { id: "weird", label: "?", description: null, effect: "unknown" },
        ],
        questions: null,
        free_text: false,
        raw_screen: null,
        opened_at: "2026-10-08T10:00:00Z",
      },
      can_send: true,
      role: "main",
      parent_title: null,
    },
    group: "request",
    live: true,
    offline_since_ms: null,
    answerable: false,
    free_text_answerable: false,
    selectable_options: [],
    can_reply: false,
    can_mark_seen: false,
    busy: false,
    notice: null,
    action: null,
    ...overrides,
  };
}

export function view(overrides: Partial<InboxView> = {}): InboxView {
  return {
    machines: [
      {
        key: "local",
        name: "This Mac",
        machine_id: "machine-1",
        status: { kind: "online" },
        is_local: true,
        duplicate_of: null,
      },
    ],
    cards: [],
    hidden_idle: 0,
    show_idle: false,
    show_subagents: false,
    show_hook_only: false,
    tray: { color: "grey", needs_you: 0 },
    actions: [],
    ...overrides,
  };
}

export type FakeApi = Api & {
  calls: unknown[][];
  /** Presentation publishes, kept out of `calls` so exact-call assertions stay valid. */
  presentations: Presentation[];
  /** HUD state seqs sent to `voiceHudFit`, also kept out of `calls`. */
  fits: number[];
  voiceSelect?: (cardKey: string) => void;
  voiceProbeReport?: (report: string) => void;
  voiceSettings?: (s: VoiceSettings) => void;
  status: VoiceStatus;
  voiceStatusChanged?: () => void;
  hud?: (e: HudEvent) => void;
  hudClear?: (seq: number) => void;
};

export function fakeApi(
  opts: { machines?: MachineEntryView[]; escapeReturns?: boolean; voice?: VoiceSettings; status?: VoiceStatus } = {},
): FakeApi {
  const calls: unknown[][] = [];
  const presentations: Presentation[] = [];
  const rec =
    <T>(name: string, result: T) =>
    async (...args: unknown[]): Promise<T> => {
      calls.push([name, ...args]);
      return result;
    };
  const machine: MachineEntryView = {
    key: "local",
    name: "This Mac",
    endpoint: { kind: "local" },
    pairing: "paired",
  };
  const api = {
    calls,
    pick: rec("pick", undefined),
    reply: rec("reply", "action-2"),
    answerText: rec("answerText", "action-3"),
    cancel: rec("cancel", true),
    escape: rec("escape", opts.escapeReturns ?? false),
    markSeen: rec("markSeen", undefined),
    setShowIdle: rec("setShowIdle", undefined),
    setShowSubagents: rec("setShowSubagents", undefined),
    setShowHookOnly: rec("setShowHookOnly", undefined),
    hide: rec("hide", undefined),
    openSettings: rec("openSettings", undefined),
    listMachines: rec("listMachines", opts.machines ?? []),
    importPairingFromClipboard: rec("importPairingFromClipboard", machine),
    renameMachine: rec("renameMachine", undefined),
    removeMachine: rec("removeMachine", undefined),
    retryMachine: rec("retryMachine", undefined),
    getHotkey: rec("getHotkey", "Control+Option+Space"),
    setHotkey: rec("setHotkey", undefined),
    getView: rec("getView", view()),
    onView: async () => () => {},
    onShown: async () => () => {},
    presentations,
    publishPresentation: async (p: Presentation) => {
      presentations.push(p);
    },
    voiceTextInput: rec("voiceTextInput", undefined),
    getVoiceSettings: async () => opts.voice ?? VOICE_OFF,
    setVoiceSettings: async (s: VoiceSettings) => {
      calls.push(["setVoiceSettings", s]);
      return s;
    },
    onVoiceSettings: async (cb: (s: VoiceSettings) => void) => {
      api.voiceSettings = cb;
      return () => {};
    },
    onVoiceSelect: async (cb: (cardKey: string) => void) => {
      api.voiceSelect = cb;
      return () => {};
    },
    onHud: async (cb: (e: HudEvent) => void) => {
      api.hud = cb;
      return () => {};
    },
    onHudClear: async (cb: (seq: number) => void) => {
      api.hudClear = cb;
      return () => {};
    },
    voiceShown: rec("voiceShown", undefined),
    voiceConfirm: rec("voiceConfirm", undefined),
    voiceChoose: rec("voiceChoose", undefined),
    voiceCancel: rec("voiceCancel", undefined),
    // Kept out of `calls`: sizing is not an action.
    voiceHudFit: async (seq: number) => {
      api.fits.push(seq);
    },
    voiceProbe: rec("voiceProbe", undefined),
    openVoicePrivacy: rec("openVoicePrivacy", undefined),
    status: opts.status ?? STATUS_OK,
    getVoiceStatus: async () => api.status,
    onVoiceStatus: async (cb: () => void) => {
      api.voiceStatusChanged = cb;
      return () => {};
    },
    requestVoicePermission: rec("requestVoicePermission", undefined),
    installSpeechAssets: rec("installSpeechAssets", undefined),
    onVoiceProbe: async (cb: (report: string) => void) => {
      api.voiceProbeReport = cb;
      return () => {};
    },
    fits: [] as number[],
  } as FakeApi;
  return api;
}
