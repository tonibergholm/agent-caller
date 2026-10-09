import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "vitest";

function files(dir: string, ext = ".svelte"): string[] {
  return readdirSync(dir).flatMap((f) => {
    const p = join(dir, f);
    return statSync(p).isDirectory() ? files(p, ext) : p.endsWith(ext) ? [p] : [];
  });
}

test("no {@html} anywhere (remote text is untrusted)", () => {
  const found = files(__dirname);
  expect(found.length).toBeGreaterThan(3);
  for (const f of found) expect(readFileSync(f, "utf8"), f).not.toContain("{@html");
});

// WKWebView returns false from confirm() without showing anything.
test("no window.confirm/alert/prompt in app code", () => {
  const found = [...files(__dirname), ...files(__dirname, ".ts")].filter((f) => !f.endsWith(".test.ts"));
  expect(found.length).toBeGreaterThan(3);
  for (const f of found) expect(readFileSync(f, "utf8"), f).not.toMatch(/\b(window\.)?(confirm|alert|prompt)\s*\(/);
});
