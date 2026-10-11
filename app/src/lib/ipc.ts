import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { InboxView } from "./bindings/InboxView";
import type { MachineEntryView } from "./bindings/MachineEntryView";

export interface Api {
  pick(cardKey: string, requestId: string, optionId: string, revision: number): Promise<void>;
  reply(cardKey: string, text: string): Promise<string>;
  /** A typed answer to a free-text request; starts the undo countdown. */
  answerText(cardKey: string, requestId: string, revision: number, text: string): Promise<string>;
  cancel(actionId: string): Promise<boolean>;
  /** Atomic backend cancel of the newest countdown (D9). */
  escape(): Promise<boolean>;
  markSeen(cardKey: string): Promise<void>;
  setShowIdle(on: boolean): Promise<void>;
  setShowSubagents(on: boolean): Promise<void>;
  setShowHookOnly(on: boolean): Promise<void>;
  hide(): Promise<void>;
  openSettings(): Promise<void>;
  listMachines(): Promise<MachineEntryView[]>;
  importPairingFromClipboard(): Promise<MachineEntryView>;
  renameMachine(key: string, name: string): Promise<void>;
  removeMachine(key: string): Promise<void>;
  retryMachine(key: string): Promise<void>;
  getHotkey(): Promise<string>;
  setHotkey(hotkey: string): Promise<void>;
  getView(): Promise<InboxView>;
  onView(cb: (v: InboxView) => void): Promise<() => void>;
  onShown(cb: () => void): Promise<() => void>;
}

export const tauriApi: Api = {
  pick: (cardKey, requestId, optionId, revision) =>
    invoke("pick_option", { cardKey, requestId, optionId, revision }),
  reply: (cardKey, text) => invoke("send_reply", { cardKey, text }),
  answerText: (cardKey, requestId, revision, text) =>
    invoke("answer_text", { cardKey, requestId, revision, text }),
  cancel: (actionId) => invoke("cancel_action", { actionId }),
  escape: () => invoke("escape"),
  markSeen: (cardKey) => invoke("mark_seen", { cardKey }),
  setShowIdle: (on) => invoke("set_show_idle", { on }),
  setShowSubagents: (on) => invoke("set_show_subagents", { on }),
  setShowHookOnly: (on) => invoke("set_show_hook_only", { on }),
  hide: () => invoke("hide_panel"),
  openSettings: () => invoke("open_settings"),
  listMachines: () => invoke("list_machines"),
  importPairingFromClipboard: () => invoke("import_pairing_from_clipboard"),
  renameMachine: (key, name) => invoke("rename_machine", { key, name }),
  removeMachine: (key) => invoke("remove_machine", { key }),
  retryMachine: (key) => invoke("retry_machine", { key }),
  getHotkey: () => invoke("get_hotkey"),
  setHotkey: (hotkey) => invoke("set_hotkey", { hotkey }),
  getView: () => invoke("get_view"),
  onView: (cb) => listen<InboxView>("inbox://view", (e) => cb(e.payload)),
  onShown: (cb) => listen("panel://shown", () => cb()),
};
