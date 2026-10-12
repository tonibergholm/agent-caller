import { cleanup, render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import Settings from "./Settings.svelte";
import { STATUS_OK, fakeApi, view } from "../lib/fixtures";
import { DEFAULT_VOICE_TOGGLE, VOICE_OFF } from "../lib/voice";
import type { MachineEntryView } from "../lib/bindings/MachineEntryView";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const devbox = (pairing: MachineEntryView["pairing"] = "paired"): MachineEntryView => ({
  key: "m1",
  name: "devbox",
  endpoint: { kind: "tcp", host: "devbox.ts.net", port: 47800 },
  pairing,
});

test("adding a machine never touches a secret in the webview", async () => {
  const api = fakeApi({ machines: [devbox()] });
  const { container } = render(Settings, { api });
  expect(await screen.findByText(/^paired$/i)).toBeTruthy();
  expect(screen.getByText("devbox.ts.net:47800")).toBeTruthy();
  expect(container.querySelector("input[type=password]")).toBeNull();
  await userEvent.click(screen.getByRole("button", { name: /add from clipboard/i }));
  expect(api.calls).toContainEqual(["importPairingFromClipboard"]);
  expect(screen.getByText(/acd pair/)).toBeTruthy();
});

test("rename and remove", async () => {
  const api = fakeApi({ machines: [devbox()] });
  render(Settings, { api });
  const input = await screen.findByLabelText("Name for devbox");
  await userEvent.clear(input);
  await userEvent.type(input, "build box");
  await userEvent.click(screen.getByRole("button", { name: "Rename" }));
  expect(api.calls).toContainEqual(["renameMachine", "m1", "build box"]);
  await userEvent.click(screen.getByRole("button", { name: "Remove" }));
  expect(api.calls.some((c) => c[0] === "removeMachine")).toBe(false);
  await userEvent.click(screen.getByRole("button", { name: "Confirm remove" }));
  expect(api.calls).toContainEqual(["removeMachine", "m1"]);
  expect(screen.queryByRole("button", { name: "Confirm remove" })).toBeNull();
});

test("a double click on Remove lands on Cancel and removes nothing", async () => {
  const api = fakeApi({ machines: [devbox()] });
  render(Settings, { api });
  const remove = await screen.findByRole("button", { name: "Remove" });
  const row = remove.closest("li")!;
  const index = [...row.querySelectorAll("button")].indexOf(remove as HTMLButtonElement);
  await userEvent.click(remove);
  const same = row.querySelectorAll("button")[index] as HTMLButtonElement;
  expect(same.textContent).toBe("Cancel");
  expect(document.activeElement).toBe(same);
  expect(row.querySelector("[aria-live]")?.textContent).toBe("Remove devbox?");
  await userEvent.click(same);
  expect(api.calls.some((c) => c[0] === "removeMachine")).toBe(false);
  expect(screen.getByRole("button", { name: "Remove" })).toBeTruthy();
});

test("cancelling the inline remove confirmation sends nothing", async () => {
  const api = fakeApi({ machines: [devbox()] });
  render(Settings, { api });
  await userEvent.click(await screen.findByRole("button", { name: "Remove" }));
  await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(screen.getByRole("button", { name: "Remove" })).toBeTruthy();
  expect(api.calls.some((c) => c[0] === "removeMachine")).toBe(false);
});

test("only one machine is pending remove confirmation", async () => {
  const api = fakeApi({
    machines: [
      { ...devbox(), key: "a", name: "A" },
      { ...devbox(), key: "b", name: "B" },
    ],
  });
  render(Settings, { api });
  const [first, second] = await screen.findAllByRole("button", { name: "Remove" });
  await userEvent.click(first);
  await userEvent.click(second);
  expect(screen.getAllByRole("button", { name: "Confirm remove" }).length).toBe(1);
  expect(api.calls.some((c) => c[0] === "removeMachine")).toBe(false);
});

test("Retry is hidden for an online machine and shown otherwise", async () => {
  const api = fakeApi({ machines: [{ ...devbox(), key: "m1" }] });
  api.getView = async () => view({ machines: [{ key: "m1", name: "devbox", machine_id: "x", status: { kind: "online" }, is_local: false, duplicate_of: null }] });
  const { unmount } = render(Settings, { api });
  await screen.findByText("online");
  expect(screen.queryByRole("button", { name: "Retry" })).toBeNull();
  unmount();
  const api2 = fakeApi({ machines: [devbox()] });
  api2.getView = async () => view({ machines: [{ key: "m1", name: "devbox", machine_id: "x", status: { kind: "offline", since_ms: 0, error: "refused" }, is_local: false, duplicate_of: null }] });
  render(Settings, { api: api2 });
  await screen.findByText("offline: refused");
  expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();
});

test("all three pairing states are shown, unreadable suggests re-pairing", async () => {
  const api = fakeApi({
    machines: [
      { ...devbox("paired"), key: "a", name: "A" },
      { ...devbox("not_paired"), key: "b", name: "B" },
      { ...devbox("unreadable"), key: "c", name: "C" },
    ],
  });
  render(Settings, { api });
  expect(await screen.findByText("paired")).toBeTruthy();
  expect(screen.getByText("not paired")).toBeTruthy();
  expect(screen.getByText(/credentials unreadable/i)).toBeTruthy();
  expect(screen.getByText(/re-pair/i)).toBeTruthy();
});

test("the local machine shows acd.sock and has no Remove button", async () => {
  const api = fakeApi({
    machines: [{ key: "local", name: "This Mac", endpoint: { kind: "local" }, pairing: "paired" }],
  });
  render(Settings, { api });
  expect(await screen.findByText("This Mac (acd.sock)")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Remove" })).toBeNull();
});

test("machine status text comes from the view", async () => {
  const api = fakeApi({ machines: [devbox()] });
  api.getView = async () =>
    view({
      machines: [
        { key: "m1", name: "devbox", machine_id: null, status: { kind: "auth_failed", message: "bad token" }, is_local: false, duplicate_of: null },
      ],
    });
  render(Settings, { api });
  expect(await screen.findByText(/auth failed: bad token/)).toBeTruthy();
});

test("the hotkey field loads the stored value and the rejection is shown", async () => {
  const api = fakeApi();
  api.setHotkey = async () => {
    throw "shortcut already in use";
  };
  render(Settings, { api });
  const field = await screen.findByLabelText("Hotkey");
  await vi.waitFor(() => expect((field as HTMLInputElement).value).toBe("Control+Option+Space"));
  await userEvent.clear(field);
  await userEvent.type(field, "Command+K");
  await userEvent.click(screen.getByRole("button", { name: /save hotkey/i }));
  expect((await screen.findByRole("alert")).textContent).toContain("shortcut already in use");
});

test("a successful hotkey change calls setHotkey with the typed value", async () => {
  const api = fakeApi();
  render(Settings, { api });
  const field = await screen.findByLabelText("Hotkey");
  await vi.waitFor(() => expect((field as HTMLInputElement).value).toBe("Control+Option+Space"));
  await userEvent.clear(field);
  await userEvent.type(field, "Command+K");
  await userEvent.click(screen.getByRole("button", { name: /save hotkey/i }));
  expect(api.calls).toContainEqual(["setHotkey", "Command+K"]);
  expect(screen.queryByRole("alert")).toBeNull();
});

test("rejected rename, remove, retry, import and show-idle are each shown", async () => {
  const cases: [keyof ReturnType<typeof fakeApi>, string | RegExp, string][] = [
    ["renameMachine", "Rename", "name taken"],
    ["removeMachine", "Remove", "locked"],
    ["retryMachine", "Retry", "no such machine"],
    ["importPairingFromClipboard", /add from clipboard/i, "clipboard empty"],
  ];
  for (const [method, button, reason] of cases) {
    const api = fakeApi({ machines: [devbox()] });
    (api as unknown as Record<string, unknown>)[method] = async () => {
      throw reason;
    };
    const { unmount } = render(Settings, { api });
    await userEvent.click(await screen.findByRole("button", { name: button }));
    if (method === "removeMachine") await userEvent.click(screen.getByRole("button", { name: "Confirm remove" }));
    expect((await screen.findByRole("alert")).textContent).toContain(reason);
    unmount();
  }
  const api = fakeApi();
  api.setShowIdle = async () => {
    throw "nope";
  };
  render(Settings, { api });
  await userEvent.click(await screen.findByRole("checkbox", { name: /show idle/i }));
  expect((await screen.findByRole("alert")).textContent).toContain("nope");
  expect((screen.getByRole("checkbox", { name: /show idle/i }) as HTMLInputElement).checked).toBe(false);
});

test("an empty or blank name is not sent", async () => {
  const api = fakeApi({ machines: [devbox()] });
  render(Settings, { api });
  const input = await screen.findByLabelText("Name for devbox");
  await userEvent.clear(input);
  await userEvent.type(input, "   ");
  await userEvent.click(screen.getByRole("button", { name: "Rename" }));
  expect(api.calls.some((c) => c[0] === "renameMachine")).toBe(false);
  expect((await screen.findByRole("alert")).textContent).toContain("cannot be empty");
});

test("a padded name is trimmed before sending", async () => {
  const api = fakeApi({ machines: [devbox()] });
  render(Settings, { api });
  const input = await screen.findByLabelText("Name for devbox");
  await userEvent.clear(input);
  await userEvent.type(input, "  box  ");
  await userEvent.click(screen.getByRole("button", { name: "Rename" }));
  expect(api.calls).toContainEqual(["renameMachine", "m1", "box"]);
});

test("a slow getHotkey does not overwrite what the user typed", async () => {
  const api = fakeApi();
  let release: (h: string) => void = () => {};
  api.getHotkey = () => new Promise((r) => (release = r));
  render(Settings, { api });
  const field = (await screen.findByLabelText("Hotkey")) as HTMLInputElement;
  await userEvent.type(field, "Command+K");
  release("Control+Option+Space");
  await new Promise((r) => setTimeout(r, 0));
  expect(field.value).toBe("Command+K");
});

test("a changed machine set in a pushed view refreshes the list", async () => {
  const api = fakeApi();
  let push: (v: ReturnType<typeof view>) => void = () => {};
  api.onView = async (cb) => {
    push = cb;
    return () => {};
  };
  let list: MachineEntryView[] = [];
  api.listMachines = async () => list;
  render(Settings, { api });
  await new Promise((r) => setTimeout(r, 0));
  list = [devbox()];
  push(view({ machines: [{ key: "m1", name: "devbox", machine_id: null, status: { kind: "online" }, is_local: false, duplicate_of: null }] }));
  expect(await screen.findByText("devbox.ts.net:47800")).toBeTruthy();
});

test("a successful import names the new machine and clears a previous error", async () => {
  const api = fakeApi();
  render(Settings, { api });
  await userEvent.click(await screen.findByRole("button", { name: /add from clipboard/i }));
  expect(await screen.findByText("Added This Mac")).toBeTruthy();
});

test("Show subagent threads is off by default and persisted through the api", async () => {
  const api = fakeApi();
  render(Settings, { api });
  const box = (await screen.findByRole("checkbox", { name: /show subagent threads/i })) as HTMLInputElement;
  expect(box.checked).toBe(false);
  await userEvent.click(box);
  expect(api.calls).toContainEqual(["setShowSubagents", true]);
});

test("the subagent toggle reflects the daemon's view and reverts when saving fails", async () => {
  const api = fakeApi();
  api.getView = async () => view({ show_subagents: true });
  api.setShowSubagents = async () => {
    throw "disk full";
  };
  render(Settings, { api });
  const box = (await screen.findByRole("checkbox", { name: /show subagent threads/i })) as HTMLInputElement;
  await vi.waitFor(() => expect(box.checked).toBe(true));
  await userEvent.click(box);
  expect((await screen.findByRole("alert")).textContent).toContain("disk full");
  expect(box.checked).toBe(true);
});

test("the Voice section turns voice and the typed command field on", async () => {
  const api = fakeApi();
  render(Settings, { api });
  const voice = (await screen.findByRole("checkbox", { name: "Voice control" })) as HTMLInputElement;
  const typed = screen.getByRole("checkbox", { name: "Show the typed command field" }) as HTMLInputElement;
  expect(voice.checked).toBe(false);
  expect(typed.disabled).toBe(true);
  await userEvent.click(voice);
  expect(api.calls).toContainEqual(["setVoiceSettings", { ...VOICE_OFF, enabled: true }]);
  expect(typed.disabled).toBe(false);
  await userEvent.click(typed);
  expect(api.calls).toContainEqual(["setVoiceSettings", { ...VOICE_OFF, enabled: true, debug_input: true }]);
});

test("a failed voice change puts the box back and says why", async () => {
  const api = fakeApi();
  api.setVoiceSettings = async () => {
    throw new Error("disk full");
  };
  render(Settings, { api });
  const voice = (await screen.findByRole("checkbox", { name: "Voice control" })) as HTMLInputElement;
  await userEvent.click(voice);
  expect(voice.checked).toBe(false);
  expect((await screen.findByRole("alert")).textContent).toContain("disk full");
});

test("Show hook-only agents is off by default and persisted through the api", async () => {
  const api = fakeApi();
  render(Settings, { api });
  const box = (await screen.findByRole("checkbox", { name: /show hook-only agents/i })) as HTMLInputElement;
  expect(box.checked).toBe(false);
  await userEvent.click(box);
  expect(api.calls).toContainEqual(["setShowHookOnly", true]);
});

test("the hook-only toggle reflects the daemon's view and reverts when saving fails", async () => {
  const api = fakeApi();
  api.getView = async () => view({ show_hook_only: true });
  api.setShowHookOnly = async () => {
    throw "disk full";
  };
  render(Settings, { api });
  const box = (await screen.findByRole("checkbox", { name: /show hook-only agents/i })) as HTMLInputElement;
  await vi.waitFor(() => expect(box.checked).toBe(true));
  await userEvent.click(box);
  expect((await screen.findByRole("alert")).textContent).toContain("disk full");
  expect(box.checked).toBe(true);
});

test("the voice probe shows only with voice and the debug setting on, and lists its reports", async () => {
  for (const voice of [{ ...VOICE_OFF, enabled: true, debug_input: false }, { ...VOICE_OFF, enabled: false, debug_input: true }]) {
    render(Settings, { api: fakeApi({ voice }) });
    await screen.findByRole("checkbox", { name: "Voice control" });
    expect(screen.queryByRole("button", { name: "1. Status" })).toBeNull();
    cleanup();
  }

  const api = fakeApi({ voice: { ...VOICE_OFF, enabled: true, debug_input: true } });
  render(Settings, { api });
  await userEvent.click(await screen.findByRole("button", { name: "1. Status" }));
  expect(api.calls).toContainEqual(["voiceProbe", 1]);
  api.voiceProbeReport!('{"microphone":"granted","step":1}');
  expect(await screen.findByText(/"microphone":"granted"/)).toBeTruthy();
});

const on = { ...VOICE_OFF, enabled: true };

test("each permission shows its state and the action that helps", async () => {
  const api = fakeApi({ voice: on, status: { ...STATUS_OK, microphone: "not_determined", input_monitoring: "denied" } });
  render(Settings, { api });
  await userEvent.click(await screen.findByRole("button", { name: "Ask for Microphone" }));
  expect(api.calls).toContainEqual(["requestVoicePermission", "microphone"]);
  await userEvent.click(screen.getByRole("button", { name: "Open System Settings for Input Monitoring" }));
  expect(api.calls).toContainEqual(["openVoicePrivacy", "input_monitoring"]);
  expect(screen.getByText("Needed only for the hold key. The voice shortcut works without it.")).toBeTruthy();
  expect(screen.queryByText("Speech Recognition")).toBeNull();
});

test("Speech Recognition shows only when the status lists it", async () => {
  const api = fakeApi({ voice: on, status: { ...STATUS_OK, speech: "not_determined" } });
  render(Settings, { api });
  await userEvent.click(await screen.findByRole("button", { name: "Ask for Speech Recognition" }));
  expect(api.calls).toContainEqual(["requestVoicePermission", "speech"]);
});

test("an unavailable Mac says why, and a missing model can be downloaded", async () => {
  render(Settings, { api: fakeApi({ voice: on, status: { ...STATUS_OK, unavailable: "os_too_old" } }) });
  expect(await screen.findByText("Not available on this Mac: voice needs macOS 26 or later.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Download speech model" })).toBeNull();
  cleanup();
  const api = fakeApi({ voice: on, status: { ...STATUS_OK, unavailable: "assets_missing" } });
  render(Settings, { api });
  await userEvent.click(await screen.findByRole("button", { name: "Download speech model" }));
  expect(api.calls).toContainEqual(["installSpeechAssets"]);
});

test("the hold key and the voice shortcut are saved, and an empty shortcut turns it off", async () => {
  const api = fakeApi({ voice: on });
  render(Settings, { api });
  await userEvent.selectOptions(await screen.findByRole("combobox", { name: "Hold key" }), "left_option");
  expect(api.calls).toContainEqual(["setVoiceSettings", { ...on, hold_key: "left_option" }]);
  const input = screen.getByRole("textbox", { name: "Voice shortcut" }) as HTMLInputElement;
  expect(input.value).toBe(DEFAULT_VOICE_TOGGLE);
  await userEvent.clear(input);
  await userEvent.type(input, "Control+Alt+M");
  await userEvent.click(screen.getByRole("button", { name: "Save shortcut" }));
  expect(api.calls).toContainEqual(["setVoiceSettings", { ...on, hold_key: "left_option", toggle_shortcut: "Control+Alt+M" }]);
  await userEvent.clear(input);
  await userEvent.click(screen.getByRole("button", { name: "Save shortcut" }));
  expect(api.calls).toContainEqual(["setVoiceSettings", { ...on, hold_key: "left_option", toggle_shortcut: null }]);
});

test("a refused shortcut says why and the hold key select reverts on failure", async () => {
  const api = fakeApi({ voice: on });
  api.setVoiceSettings = async () => {
    throw new Error("That is the inbox hotkey. Choose another voice shortcut.");
  };
  render(Settings, { api });
  await userEvent.click(await screen.findByRole("button", { name: "Save shortcut" }));
  expect((await screen.findByRole("alert")).textContent).toContain("inbox hotkey");
  const select = screen.getByRole("combobox", { name: "Hold key" }) as HTMLSelectElement;
  await userEvent.selectOptions(select, "fn");
  expect(select.value).toBe("right_option");
});

test("the status reloads when the app says it changed", async () => {
  const api = fakeApi({ voice: on, status: { ...STATUS_OK, microphone: "not_determined" } });
  render(Settings, { api });
  await screen.findByRole("button", { name: "Ask for Microphone" });
  api.status = STATUS_OK;
  api.voiceStatusChanged!();
  await waitFor(() => expect(screen.queryByRole("button", { name: "Ask for Microphone" })).toBeNull());
  expect(screen.getAllByText("Allowed").length).toBeGreaterThan(0);
});

test("nothing permission-related shows before the first status arrives", async () => {
  let release!: (s: typeof STATUS_OK) => void;
  const api = fakeApi({ voice: on });
  api.getVoiceStatus = () => new Promise((r) => (release = r));
  render(Settings, { api });
  await screen.findByRole("combobox", { name: "Hold key" });
  expect(screen.queryByRole("group", { name: "Permissions" })).toBeNull();
  expect(screen.queryByText("Not asked yet")).toBeNull();
  expect(screen.queryByText("The hold key needs Input Monitoring.")).toBeNull();
  release({ ...STATUS_OK, microphone: "not_determined" });
  expect(await screen.findByRole("button", { name: "Ask for Microphone" })).toBeTruthy();
});

test("a shortcut registration error is shown once, with a closing stop", async () => {
  const api = fakeApi({ voice: on, status: { ...STATUS_OK, toggle_error: "Could not register Control+Alt+M: in use" } });
  render(Settings, { api });
  const note = await screen.findByText(/Could not register Control\+Alt\+M/);
  expect(note.textContent?.trim()).toBe("Could not register Control+Alt+M: in use. The previous shortcut stays active.");
});

test("a slower, older status response never replaces a newer one", async () => {
  const api = fakeApi({ voice: on });
  const pending: ((s: typeof STATUS_OK) => void)[] = [];
  api.getVoiceStatus = () => new Promise((r) => pending.push(r));
  render(Settings, { api });
  await waitFor(() => expect(pending.length).toBe(1));
  api.voiceStatusChanged!();
  await waitFor(() => expect(pending.length).toBe(2));
  pending[1]({ ...STATUS_OK, microphone: "denied" });
  await screen.findByText("Denied");
  pending[0]({ ...STATUS_OK, microphone: "not_determined" });
  await new Promise((r) => setTimeout(r, 20));
  expect(screen.queryByRole("button", { name: "Ask for Microphone" })).toBeNull();
  expect(screen.getByText("Denied")).toBeTruthy();
});
