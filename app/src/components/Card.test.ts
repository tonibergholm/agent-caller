import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import Card from "./Card.svelte";
import { card } from "../lib/fixtures";
import cardSource from "./Card.svelte?raw";

afterEach(cleanup);

const props = (c: ReturnType<typeof card>, now = 0) => ({
  card: c,
  selected: false,
  now,
  onpick: () => {},
  oncancel: () => {},
});

test("card_renders_hostile_text_as_text", () => {
  const evil = '<img src=x onerror="window.pwned=1">\u001b[31mred';
  const c = card({ key: "m/x", answerable: false });
  c.agent.title = evil;
  c.agent.pending = { ...c.agent.pending!, kind: "blocked_unparsed", answer_path: "none", detail: evil, raw_screen: evil };
  const { container } = render(Card, props(c));
  expect(container.querySelector("img")).toBeNull();
  expect(container.textContent).toContain('<img src=x onerror="window.pwned=1">');
  expect(screen.getByText(/answer in terminal/i)).toBeTruthy();
  expect(container.querySelector("pre")?.textContent).toContain("onerror");
});

test("offline and syncing cards are greyed with no option buttons", () => {
  const off = card({ key: "m/x", live: false, offline_since_ms: Date.parse("2026-10-08T10:00:00Z"), answerable: false });
  const { container } = render(Card, props(off, Date.parse("2026-10-08T10:05:00Z")));
  expect(container.firstElementChild?.classList.contains("offline")).toBe(true);
  expect(container.textContent).toMatch(/offline since 5 min ago/i);
  expect(container.querySelectorAll("button.option").length).toBe(0);
});

test("a syncing card says connecting and has no buttons", () => {
  const c = card({ key: "m/x", live: false, offline_since_ms: null, answerable: true, selectable_options: ["allow"] });
  const { container } = render(Card, props(c));
  expect(container.textContent).toContain("connecting…");
  expect(container.querySelectorAll("button").length).toBe(0);
});

test("allow_always shows the exact rule during its countdown", () => {
  const c = card({
    key: "m/x",
    answerable: true,
    selectable_options: ["always"],
    action: { id: "a1", card_key: "m/x", label: "Yes, always", detail: "Bash(npm test:*)", fires_at_ms: 2000 },
  });
  const { container } = render(Card, props(c, 1000));
  expect(container.textContent).toContain("Bash(npm test:*)");
  expect(container.textContent).toMatch(/sending .* in 1/i);
});

test("the countdown never goes below zero and a click cancels it", async () => {
  const oncancel = vi.fn();
  const c = card({
    key: "m/x",
    action: { id: "a1", card_key: "m/x", label: "Yes", detail: null, fires_at_ms: 1000 },
  });
  render(Card, { ...props(c, 5000), oncancel });
  const bar = screen.getByRole("button", { name: /sending yes in 0/i });
  await userEvent.click(bar);
  expect(oncancel).toHaveBeenCalledWith("a1");
});

test("notice and spinner render", () => {
  const c = card({ key: "m/x", busy: true, notice: { tone: "warning", text: "Check the terminal" } });
  const { container } = render(Card, props(c));
  expect(container.querySelector("[role=status]")?.textContent).toContain("Check the terminal");
  expect(container.querySelector(".spinner")).not.toBeNull();
});

test("answerable cards show numbered option buttons; unsupported ones are disabled", async () => {
  const onpick = vi.fn();
  const c = card({ key: "m/x", answerable: true, selectable_options: ["allow", "deny"] });
  render(Card, { ...props(c), onpick });
  const buttons = screen.getAllByRole("button");
  expect(buttons.map((b) => b.textContent?.trim())).toEqual([
    "1. Yes",
    "2. No",
    "3. Yes, always (Bash(npm test:*))",
    "4. ?",
  ]);
  expect((buttons[2] as HTMLButtonElement).disabled).toBe(true);
  expect(buttons[3].getAttribute("title")).toBe("Not supported here");
  await userEvent.click(buttons[1]);
  expect(onpick).toHaveBeenCalledWith("deny", c.agent.pending!.id, 1);
});

test("a question whose detail is its text shows that text once", () => {
  const c = card({ key: "m/x", answerable: true });
  c.agent.pending = {
    ...c.agent.pending!,
    kind: "question",
    detail: "Which branch?",
    questions: [{ id: "q", header: null, text: "Which branch?", multi_select: false, options: [] }],
  };
  const { container } = render(Card, props(c));
  expect(screen.getAllByText("Which branch?").length).toBe(1);
  expect(container.querySelector(".detail")?.textContent).toContain("Which branch?");
  expect(container.querySelector(".question")).toBeNull();
});

test("a question text that differs from the detail is still shown", () => {
  const c = card({ key: "m/x", answerable: true });
  c.agent.pending = {
    ...c.agent.pending!,
    kind: "question",
    detail: "The question could not be read.",
    questions: [{ id: "q", header: null, text: "Which branch?", multi_select: false, options: [] }],
  };
  const { container } = render(Card, props(c));
  expect(container.querySelector(".detail")?.textContent).toContain("could not be read");
  expect(container.querySelector(".question")?.textContent).toBe("Which branch?");
});

test("a question with an empty detail falls back to the question text", () => {
  const c = card({ key: "m/x", answerable: true });
  c.agent.pending = {
    ...c.agent.pending!,
    kind: "question",
    detail: "",
    questions: [{ id: "q", header: null, text: "Which branch?", multi_select: false, options: [] }],
  };
  render(Card, props(c));
  expect(screen.getAllByText("Which branch?").length).toBe(1);
});

test("a read-only T3 request says to answer in T3", () => {
  const c = card({ key: "m/x", answerable: false });
  c.agent.source = "t3";
  c.agent.pending = { ...c.agent.pending!, answer_path: "none" };
  const { container } = render(Card, props(c));
  expect(screen.getByText("Answer in T3")).toBeTruthy();
  expect(container.textContent).not.toMatch(/answer in terminal/i);
});

test.each(["herdr", "hooks", "unknown"] as const)("a read-only %s request says to answer in the terminal", (source) => {
  const c = card({ key: "m/x", answerable: false });
  c.agent.source = source;
  c.agent.pending = { ...c.agent.pending!, answer_path: "none" };
  const { container } = render(Card, props(c));
  expect(screen.getByText("Answer in terminal")).toBeTruthy();
  expect(container.textContent).not.toMatch(/answer in t3/i);
});

test("an offline card still shows the request but no options, countdown or terminal hint", () => {
  const c = card({
    key: "m/x",
    live: false,
    offline_since_ms: 0,
    answerable: true,
    selectable_options: ["allow"],
    action: { id: "a1", card_key: "m/x", label: "Yes", detail: null, fires_at_ms: 5000 },
  });
  c.agent.pending = { ...c.agent.pending!, kind: "blocked_unparsed", raw_screen: "SCREEN TEXT" };
  const { container } = render(Card, props(c, 1000));
  expect(screen.getByText("Run npm test?")).toBeTruthy();
  expect(screen.getByText("Bash(npm test)")).toBeTruthy();
  expect(container.querySelector("pre")?.textContent).toBe("SCREEN TEXT");
  expect(container.querySelector("button.undo")).toBeNull();
  expect(container.querySelectorAll("button").length).toBe(0);
  expect(container.textContent).not.toMatch(/answer in terminal/i);
});

test("answer in terminal is not shown while busy or answering", () => {
  const busy = render(Card, props(card({ key: "m/x", busy: true })));
  expect(busy.container.textContent).not.toMatch(/answer in terminal/i);
  cleanup();
  const c = card({ key: "m/y" });
  c.agent.pending = { ...c.agent.pending!, status: "answering" };
  const r = render(Card, props(c));
  expect(r.container.textContent).not.toMatch(/answer in terminal/i);
});

test("the selected card hints only at the keys it accepts", () => {
  const both = card({ key: "m/x", can_reply: true, can_mark_seen: true });
  const { container, unmount } = render(Card, { ...props(both), selected: true });
  expect(container.querySelector(".keys")?.textContent).toBe("R reply · E mark seen");
  unmount();
  const reply = render(Card, { ...props(card({ key: "m/x", can_reply: true })), selected: true });
  expect(reply.container.querySelector(".keys")?.textContent).toBe("R reply");
  reply.unmount();
  const none = render(Card, { ...props(card({ key: "m/x" })), selected: true });
  expect(none.container.querySelector(".keys")).toBeNull();
  none.unmount();
  const unselected = render(Card, props(both));
  expect(unselected.container.querySelector(".keys")).toBeNull();
});

test("a multi-line detail renders one hanging-indent block per logical line", () => {
  const c = card({ key: "m/x" });
  const long = `echo a${" ".repeat(300)}b`;
  c.agent.pending = { ...c.agent.pending!, detail: `$ echo hello\n> ${long}\n"run_in_background": true` };
  const { container } = render(Card, props(c));
  const lines = Array.from(container.querySelectorAll(".detail > .line")).map((e) => e.textContent);
  expect(lines).toEqual(["$ echo hello", `> ${long}`, '"run_in_background": true']);
  // jsdom applies no component styles, so check the stylesheet source.
  const css = cardSource;
  const rule = css.match(/\.detail \.line\s*\{([^}]*)\}/)?.[1] ?? "";
  expect(rule).toMatch(/white-space:\s*pre-wrap/);
  expect(rule).toMatch(/padding-left:\s*2ch/);
  expect(rule).toMatch(/text-indent:\s*-2ch/);
  expect(css).toMatch(/\.detail\s*\{[^}]*monospace/);
});

test.each([
  ["herdr", "herdr"],
  ["t3", "T3"],
  ["hooks", "hooks"],
  ["unknown", "unknown"],
] as const)("the %s source is shown as a badge in the meta line", (source, text) => {
  const c = card({ key: "m/x" });
  c.agent.source = source;
  const { container } = render(Card, props(c));
  const badge = container.querySelector(".meta .source");
  expect(badge?.textContent).toBe(text);
  expect(badge?.getAttribute("data-source")).toBe(source);
  expect(container.querySelector(".meta")?.textContent).toContain("Studio");
});
