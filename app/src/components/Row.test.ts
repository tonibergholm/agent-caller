import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, expect, test } from "vitest";
import Row from "./Row.svelte";
import { card } from "../lib/fixtures";

afterEach(cleanup);

const working = () => {
  const c = card({ key: "m/w", group: "working", live: true, can_reply: true, can_mark_seen: true });
  c.agent.pending = null;
  return c;
};

test("macOS: a row offers Reply and Mark seen only once it is selected", () => {
  const { unmount } = render(Row, { card: working(), selected: false, now: 0, oncancel: () => {} });
  expect(screen.queryByRole("button", { name: /reply/i })).toBeNull();
  unmount();
  render(Row, { card: working(), selected: true, now: 0, oncancel: () => {} });
  expect(screen.getByRole("button", { name: /reply/i })).toBeTruthy();
});

test("macOS: the row title keeps the directory as a tooltip", () => {
  const { container } = render(Row, { card: working(), selected: false, now: 0, oncancel: () => {} });
  expect(container.querySelector(".title")?.getAttribute("title")).toBe("/work/app");
});
