import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, writeFileSync, chmodSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const script = new URL("./run-qml.sh", import.meta.url).pathname;

// Runs the wrapper with a fake qmllint (a shell body) and a no-op test runner.
function run(lintBody) {
  const dir = mkdtempSync(join(tmpdir(), "run-qml-"));
  try {
    const lint = join(dir, "qmllint");
    const runner = join(dir, "qmltestrunner");
    writeFileSync(lint, `#!/bin/sh\n${lintBody}\n`);
    writeFileSync(runner, "#!/bin/sh\nexit 0\n");
    chmodSync(lint, 0o755);
    chmodSync(runner, 0o755);
    return spawnSync("bash", [script], { encoding: "utf8", env: { ...process.env, QMLLINT: lint, QMLTESTRUNNER: runner } });
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test("a syntax error reported as Error: with exit 1 fails the wrapper", () => {
  const r = run('echo "Error: x.qml:3:1: Expected token"; exit 1');
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /Error: /);
});

test("a linter that dies without a warning fails the wrapper", () => {
  const r = run("echo boom >&2; exit 3");
  assert.notEqual(r.status, 0);
  assert.match(r.stderr, /exit 3/);
});

test("a clean linter passes", () => {
  const r = run("exit 0");
  assert.equal(r.status, 0, r.stderr);
});
