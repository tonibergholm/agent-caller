import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import Panel from "./Panel.svelte";
import { card, view, fakeApi } from "../lib/fixtures";
import type { InboxView } from "../lib/bindings/InboxView";
import { MAX_ANSWER_BYTES, MAX_REPLY_BYTES } from "../lib/limits";

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
  expect(api.calls).toEqual([["pick", "m/a", c.agent.pending!.id, "allow", 1]]);
});

function atRevision(rev: number) {
  const c = card({ key: "m/a", answerable: true, selectable_options: ["allow", "deny"] });
  c.agent.pending = { ...c.agent.pending!, revision: rev };
  return c;
}

test("a digit pick sends the revision on screen", async () => {
  const api = fakeApi();
  const c = atRevision(7);
  render(Panel, { view: view({ cards: [c] }), api });
  await userEvent.keyboard("2");
  expect(api.calls).toEqual([["pick", "m/a", c.agent.pending!.id, "deny", 7]]);
});

test("a button pick sends the revision on screen", async () => {
  const api = fakeApi();
  const c = atRevision(7);
  render(Panel, { view: view({ cards: [c] }), api });
  await userEvent.click(screen.getByRole("button", { name: /^2 No$/ }));
  expect(api.calls).toEqual([["pick", "m/a", c.agent.pending!.id, "deny", 7]]);
});

test("a pick keeps the revision it was made on when the view moves on afterwards", async () => {
  const api = fakeApi();
  let release: () => void = () => {};
  api.pick = ((...args: unknown[]) => {
    api.calls.push(["pick", ...args]);
    return new Promise<void>((r) => (release = r));
  }) as typeof api.pick;
  const { rerender } = render(Panel, { view: view({ cards: [atRevision(1)] }), api });
  await userEvent.keyboard("1");
  await rerender({ view: view({ cards: [atRevision(2)] }), api });
  release();
  // Effect built from revision 1 as rendered; later views do not rewrite it.
  expect(api.calls).toEqual([["pick", "m/a", "req-a", "allow", 1]]);
  // The next pick is made against what is now rendered.
  await userEvent.click(screen.getByRole("button", { name: /^1 Yes$/ }));
  expect(api.calls[1]).toEqual(["pick", "m/a", "req-a", "allow", 2]);
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
  await userEvent.click(screen.getByRole("button", { name: /^1 Yes$/ }));
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

const machine = (key: string, status: InboxView["machines"][number]["status"]) => ({
  key, name: key, machine_id: key, status, is_local: false, duplicate_of: null,
});

test("each machine gets a status dot; a failed one also shows its reason", () => {
  const v = view({
    machines: [
      machine("local", { kind: "local_unavailable", message: "socket missing" }),
      machine("box", { kind: "offline", since_ms: 0, error: "refused" }),
      machine("ok", { kind: "online" }),
      machine("sync", { kind: "syncing" }),
    ],
  });
  const { container } = render(Panel, { view: v, api: fakeApi() });
  const dots = [...container.querySelectorAll(".dot")];
  expect(dots.map((d) => d.getAttribute("data-dot"))).toEqual(["error", "offline", "online", "syncing"]);
  expect(dots[0].getAttribute("title")).toBe("local: local unavailable: socket missing");
  expect(dots[1].getAttribute("title")).toBe("box: offline: refused");
  expect(dots[2].getAttribute("title")).toBe("ok: online");
  const alerts = [...container.querySelectorAll(".machine-alert")].map((a) => a.textContent?.trim());
  expect(alerts).toEqual(["local: local unavailable: socket missing", "box: offline: refused"]);
});

test("a duplicate machine entry gets no dot and no filter chip", () => {
  const v = view({
    machines: [machine("a", { kind: "online" }), { ...machine("b", { kind: "online" }), duplicate_of: "a" }, machine("c", { kind: "online" })],
  });
  const { container } = render(Panel, { view: v, api: fakeApi() });
  expect(container.querySelectorAll(".dot").length).toBe(2);
  expect(screen.queryByRole("button", { name: /^b/ })).toBeNull();
});

test("read-only cards in the panel name where to answer, per agent source", () => {
  const t3 = card({ key: "m/t3", answerable: false });
  t3.agent.source = "t3";
  t3.agent.pending = { ...t3.agent.pending!, answer_path: "none" };
  const herdr = card({ key: "m/herdr", answerable: false });
  herdr.agent.source = "herdr";
  herdr.agent.pending = { ...herdr.agent.pending!, answer_path: "none" };
  render(Panel, { view: view({ cards: [t3, herdr] }), api: fakeApi() });
  expect(screen.getAllByText("Answer in T3").length).toBe(1);
  expect(screen.getAllByText("Answer in terminal").length).toBe(1);
});

test("a question card shows its text once", () => {
  const c = card({ key: "m/q", answerable: true });
  c.agent.pending = {
    ...c.agent.pending!,
    kind: "question",
    detail: "Which branch?",
    questions: [{ id: "q1", header: null, text: "Which branch?", multi_select: false, options: [] }],
  };
  render(Panel, { view: view({ cards: [c] }), api: fakeApi() });
  expect(screen.getAllByText("Which branch?").length).toBe(1);
});

test("the panel has a drag region and J/K scroll the selection into view", async () => {
  const calls: string[] = [];
  const original = Element.prototype.scrollIntoView;
  Element.prototype.scrollIntoView = function (this: Element, arg?: boolean | ScrollIntoViewOptions) {
    calls.push(`${(this as HTMLElement).dataset.key}:${JSON.stringify(arg)}`);
  };
  try {
    const { container } = render(Panel, {
      view: view({ cards: [card({ key: "m/a" }), card({ key: "m/b" })] }),
      api: fakeApi(),
    });
    expect(container.querySelector("[data-tauri-drag-region]")).not.toBeNull();
    await userEvent.keyboard("j");
    expect(calls.at(-1)).toMatch(/^m\/[ab]:\{"block":"nearest"\}$/);
    const before = calls.length;
    await userEvent.keyboard("j");
    expect(calls.length).toBeGreaterThan(before);
  } finally {
    Element.prototype.scrollIntoView = original;
  }
});

const waiting = (key: string, machineKey = "local", over: Partial<ReturnType<typeof card>> = {}) =>
  card({ key, machine_key: machineKey, machine_name: machineKey, ...over });
const working = (key: string, machineKey = "local") => {
  const c = waiting(key, machineKey, { group: "working" });
  c.agent.state = "working";
  c.agent.pending = null;
  c.agent.title = `Working ${key}`;
  return c;
};
const done = (key: string, machineKey = "local") => {
  const c = waiting(key, machineKey, { group: "done", can_mark_seen: true });
  c.agent.state = "done";
  c.agent.pending = null;
  c.agent.title = `Done ${key}`;
  return c;
};

test("sections list needs-you, working and done with counts; the summary counts the first two", () => {
  const v = view({ cards: [waiting("local/a"), waiting("local/b"), working("local/c"), done("local/d")] });
  const { container } = render(Panel, { view: v, api: fakeApi() });
  expect(container.querySelector(".summary")?.textContent).toBe("2 need you · 1 working");
  const heads = [...container.querySelectorAll("h2.sec")].map((h) => h.textContent?.replace(/\s+/g, " ").trim());
  expect(heads).toEqual(["Needs you 2", "Working 1", "› Done 1"]);
  expect(container.querySelectorAll("section[aria-label='Needs you'] article.card").length).toBe(2);
  expect(container.querySelectorAll("section[aria-label='Working'] .row").length).toBe(1);
});

test("Done is collapsed until opened, and its rows then take J/K and E", async () => {
  const api = fakeApi();
  const v = view({ cards: [waiting("local/a"), done("local/d")] });
  const { container } = render(Panel, { view: v, api });
  expect(screen.queryByText("Done local/d")).toBeNull();
  await userEvent.keyboard("jE");
  expect(api.calls).toEqual([]);
  await userEvent.click(screen.getByRole("button", { name: /^done/i }));
  expect(screen.getByText("Done local/d")).toBeTruthy();
  expect(container.querySelector(".dot[data-state=done]")).not.toBeNull();
  await userEvent.keyboard("je");
  expect(api.calls).toEqual([["markSeen", "local/d"]]);
});

test("a working row is one line with a working dot and no option buttons", () => {
  const { container } = render(Panel, { view: view({ cards: [working("local/c")] }), api: fakeApi() });
  const row = container.querySelector(".row")!;
  expect(row.querySelector(".dot")?.getAttribute("data-state")).toBe("working");
  expect(row.querySelector("button")).toBeNull();
  expect(row.textContent).toContain("Working local/c");
});

test("machine chips filter the cards client-side and keep the choice", async () => {
  const v = view({
    machines: [machine("local", { kind: "online" }), machine("box", { kind: "online" })],
    cards: [waiting("local/a"), waiting("box/b", "box"), working("box/w", "box")],
  });
  const { container, rerender } = render(Panel, { view: v, api: fakeApi() });
  const chip = (name: RegExp) => screen.getByRole("button", { name });
  expect(chip(/^All/).textContent?.replace(/\s+/g, " ").trim()).toBe("All 3");
  expect(chip(/^box/).textContent?.replace(/\s+/g, " ").trim()).toBe("box 2");
  expect(container.querySelectorAll("article.card").length).toBe(2);
  await userEvent.click(chip(/^box/));
  expect(chip(/^box/).getAttribute("aria-pressed")).toBe("true");
  expect(container.querySelectorAll("article.card").length).toBe(1);
  expect(container.querySelector("article.card")?.getAttribute("data-key")).toBe("box/b");
  // The filter survives a view push, and J/K stay inside it.
  await rerender({ view: { ...v, hidden_idle: 1 }, api: fakeApi() });
  expect(container.querySelectorAll("article.card").length).toBe(1);
  await userEvent.click(chip(/^All/));
  expect(container.querySelectorAll("article.card").length).toBe(2);
});

test("a filter on a machine that went away falls back to All", async () => {
  const v = view({
    machines: [machine("local", { kind: "online" }), machine("box", { kind: "online" })],
    cards: [waiting("local/a"), waiting("box/b", "box")],
  });
  const { container, rerender } = render(Panel, { view: v, api: fakeApi() });
  await userEvent.click(screen.getByRole("button", { name: /^box/ }));
  await rerender({ view: { ...v, machines: [v.machines[0]], cards: [v.cards[0]] }, api: fakeApi() });
  expect(container.querySelectorAll("article.card").length).toBe(1);
});

test("J/K only visit the cards the filter leaves on screen", async () => {
  const api = fakeApi();
  const v = view({
    machines: [machine("local", { kind: "online" }), machine("box", { kind: "online" })],
    cards: [
      waiting("local/a", "local", { answerable: true, selectable_options: ["allow"] }),
      waiting("box/b", "box", { answerable: true, selectable_options: ["allow"] }),
    ],
  });
  render(Panel, { view: v, api });
  await userEvent.click(screen.getByRole("button", { name: /^box/ }));
  await userEvent.keyboard("1");
  expect(api.calls).toEqual([["pick", "box/b", "req-b", "allow", 1]]);
});

const freeText = (key = "m/q") => {
  const c = card({ key, answerable: true, free_text_answerable: true });
  c.agent.pending = {
    ...c.agent.pending!,
    kind: "question",
    options: [],
    detail: "Which colour?",
    free_text: true,
    questions: [{ id: "q", header: null, text: "Which colour?", multi_select: false, options: [] }],
  };
  return c;
};

test("Enter in the answer field sends it through answerText with the request on screen", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [freeText()] }), api });
  await userEvent.click(screen.getByRole("textbox", { name: "Type an answer" }));
  await userEvent.keyboard("purple{Enter}");
  expect(api.calls).toEqual([["answerText", "m/q", "req-q", 1, "purple"]]);
  expect((screen.getByRole("textbox", { name: "Type an answer" }) as HTMLTextAreaElement).value).toBe("");
});

test("T focuses the answer field of the selected free-text card", async () => {
  render(Panel, { view: view({ cards: [freeText()] }), api: fakeApi() });
  const box = screen.getByRole("textbox", { name: "Type an answer" });
  expect(document.activeElement).not.toBe(box);
  await userEvent.keyboard("t");
  expect(document.activeElement).toBe(box);
  // Typing a "t" or a digit in the field is text, not a shortcut.
  await userEvent.keyboard("t1");
  expect((box as HTMLTextAreaElement).value).toBe("t1");
});

test("Shift+Enter adds a line and sends nothing", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [freeText()] }), api });
  await userEvent.keyboard("t");
  await userEvent.keyboard("a{Shift>}{Enter}{/Shift}b");
  expect(api.calls).toEqual([]);
  expect((screen.getByRole("textbox", { name: "Type an answer" }) as HTMLTextAreaElement).value).toBe("a\nb");
});

test("composition Enter in the answer field does not send", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [freeText()] }), api });
  await userEvent.keyboard("t");
  await userEvent.keyboard("hello");
  const box = screen.getByRole("textbox", { name: "Type an answer" });
  box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", isComposing: true, bubbles: true }));
  box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", keyCode: 229, bubbles: true }));
  expect(api.calls.filter((c) => c[0] === "answerText")).toEqual([]);
});

test("a blank or over-long answer is not sent", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [freeText()] }), api });
  await userEvent.keyboard("t");
  await userEvent.keyboard("   {Enter}");
  const box = screen.getByRole("textbox", { name: "Type an answer" });
  await fireEvent.input(box, { target: { value: "x".repeat(MAX_ANSWER_BYTES + 1) } });
  await userEvent.keyboard("{Enter}");
  expect(api.calls.filter((c) => c[0] === "answerText")).toEqual([]);
  expect(screen.getByRole("note").textContent).toContain("Too long");
});

test("a rejected answer is shown and the typed text stays", async () => {
  const api = fakeApi();
  api.answerText = async () => {
    throw "StaleRequest";
  };
  render(Panel, { view: view({ cards: [freeText()] }), api });
  await userEvent.keyboard("t");
  await userEvent.keyboard("purple{Enter}");
  expect((await screen.findByRole("alert")).textContent).toContain("StaleRequest");
  expect((screen.getByRole("textbox", { name: "Type an answer" }) as HTMLTextAreaElement).value).toBe("purple");
});

test("the Send button sends the answer too", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [freeText()] }), api });
  await userEvent.type(screen.getByRole("textbox", { name: "Type an answer" }), "purple");
  await userEvent.click(screen.getByRole("button", { name: "Send answer" }));
  expect(api.calls).toEqual([["answerText", "m/q", "req-q", 1, "purple"]]);
});

test("typed text survives a view push", async () => {
  const api = fakeApi();
  const c = freeText();
  const { rerender } = render(Panel, { view: view({ cards: [c] }), api });
  await userEvent.keyboard("t");
  await userEvent.keyboard("pur");
  await rerender({ view: view({ cards: [freeText()], hidden_idle: 2 }), api });
  expect((screen.getByRole("textbox", { name: "Type an answer" }) as HTMLTextAreaElement).value).toBe("pur");
});

test("Esc in the answer field leaves it first; the next Esc closes the panel", async () => {
  const api = fakeApi({ escapeReturns: false });
  render(Panel, { view: view({ cards: [freeText()] }), api });
  const box = screen.getByRole("textbox", { name: "Type an answer" });
  await userEvent.keyboard("t");
  await userEvent.keyboard("{Escape}");
  expect(document.activeElement).not.toBe(box);
  expect(api.calls.filter((c) => c[0] === "hide")).toEqual([]);
  await userEvent.keyboard("{Escape}");
  expect(api.calls.filter((c) => c[0] === "hide").length).toBe(1);
});

test("the countdown on a card is shown with its progress bar and cancels on click", async () => {
  const api = fakeApi();
  const c = card({
    key: "m/a",
    answerable: true,
    selectable_options: ["allow"],
    action: { id: "a1", card_key: "m/a", label: "Yes", detail: null, fires_at_ms: Date.now() + 2000 },
  });
  const { container } = render(Panel, { view: view({ cards: [c] }), api });
  expect(container.querySelector(".undo .bar")).not.toBeNull();
  await userEvent.click(container.querySelector("button.undo")!);
  expect(api.calls).toEqual([["cancel", "a1"]]);
});

test("the footer lists the keyboard hints", () => {
  const { container } = render(Panel, { view: view(), api: fakeApi() });
  const text = container.querySelector("footer .keys")?.textContent?.replace(/\s+/g, " ") ?? "";
  for (const hint of ["J", "K", "move", "answer", "T", "type", "R", "reply", "Esc", "close / undo"]) {
    expect(text).toContain(hint);
  }
});
