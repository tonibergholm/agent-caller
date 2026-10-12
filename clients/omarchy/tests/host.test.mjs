import test from "node:test";
import assert from "node:assert/strict";

// QML's JavaScript host has no TextEncoder or TextDecoder (plan D20).
test("model.mjs works without browser or Node globals", async () => {
  delete globalThis.TextEncoder;
  delete globalThis.TextDecoder;
  const M = await import("../model.mjs");
  const r = new M.LineReader(8);
  assert.deepEqual(r.push("é😀\n"), { lines: ["é😀"], overflow: false });
  assert.equal(r.push("éééée\n").overflow, true);
  assert.deepEqual(M.validateAnswer(" ok "), { ok: true, text: "ok" });
});
