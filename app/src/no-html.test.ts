import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { expect, test } from "vitest";

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((f) => {
    const p = join(dir, f);
    return statSync(p).isDirectory() ? files(p) : p.endsWith(".svelte") ? [p] : [];
  });
}

test("no {@html} anywhere (remote text is untrusted)", () => {
  const found = files(__dirname);
  expect(found.length).toBeGreaterThan(3);
  for (const f of found) expect(readFileSync(f, "utf8"), f).not.toContain("{@html");
});
