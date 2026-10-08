import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, expect, test } from "vitest";

afterEach(cleanup);
import App from "./App.svelte";
import { card, fakeApi, view } from "./lib/fixtures";

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
