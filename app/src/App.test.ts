import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, expect, test } from "vitest";
import App from "./App.svelte";
import { card, fakeApi, view } from "./lib/fixtures";

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

test("the hud label never renders the panel", async () => {
  const api = fakeApi();
  api.getView = async () => view({ cards: [card({ key: "m/a" })] });
  render(App, { api, label: "hud" });
  await new Promise((r) => setTimeout(r, 20));
  expect(screen.queryByText("Fix the build")).toBeNull();
});
