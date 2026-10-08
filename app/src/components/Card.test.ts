import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import Card from "./Card.svelte";
import { card } from "../lib/fixtures";

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
  expect(onpick).toHaveBeenCalledWith("deny");
});

test("a question shows its first question text", () => {
  const c = card({ key: "m/x", answerable: true });
  c.agent.pending = {
    ...c.agent.pending!,
    kind: "question",
    questions: [{ id: "q", header: null, text: "Which branch?", multi_select: false, options: [] }],
  };
  render(Card, props(c));
  expect(screen.getByText("Which branch?")).toBeTruthy();
});
