import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const read = (name) => readFileSync(new URL(`../${name}`, import.meta.url), "utf8");
const QML = ["BarWidget.qml", "Service.qml"]; // Task 19 adds Panel.qml

// Shape per docs/research/2026-10-12-omarchy-plugin-api.md section 1.
test("the manifest names the plugin, its kind, section and category", () => {
  const m = JSON.parse(read("manifest.json"));
  assert.equal(m.schemaVersion, 1);
  assert.equal(m.id, "tonibergholm.agentcaller");
  assert.ok(!m.id.startsWith("omarchy."));
  assert.ok(m.license);
  assert.deepEqual(m.kinds, ["bar-widget"]);
  assert.equal(m.entryPoints.barWidget, "BarWidget.qml");
  assert.equal(m.barWidget.defaultSection, "right");
  assert.equal(m.barWidget.category, "AI");
});

test("the IPC target has open, close, toggle and openCard", () => {
  const bar = read("BarWidget.qml");
  assert.match(bar, /Commons\.ShellIpc\s*\{/);
  assert.match(bar, /target:\s*"tonibergholm\.agentcaller"/);
  for (const fn of ["open", "close", "toggle", "openCard"]) assert.match(bar, new RegExp(`function ${fn}\\(`), fn);
});

test("no QML file logs or sets colours of its own", () => {
  for (const f of QML) {
    const src = read(f);
    assert.doesNotMatch(src, /console\./, f);
    assert.doesNotMatch(src, /#[0-9a-fA-F]{3,8}\b/, f);
    assert.doesNotMatch(src, /color:\s*"/, f);
    assert.doesNotMatch(src, /TextEncoder|TextDecoder/, f);
  }
});

test("names come from the installed shell, not the plan", () => {
  const src = QML.map(read).join("\n");
  assert.doesNotMatch(src, /Color\.(dim|surface|surfaceActive|text)\b/);
  assert.doesNotMatch(src, /Style\.(spacingSmall|radius)\b/);
  assert.doesNotMatch(src, /\bIpcHandler\b/);
});

test("every write is flushed and every QML file uses model.mjs", () => {
  assert.match(read("Service.qml"), /socket\.write\([^\n]*\);\s*\n\s*socket\.flush\(\);/);
  for (const f of QML) assert.match(read(f), /import "model\.mjs" as Model/, f);
});

test("a view message carries its omitted counts to the panel", () => {
  assert.match(read("Service.qml"), /root.omitted = msg\.omitted \|\| /);
});

test("the service never stores or prints a command's fields", () => {
  const src = read("Service.qml");
  assert.doesNotMatch(src, /pairing/i);
});

test("the bar slot is sized from the button and the button fills the entry item", () => {
  const bar = read("BarWidget.qml");
  assert.match(bar, /implicitWidth:\s*button\.implicitWidth/);
  assert.match(bar, /implicitHeight:\s*button\.implicitHeight/);
  assert.match(bar, /Ui\.WidgetButton\s*\{[^}]*id: button[^}]*anchors\.fill: parent/s);
});

test("the bar reads overflow and only a valid hello resets the backoff", () => {
  assert.match(read("BarWidget.qml"), /barState\(service\.view, service\.connected, service\.overflow\)/);
  const svc = read("Service.qml");
  assert.match(svc, /backoff\.reset\(\)/);
  assert.equal((svc.match(/backoff\.reset\(\)/g) || []).length, 1);
  assert.match(svc, /if \(msg\.protocol !== 1\) dropConnection\(\);\s*else \{[^}]*backoff\.reset\(\)/s);
});
