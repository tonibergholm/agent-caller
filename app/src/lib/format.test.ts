import { expect, test } from "vitest";
import { since } from "./format";

test("since buckets", () => {
  const now = Date.parse("2026-10-08T12:00:00Z");
  expect(since(now - 30_000, now)).toBe("just now");
  expect(since(now + 5_000, now)).toBe("just now");
  expect(since(now - 5 * 60_000, now)).toBe("5 min ago");
  expect(since(now - 2 * 3_600_000, now)).toBe("2 h ago");
  expect(since(now - 25 * 3_600_000, now)).toBe(new Date(now - 25 * 3_600_000).toLocaleString());
});
