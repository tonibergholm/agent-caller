import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import Panel from "./Panel.svelte";
import { card, view, fakeApi } from "../lib/fixtures";
import { MAX_REPLY_BYTES } from "../lib/limits";

afterEach(cleanup);

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
  await userEvent.keyboard("hello");
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

test("a rejected pick is shown to the user", async () => {
  const api = fakeApi();
  api.pick = async () => {
    throw "StaleRequest";
  };
  const c = card({ key: "m/a", answerable: true, selectable_options: ["allow"] });
  render(Panel, { view: view({ cards: [c] }), api });
  await userEvent.keyboard("1");
  expect((await screen.findByRole("alert")).textContent).toContain("StaleRequest");
});

test("a rejected markSeen is shown, and the next key clears it", async () => {
  const api = fakeApi();
  api.markSeen = async () => {
    throw "boom";
  };
  render(Panel, { view: view({ cards: [card({ key: "m/b", can_mark_seen: true })] }), api });
  await userEvent.keyboard("e");
  expect((await screen.findByRole("alert")).textContent).toContain("boom");
  await userEvent.keyboard("x");
  expect(screen.queryByRole("alert")).toBeNull();
});

test("the empty-state hint uses the given hotkey", () => {
  render(Panel, { view: view(), api: fakeApi(), hotkey: "Command+Shift+K" });
  expect(screen.getByText(/Command\+Shift\+K/)).toBeTruthy();
});

test("a rejected reply is shown and the field stays open", async () => {
  const api = fakeApi();
  api.reply = async () => {
    throw "agent gone";
  };
  render(Panel, { view: view({ cards: [card({ key: "m/b", can_reply: true })] }), api });
  await userEvent.keyboard("r");
  await userEvent.keyboard("hello");
  await userEvent.keyboard("{Enter}");
  expect((await screen.findByRole("alert")).textContent).toContain("agent gone");
  expect(screen.getByRole("textbox")).toBeTruthy();
});

test("an over-limit reply is not sent and the field stays open", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [card({ key: "m/b", can_reply: true })] }), api });
  await userEvent.keyboard("r");
  const box = screen.getByRole("textbox") as HTMLTextAreaElement;
  await fireEvent.input(box, { target: { value: "x".repeat(MAX_REPLY_BYTES + 1) } });
  await userEvent.keyboard("{Enter}");
  expect(api.calls.filter((c) => c[0] === "reply")).toEqual([]);
  expect(screen.getByRole("textbox")).toBeTruthy();
});

test("a whitespace-only reply is not sent", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [card({ key: "m/b", can_reply: true })] }), api });
  await userEvent.keyboard("r");
  await userEvent.keyboard("   {Enter}");
  expect(api.calls.filter((c) => c[0] === "reply")).toEqual([]);
});

test("a view push does not clear a rejection; the next key does", async () => {
  const api = fakeApi();
  api.markSeen = async () => {
    throw "boom";
  };
  const c = card({ key: "m/b", can_mark_seen: true });
  const { rerender } = render(Panel, { view: view({ cards: [c] }), api });
  await userEvent.keyboard("e");
  expect((await screen.findByRole("alert")).textContent).toContain("boom");
  await rerender({ view: view({ cards: [c], hidden_idle: 1 }), api });
  expect(screen.getByRole("alert").textContent).toContain("boom");
  await userEvent.keyboard("x");
  expect(screen.queryByRole("alert")).toBeNull();
});

test("rejected footer, card-button and countdown-cancel calls are shown", async () => {
  const boom = async () => {
    throw "denied";
  };
  let api = fakeApi();
  api.setShowIdle = boom;
  const r1 = render(Panel, { view: view({ hidden_idle: 2 }), api });
  await userEvent.click(screen.getByRole("button", { name: /show 2 idle/i }));
  expect((await screen.findByRole("alert")).textContent).toContain("denied");
  r1.unmount();

  api = fakeApi();
  api.openSettings = boom;
  const r2 = render(Panel, { view: view(), api });
  await userEvent.click(screen.getByRole("button", { name: "Settings" }));
  expect((await screen.findByRole("alert")).textContent).toContain("denied");
  r2.unmount();

  api = fakeApi();
  api.pick = boom;
  const c = card({ key: "m/a", answerable: true, selectable_options: ["allow"] });
  const r3 = render(Panel, { view: view({ cards: [c] }), api });
  await userEvent.click(screen.getByRole("button", { name: /1\. Yes/ }));
  expect((await screen.findByRole("alert")).textContent).toContain("denied");
  r3.unmount();

  api = fakeApi();
  api.cancel = boom;
  const a = card({
    key: "m/a",
    action: { id: "a1", card_key: "m/a", label: "Yes", detail: null, fires_at_ms: Date.now() + 2000 },
  });
  render(Panel, { view: view({ cards: [a] }), api });
  await userEvent.click(screen.getByRole("button", { name: /sending yes/i }));
  expect((await screen.findByRole("alert")).textContent).toContain("denied");
});
