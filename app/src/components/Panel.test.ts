import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";

afterEach(cleanup);
import Panel from "./Panel.svelte";
import { card, view, fakeApi } from "../lib/fixtures";

test("esc_before_the_view_event_still_cancels", async () => {   // finding 5
  // The view has NO actions yet (the pick's view event has not arrived), but Rust holds a countdown.
  const api = fakeApi({ escapeReturns: true });
  render(Panel, { view: view({ cards: [card({ key: "m/a" })], actions: [] }), api });
  await userEvent.keyboard("{Escape}");
  expect(api.calls).toEqual([["escape"]]);                 // no hide
});

test("Esc with no countdown closes the reply field, then hides", async () => {
  const api = fakeApi({ escapeReturns: false });
  render(Panel, { view: view({ cards: [card({ key: "m/b", can_reply: true })] }), api });
  await userEvent.keyboard("r");
  expect(screen.getByRole("textbox")).toBeTruthy();
  await userEvent.keyboard("{Escape}");
  expect(screen.queryByRole("textbox")).toBeNull();
  await userEvent.keyboard("{Escape}");
  expect(api.calls).toEqual([["escape"], ["escape"], ["hide"]]);
});

test("1 on the selected answerable card calls pick with its request id", async () => {
  const api = fakeApi();
  const c = card({ key: "m/a", answerable: true, selectable_options: ["allow", "deny"] });
  render(Panel, { view: view({ cards: [c] }), api });
  await userEvent.keyboard("1");
  expect(api.calls).toEqual([["pick", "m/a", c.agent.pending!.id, "allow"]]);
});

test("composition Enter in the reply field does not submit", async () => {   // finding 8
  const api = fakeApi();
  render(Panel, { view: view({ cards: [card({ key: "m/b", can_reply: true })] }), api });
  await userEvent.keyboard("r");
  const box = screen.getByRole("textbox");
  box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", isComposing: true, bubbles: true }));
  box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", keyCode: 229, bubbles: true }));
  expect(api.calls.filter((c) => c[0] === "reply")).toEqual([]);
});

test("idle toggle shows the hidden count", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [], hidden_idle: 3 }), api });
  await userEvent.click(screen.getByRole("button", { name: /show 3 idle/i }));
  expect(api.calls).toEqual([["setShowIdle", true]]);
});

test("Enter in the reply field sends the text and closes the field", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [card({ key: "m/b", can_reply: true })] }), api });
  await userEvent.keyboard("r");
  await userEvent.keyboard("hello");
  await userEvent.keyboard("{Enter}");
  expect(api.calls).toEqual([["reply", "m/b", "hello"]]);
  expect(screen.queryByRole("textbox")).toBeNull();
});

test("an empty view shows the empty state with the hotkey", () => {
  render(Panel, { view: view(), api: fakeApi() });
  expect(screen.getByText("No agents need you.")).toBeTruthy();
  expect(screen.getByText(/Control\+Option\+Space/)).toBeTruthy();
});
