import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { tick } from "svelte";
import { afterEach, expect, test, vi } from "vitest";
import Panel from "./Panel.svelte";
import { card, view, fakeApi } from "../lib/fixtures";
import type { MachineView } from "../lib/bindings/MachineView";
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
  await userEvent.click(screen.getByRole("button", { name: /sending “yes”/i }));
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
    expect(container.querySelector("header[data-tauri-drag-region]")).not.toBeNull();
    expect(container.querySelector(".grip[data-tauri-drag-region]")).not.toBeNull();
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
  // The draft stays until the request resolves, so an undo gets it back.
  expect((screen.getByRole("textbox", { name: "Type an answer" }) as HTMLTextAreaElement).value).toBe("purple");
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

test("a subagent row shows its badge and parent title in the meta line", () => {
  const c = working("local/s");
  c.agent.role = "subagent";
  c.agent.parent_title = "Fix the build";
  const { container } = render(Panel, { view: view({ cards: [c], show_subagents: true }), api: fakeApi() });
  expect(container.querySelector(".row .subagent")?.textContent).toBe("subagent");
  expect(container.querySelector(".row .parent")?.textContent).toBe("↳ Fix the build");
});

test("the summary's working count matches the Working section, unknown agents included", () => {
  const unknown = working("local/u");
  unknown.group = "unknown";
  unknown.agent.state = "unknown";
  const { container } = render(Panel, { view: view({ cards: [working("local/w"), unknown] }), api: fakeApi() });
  expect(container.querySelector(".summary")?.textContent).toBe("0 need you · 2 working");
  expect(container.querySelector("h2.sec.working .count")?.textContent).toBe("2");
});

test("a draft belongs to the request it was typed for, not a newer one on the same card", async () => {
  const api = fakeApi();
  const { rerender } = render(Panel, { view: view({ cards: [freeText()] }), api });
  await userEvent.keyboard("t");
  await userEvent.keyboard("old answer");
  const newer = freeText();
  newer.agent.pending = { ...newer.agent.pending!, id: "req-new", revision: 1 };
  await rerender({ view: view({ cards: [newer] }), api });
  const box = screen.getByRole("textbox", { name: "Type an answer" }) as HTMLTextAreaElement;
  expect(box.value).toBe("");
  // The box still has focus: Enter would send the old draft if it had followed the card.
  await userEvent.keyboard("{Enter}");
  expect(api.calls.filter((c) => c[0] === "answerText")).toEqual([]);
});

test("the draft is kept while the answer counts down and is there again after an undo", async () => {
  const api = fakeApi();
  const { rerender } = render(Panel, { view: view({ cards: [freeText()] }), api });
  await userEvent.keyboard("t");
  await userEvent.keyboard("purple{Enter}");
  expect(api.calls).toEqual([["answerText", "m/q", "req-q", 1, "purple"]]);
  const counting = freeText();
  counting.action = { id: "a9", card_key: "m/q", label: "Answer", detail: null, fires_at_ms: Date.now() + 2000 };
  await rerender({ view: view({ cards: [counting] }), api });
  const box = screen.getByRole("textbox", { name: "Type an answer" }) as HTMLTextAreaElement;
  expect(box.value).toBe("purple");
  expect(box.disabled).toBe(true);
  // Undo: the countdown ends without the request resolving.
  await rerender({ view: view({ cards: [freeText()] }), api });
  const again = screen.getByRole("textbox", { name: "Type an answer" }) as HTMLTextAreaElement;
  expect(again.disabled).toBe(false);
  expect(again.value).toBe("purple");
});

test("Enter during the card's own countdown sends nothing", async () => {
  const api = fakeApi();
  const c = freeText();
  c.action = { id: "a9", card_key: "m/q", label: "Answer", detail: null, fires_at_ms: Date.now() + 2000 };
  render(Panel, { view: view({ cards: [c] }), api });
  const box = screen.getByRole("textbox", { name: "Type an answer" }) as HTMLTextAreaElement;
  expect(box.disabled).toBe(true);
  box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
  expect(api.calls.filter((x) => x[0] === "answerText")).toEqual([]);
});

test("the scroll container keeps the selection clear of the sticky bars", () => {
  const { container } = render(Panel, { view: view({ cards: [waiting("local/a")] }), api: fakeApi() });
  const style = container.querySelector("main.panel")?.getAttribute("style") ?? "";
  expect(style).toContain("scroll-padding-top");
  expect(style).toContain("scroll-padding-bottom");
});

test("a card with a countdown stays on screen when Done is collapsed or the filter excludes it", async () => {
  const counting = (key: string, machineKey: string) => {
    const c = done(key, machineKey);
    c.action = { id: "a7", card_key: key, label: "Reply", detail: null, fires_at_ms: Date.now() + 2000 };
    return c;
  };
  const v = view({
    machines: [machine("local", { kind: "online" }), machine("box", { kind: "online" })],
    cards: [waiting("local/a"), counting("box/d", "box"), done("box/e", "box")],
  });
  const { container } = render(Panel, { view: v, api: fakeApi() });
  // Done is collapsed: only the counting row shows.
  expect(screen.getByText("Done box/d")).toBeTruthy();
  expect(screen.queryByText("Done box/e")).toBeNull();
  expect(container.querySelector("button.undo")).not.toBeNull();
  // Filtering to "This Mac" does not hide it either.
  await userEvent.click(screen.getByRole("button", { name: /^local/ }));
  expect(screen.getByText("Done box/d")).toBeTruthy();
  expect(container.querySelector("button.undo")).not.toBeNull();
});

const online = (key: string): MachineView => ({
  key,
  name: key,
  machine_id: key,
  status: { kind: "online" },
  is_local: false,
  duplicate_of: null,
});

test("the panel publishes what voice may target, in display order", async () => {
  const api = fakeApi();
  const cards = [card({ key: "m/a" }), card({ key: "m/d", group: "done" }), card({ key: "m/w", group: "working" })];
  render(Panel, { view: view({ cards }), api });
  await tick();
  const first = api.presentations.at(-1)!;
  expect(first).toMatchObject({ selected: "m/a", visible: ["m/a", "m/w"], machine_filter: null, done_collapsed: true });
  await userEvent.click(screen.getByRole("button", { name: /Done/ }));
  const opened = api.presentations.at(-1)!;
  expect(opened.visible).toEqual(["m/a", "m/w", "m/d"]);
  expect(opened.done_collapsed).toBe(false);
  expect(opened.epoch).toBe(first.epoch);
  expect(opened.version).toBeGreaterThan(first.version);
});

test("the machine filter is published", async () => {
  const api = fakeApi();
  const v = view({ machines: [online("local"), online("box")], cards: [card({ key: "local/a" }), card({ key: "box/b" })] });
  render(Panel, { view: v, api });
  await userEvent.click(screen.getByRole("button", { name: /^box/ }));
  expect(api.presentations.at(-1)).toMatchObject({ machine_filter: "box", visible: ["box/b"] });
});

test("a voice selection selects the card and clears a filter that hides it", async () => {
  const api = fakeApi();
  const v = view({ machines: [online("local"), online("box")], cards: [card({ key: "local/a" }), card({ key: "box/b" })] });
  render(Panel, { view: v, api });
  await userEvent.click(screen.getByRole("button", { name: /^box/ }));
  await tick();
  api.voiceSelect!("local/a");
  await tick();
  expect(api.presentations.at(-1)).toMatchObject({ selected: "local/a", machine_filter: null });
});

test("the typed command field is hidden unless asked for", () => {
  render(Panel, { view: view({ cards: [card()] }), api: fakeApi() });
  expect(screen.queryByRole("textbox", { name: "Voice command" })).toBeNull();
});

test("Enter in the typed command field sends it to voice; its letters are not shortcuts", async () => {
  const api = fakeApi();
  const cards = [card({ key: "m/a", answerable: true, selectable_options: ["allow"] }), card({ key: "m/b" })];
  render(Panel, { view: view({ cards }), api, voiceInput: true });
  const field = screen.getByRole("textbox", { name: "Voice command" }) as HTMLTextAreaElement;
  await userEvent.click(field);
  await userEvent.keyboard("j1 approve{Enter}");
  expect(api.calls).toEqual([["voiceTextInput", "j1 approve"]]);
  expect(field.value).toBe("");
  expect(api.presentations.at(-1)?.selected).toBe("m/a");
});

test("/ focuses the typed command field, and a click focuses it too", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [card()] }), api, voiceInput: true });
  const field = screen.getByRole("textbox", { name: "Voice command" }) as HTMLTextAreaElement;
  await userEvent.keyboard("/");
  expect(document.activeElement).toBe(field);
  expect(field.value).toBe("");
  field.blur();
  await fireEvent.mouseDown(field);
  expect(document.activeElement).toBe(field);
});

test("/ does nothing without the typed command field", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [card()] }), api });
  await userEvent.keyboard("/");
  expect(api.calls).toEqual([]);
});

test("turning the typed command field off drops its draft", async () => {
  const props = { view: view({ cards: [card()] }), api: fakeApi(), voiceInput: true };
  const { rerender } = render(Panel, props);
  await userEvent.type(screen.getByRole("textbox", { name: "Voice command" }), "SECRET draft");
  await rerender({ ...props, voiceInput: false });
  await rerender({ ...props, voiceInput: true });
  expect((screen.getByRole("textbox", { name: "Voice command" }) as HTMLTextAreaElement).value).toBe("");
});

test("Esc in the typed command field leaves the field before it closes the panel", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [card()] }), api, voiceInput: true });
  await userEvent.click(screen.getByRole("textbox", { name: "Voice command" }));
  await userEvent.keyboard("{Escape}");
  expect(api.calls).toEqual([["escape"]]);
  await userEvent.keyboard("{Escape}");
  expect(api.calls).toEqual([["escape"], ["escape"], ["hide"]]);
});

function destructive() {
  const c = card({ key: "m/e", answerable: true, selectable_options: ["go", "abandon"] });
  c.agent.source = "external";
  c.agent.pending = {
    ...c.agent.pending!,
    options: [
      { id: "go", label: "Go", description: null, effect: "choose" },
      { id: "abandon", label: "Abandon", description: null, effect: "choose", confirm: true },
    ],
  };
  return c;
}

test("a confirm option needs two clicks and the first sends nothing", async () => {
  const api = fakeApi();
  const c = destructive();
  render(Panel, { view: view({ cards: [c] }), api });
  await userEvent.click(screen.getByRole("button", { name: /abandon/i }));
  expect(api.calls).toEqual([]);
  await userEvent.click(screen.getByRole("button", { name: /tap again to abandon/i }));
  expect(api.calls).toEqual([["pick", "m/e", c.agent.pending!.id, "abandon", 1]]);
  expect(screen.queryByRole("button", { name: /tap again/i })).toBeNull();
});

test("the keyboard and the mouse share one arm", async () => {
  const api = fakeApi();
  const c = destructive();
  render(Panel, { view: view({ cards: [c] }), api });
  await userEvent.keyboard("2");
  expect(api.calls).toEqual([]);
  expect(screen.getByRole("button", { name: /tap again to abandon/i })).toBeTruthy();
  await userEvent.click(screen.getByRole("button", { name: /tap again to abandon/i }));
  expect(api.calls).toEqual([["pick", "m/e", c.agent.pending!.id, "abandon", 1]]);
});

test("an armed option disarms when the request changes", async () => {
  const api = fakeApi();
  const c = destructive();
  const { rerender } = render(Panel, { view: view({ cards: [c] }), api });
  await userEvent.keyboard("2");
  expect(screen.getByRole("button", { name: /tap again to abandon/i })).toBeTruthy();
  const next = destructive();
  next.agent.pending!.revision = 2;
  await rerender({ view: view({ cards: [next] }), api });
  await tick();
  expect(screen.queryByRole("button", { name: /tap again/i })).toBeNull();
  await userEvent.keyboard("2");
  expect(api.calls).toEqual([]);
});

test("a second click after the window re-arms instead of sending", async () => {
  vi.spyOn(globalThis, "setInterval").mockImplementation((() => 0) as unknown as typeof setInterval);
  const clock = vi.spyOn(Date, "now").mockReturnValue(1_000_000);
  try {
    const api = fakeApi();
    const c = destructive();
    render(Panel, { view: view({ cards: [c] }), api });
    await userEvent.click(screen.getByRole("button", { name: /abandon/i }));
    clock.mockReturnValue(1_000_000 + 5_001);
    await userEvent.click(screen.getByRole("button", { name: /tap again to abandon/i }));
    expect(api.calls).toEqual([]);
    clock.mockReturnValue(1_000_000 + 5_001 + 1_000);
    await userEvent.click(screen.getByRole("button", { name: /tap again to abandon/i }));
    expect(api.calls).toEqual([["pick", "m/e", c.agent.pending!.id, "abandon", 1]]);
  } finally {
    vi.restoreAllMocks();
  }
});

test("an unrelated click drops the armed option", async () => {
  const api = fakeApi();
  const c = destructive();
  c.can_reply = true;
  const other = card({ key: "m/o", can_mark_seen: true, group: "done" });
  render(Panel, { view: view({ cards: [c, other] }), api });
  await userEvent.click(screen.getByRole("button", { name: /abandon/i }));
  expect(screen.getByRole("button", { name: /tap again to abandon/i })).toBeTruthy();
  await userEvent.click(screen.getByRole("button", { name: /reply/i }));
  expect(screen.queryByRole("button", { name: /tap again/i })).toBeNull();
  await userEvent.click(screen.getByRole("button", { name: /abandon/i }));
  await userEvent.click(document.querySelector(".card")!);
  expect(screen.queryByRole("button", { name: /tap again/i })).toBeNull();
  expect(api.calls.filter((x) => x[0] === "pick")).toEqual([]);
});
