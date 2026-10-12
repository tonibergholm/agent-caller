import { expect, test } from "vitest";
import { hideLocal } from "./panel";
import { card, view } from "./fixtures";

const local = { key: "local", name: "This Mac", machine_id: "m1", status: { kind: "online" as const }, is_local: true, duplicate_of: null };
const dev = { key: "dev", name: "dev-box", machine_id: "m2", status: { kind: "online" as const }, is_local: false, duplicate_of: null };

test("hideLocal drops the local machine and the cards that belong to it", () => {
  const v = view({
    machines: [local, dev],
    cards: [card({ key: "local/a", machine_key: "local" }), card({ key: "dev/b", machine_key: "dev" })],
  });
  const out = hideLocal(v);
  expect(out.machines.map((m) => m.key)).toEqual(["dev"]);
  expect(out.cards.map((c) => c.key)).toEqual(["dev/b"]);
});

test("hideLocal drops the countdowns of local cards and keeps the others", () => {
  const countdown = (id: string, card_key: string) => ({ id, card_key, label: "Yes", detail: null, fires_at_ms: 1 });
  const v = view({
    machines: [local, dev],
    cards: [card({ key: "local/a", machine_key: "local" }), card({ key: "dev/b", machine_key: "dev" })],
    actions: [countdown("a1", "local/a"), countdown("a2", "dev/b")],
  });
  expect(hideLocal(v).actions.map((a) => a.id)).toEqual(["a2"]);
});

test("hideLocal returns the same view when there is no local machine", () => {
  const v = view({ machines: [dev], cards: [card({ key: "dev/b", machine_key: "dev" })] });
  expect(hideLocal(v)).toBe(v);
});
