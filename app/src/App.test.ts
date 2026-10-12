import { cleanup, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, expect, test } from "vitest";
import App from "./App.svelte";
import { card, fakeApi, view } from "./lib/fixtures";
import type { VoiceSettings } from "./lib/voice";

afterEach(cleanup);

test("the panel label loads the view and renders its cards", async () => {
  const api = fakeApi();
  api.getView = async () => view({ cards: [card({ key: "m/a" })] });
  render(App, { api, label: "panel" });
  expect(await screen.findByText("Fix the build")).toBeTruthy();
});

test("the settings label renders the settings window, not the panel", () => {
  render(App, { api: fakeApi(), label: "settings" });
  expect(screen.getByRole("heading").textContent).toBe("Settings");
});

test("a pushed view replaces the loaded one", async () => {
  const api = fakeApi();
  let push: (v: ReturnType<typeof view>) => void = () => {};
  api.onView = async (cb) => {
    push = cb;
    return () => {};
  };
  render(App, { api, label: "panel" });
  expect(await screen.findByText("No agents need you.")).toBeTruthy();
  push(view({ cards: [card({ key: "m/z" })] }));
  expect(await screen.findByText("Fix the build")).toBeTruthy();
});

test("a getView failure is shown", async () => {
  const api = fakeApi();
  api.getView = async () => {
    throw "no backend";
  };
  render(App, { api, label: "panel" });
  expect((await screen.findByRole("alert")).textContent).toContain("no backend");
});

test("the empty-state hint shows the stored hotkey", async () => {
  const api = fakeApi();
  api.getHotkey = async () => "Command+Shift+K";
  render(App, { api, label: "panel" });
  expect(await screen.findByText(/Command\+Shift\+K/)).toBeTruthy();
});

test("the hud label renders the HUD, never the panel", async () => {
  const api = fakeApi();
  api.getView = async () => view({ cards: [card({ key: "m/a" })] });
  render(App, { api, label: "hud" });
  await new Promise((r) => setTimeout(r, 20));
  expect(screen.queryByText("Fix the build")).toBeNull();
  expect(api.presentations).toEqual([]);
  api.hud!({ seq: 1, id: null, hud: { state: "speaking", text: "Nothing needs you." } });
  expect(await screen.findByText("Nothing needs you.")).toBeTruthy();
});

test("the panel shows the typed command field only while voice and its debug input are on", async () => {
  const api = fakeApi({ voice: { enabled: true, debug_input: true } });
  render(App, { api, label: "panel" });
  expect(await screen.findByRole("textbox", { name: "Voice command" })).toBeTruthy();
  api.voiceSettings!({ enabled: false, debug_input: true });
  await waitFor(() => expect(screen.queryByRole("textbox", { name: "Voice command" })).toBeNull());
});

test("a voice settings load that an event overtook is discarded", async () => {
  const api = fakeApi();
  let release: ((s: VoiceSettings) => void) | undefined;
  api.getVoiceSettings = () => new Promise<VoiceSettings>((r) => (release = r));
  render(App, { api, label: "panel" });
  await waitFor(() => expect(api.voiceSettings && release).toBeTruthy());
  api.voiceSettings!({ enabled: true, debug_input: true });
  expect(await screen.findByRole("textbox", { name: "Voice command" })).toBeTruthy();
  release!({ enabled: false, debug_input: false });
  await new Promise((r) => setTimeout(r, 20));
  expect(screen.queryByRole("textbox", { name: "Voice command" })).toBeTruthy();
});
