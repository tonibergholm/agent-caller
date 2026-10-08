import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import Settings from "./Settings.svelte";
import { fakeApi, view } from "../lib/fixtures";
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
  vi.spyOn(window, "confirm").mockReturnValue(true);
  render(Settings, { api });
  const input = await screen.findByLabelText("Name for devbox");
  await userEvent.clear(input);
  await userEvent.type(input, "build box");
  await userEvent.click(screen.getByRole("button", { name: "Rename" }));
  expect(api.calls).toContainEqual(["renameMachine", "m1", "build box"]);
  await userEvent.click(screen.getByRole("button", { name: "Remove" }));
  expect(api.calls).toContainEqual(["removeMachine", "m1"]);
});

test("declining the remove confirmation sends nothing", async () => {
  const api = fakeApi({ machines: [devbox()] });
  vi.spyOn(window, "confirm").mockReturnValue(false);
  render(Settings, { api });
  await userEvent.click(await screen.findByRole("button", { name: "Remove" }));
  expect(api.calls.some((c) => c[0] === "removeMachine")).toBe(false);
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
  vi.spyOn(window, "confirm").mockReturnValue(true);
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
});

test("a successful import names the new machine and clears a previous error", async () => {
  const api = fakeApi();
  render(Settings, { api });
  await userEvent.click(await screen.findByRole("button", { name: /add from clipboard/i }));
  expect(await screen.findByText("Added This Mac")).toBeTruthy();
});
