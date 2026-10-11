import type { Api } from "./ipc";
import type { CardView } from "./bindings/CardView";
import type { InboxView } from "./bindings/InboxView";
import type { MachineEntryView } from "./bindings/MachineEntryView";

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
    tray: { color: "grey", needs_you: 0 },
    actions: [],
    ...overrides,
  };
}

export type FakeApi = Api & { calls: unknown[][] };

export function fakeApi(
  opts: { machines?: MachineEntryView[]; escapeReturns?: boolean } = {},
): FakeApi {
  const calls: unknown[][] = [];
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
  return {
    calls,
    pick: rec("pick", undefined),
    reply: rec("reply", "action-2"),
    answerText: rec("answerText", "action-3"),
    cancel: rec("cancel", true),
    escape: rec("escape", opts.escapeReturns ?? false),
    markSeen: rec("markSeen", undefined),
    setShowIdle: rec("setShowIdle", undefined),
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
  } as FakeApi;
}
