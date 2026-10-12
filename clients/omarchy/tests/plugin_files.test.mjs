import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const read = (name) => readFileSync(new URL(`../${name}`, import.meta.url), "utf8");
const QML = ["BarWidget.qml", "Panel.qml", "Service.qml"];

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

test("the pairing field is masked and cleared before the line is sent", () => {
  const panel = read("Panel.qml");
  assert.match(panel, /password:\s*true/);
  const body = panel.slice(panel.indexOf("function submitPairing"));
  const cleared = body.indexOf('pairingField.text = ""');
  const sent = body.indexOf('service.send("pair"');
  assert.ok(cleared > 0 && sent > cleared, "cleared before sending");
  assert.doesNotMatch(panel, /drafts[^\n]*pairing|pairing[^\n]*drafts/i);
});

test("every text field sends its keys through model.mjs with its IME state", () => {
  const panel = read("Panel.qml");
  const fields = panel.split("TextField {").slice(1);
  assert.equal(fields.length, 4, "reply, answer, machine name, pairing");
  for (const f of fields) {
    assert.match(f.slice(0, 2500), /Keys\.onPressed: event => [a-zA-Z.]*keyEvent\(event, \{ kind: "[a-z]+"[^\n]*inputMethodComposing/);
    assert.match(f.slice(0, 2500), /onActiveFocusChanged:[^\n]*focusedField/);
  }
});

test("the key catcher fills the panel, is blocked by a focused field and closes through Esc", () => {
  const panel = read("Panel.qml");
  const catcher = panel.slice(panel.indexOf("Ui.PanelKeyCatcher {"));
  assert.match(catcher, /anchors\.fill: parent/);
  assert.match(catcher, /blocked: panel\.focusedField !== null/);
  assert.match(catcher, /onCloseRequested: panel\.catcherKey\(Qt\.Key_Escape/);
  assert.match(panel, /Model\.escapeCloses\(reply\)/);
  assert.match(read("BarWidget.qml"), /focusTarget: inbox\.catcher/);
});

test("a typed answer goes to the request its editor was opened for", () => {
  const panel = read("Panel.qml");
  const body = panel.slice(panel.indexOf("function submitAnswer"), panel.indexOf("function keyEvent"));
  assert.match(body, /Model\.planAnswer\(answerEditor, cards, text\)/);
  assert.match(body, /service\.send\("answer_text", plan\.fields/);
  assert.doesNotMatch(body, /pending\.(id|revision)/);
  assert.match(panel, /onViewChanged:[\s\S]*answerEditorValid[\s\S]*pruneKeyState/);
});

test("drafts live outside the delegates; the pairing line never does", () => {
  const panel = read("Panel.qml");
  assert.match(panel, /property var drafts: new Model\.Drafts\(\)/);
  for (const kind of ["reply", "answer", "rename"]) assert.match(panel, new RegExp(`Model\\.draftKey\\.${kind}\\(`), kind);
  assert.ok((panel.match(/drafts\.set\(/g) ?? []).length >= 3);
  const pairing = panel.slice(panel.indexOf("id: pairingField"), panel.indexOf("}", panel.indexOf("id: pairingField")));
  assert.doesNotMatch(pairing, /drafts/);
});

test("the panel consumes pendingCard, clears it and reports open and closed", () => {
  const panel = read("Panel.qml");
  assert.match(panel, /function onPendingCardChanged\(\)[\s\S]*selectCard\(panel\.host\.pendingCard\);\s*panel\.host\.pendingCard = "";/);
  assert.match(panel, /onShownChanged: service\.send\("panel", \{ open: shown \}\)/);
  assert.match(panel, /function onReady\(\)[\s\S]*send\("panel", \{ open: panel\.shown \}\)/);
});

test("an overflowing view is shown as an error with its count", () => {
  assert.match(read("Service.qml"), /case "view_overflow":[\s\S]*root\.overflow = msg\.needs_you;/);
  assert.match(read("Panel.qml"), /Model\.overflowText\(panel\.service\.overflow\)/);
});

test("agent text is shown as plain text", () => {
  assert.match(read("Panel.qml"), /component Label: Text \{[^}]*textFormat: Text\.PlainText/s);
  assert.doesNotMatch(read("Panel.qml"), /^\s+Text \{/m, "use Label, not a bare Text");
});
