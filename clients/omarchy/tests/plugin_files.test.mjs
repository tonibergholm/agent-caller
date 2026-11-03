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
    assert.match(f.slice(0, 2500), /onActiveFocusChanged:[\s\S]*?focusedField = (activeFocus \? )?[a-zA-Z]*Field/);
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
  assert.match(body, /Model\.planAnswer\(answerEditor, cards, text, /);
  assert.match(body, /service\.send\("answer_text", plan\.fields/);
  assert.doesNotMatch(body, /pending\.(id|revision)/);
  assert.match(panel, /onViewChanged:[\s\S]*editorShouldClose[\s\S]*pruneKeyState/);
});

test("a rename keeps its draft until the service accepts the name", () => {
  const panel = read("Panel.qml");
  assert.match(panel, /rename_machine"[^\n]*reply => \{\s*panel\.report\(reply\);\s*if \(reply\.type === "ok"\) panel\.drafts\.clear\(draft\)/);
});

test("a reply keeps its draft until the service accepts it", () => {
  const panel = read("Panel.qml");
  const body = panel.slice(panel.indexOf("function submitReply"), panel.indexOf("// Sends to the request the editor"));
  const sent = body.indexOf('service.send("reply"');
  const cleared = body.indexOf("drafts.clear");
  assert.ok(sent > 0 && cleared > sent, "cleared after the send");
  assert.match(body, /if \(reply\.type !== "ok"\) return;[\s\S]*drafts\.clear/);
});

test("a reopened reply box shows its draft, empty once the reply was accepted", () => {
  const panel = read("Panel.qml");
  assert.match(panel, /onVisibleChanged: if \(visible\) \{\s*text = cardItem\.inbox\.drafts\.get\(draft\);\s*forceActiveFocus\(\);/);
});

test("the settings list scrolls and removing a machine asks first", () => {
  const panel = read("Panel.qml");
  assert.match(panel, /Flickable \{\s*visible: panel\.settingsOpen[\s\S]*contentHeight: settingsColumn\.implicitHeight/);
  const removing = panel.slice(panel.indexOf('text: confirming ? "Remove? Click again"'));
  assert.match(removing, /if \(!confirming\) \{\s*confirming = true;\s*return;/);
  assert.ok(removing.indexOf('"remove_machine"') > removing.indexOf("confirming = true"));
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
  assert.match(panel, /function onPendingCardChanged\(\) \{\s*panel\.consumePendingCard\(\);/);
  // Kept until the card is in the view, so a reconnect does not lose it.
  assert.match(panel, /function consumePendingCard\(\)[\s\S]*cardFor\(key\) === null\) return;\s*selectAndReveal\(key\);\s*host\.pendingCard = "";/);
  assert.match(panel, /onViewChanged:[\s\S]*consumePendingCard\(\)/);
  assert.match(panel, /onShownChanged: \{\s*service\.send\("panel", \{ open: shown \}\);/);
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

test("a rebuilt field takes its focus back by identity and teardown never refocuses the catcher", () => {
  const panel = read("Panel.qml");
  for (const kind of ["reply", "answer", "rename"]) {
    const f = panel.slice(panel.indexOf(`id: ${kind === "rename" ? "nameField" : kind + "Field"}`));
    assert.match(f.slice(0, 1600), new RegExp(`Model\\.fieldId\\.${kind}\\(`), kind);
    assert.match(f.slice(0, 1600), /Component\.onCompleted:[\s\S]*?focusId === fid\) forceActiveFocus\(\)/, kind);
  }
  assert.match(panel, /blocked: panel\.focusedField !== null \|\| panel\.focusId !== ""/);
  assert.doesNotMatch(panel, /onVisibleChanged:[^\n]*catcher\.forceActiveFocus/);
  assert.match(panel, /fieldFocusValid\(focusId, keyState, answerEditor, service\.settings\)\) releaseFocus\(\)/);
});

test("a sent answer closes silently and no view never closes the editor", () => {
  const panel = read("Panel.qml");
  assert.match(panel, /Model\.markSent\(answerEditor\)/);
  assert.match(panel, /Model\.editorShouldClose\(answerEditor, view\)/);
  assert.match(panel, /if \(view === null\) return;/);
});

test("minor state is cleared: pairing on hide, message on keys, uncertain marks", () => {
  const panel = read("Panel.qml");
  assert.match(panel, /onShownChanged:[\s\S]*if \(!shown\) \{\s*pairingField\.text = "";/);
  assert.match(panel, /function keyEvent\([^)]*\) \{\s*message = "";/);
  assert.match(panel, /Model\.pruneUncertain\(uncertain, cards\)/);
  assert.match(panel, /Model\.markUncertain\(panel\.uncertain, panel\.cards, cards\)/);
});

// Quickshell's QML JS engine rejects an optional catch binding; the shell
// failed to load the widget with "model.mjs:71:11: Syntax error" (Task 21).
test("model.mjs avoids syntax the QML JS engine rejects", () => {
  const src = read("model.mjs");
  assert.doesNotMatch(src, /\bcatch\s*\{/, "optional catch binding");
  assert.doesNotMatch(src, /\{\s*\.\.\./, "object spread");
});

// J/K returns a `select` effect; the panel must scroll the card Flickable to it.
// UNVERIFIED in a real Quickshell: only the wiring is pinned here.
test("Panel.run handles the select effect by scrolling the card list", () => {
  const panel = read("Panel.qml");
  assert.match(panel, /case "select":\s*revealCard\(effect\.key\);/);
  assert.match(panel, /id: cardsFlick/);
  assert.match(panel, /function revealCard\(key\) \{[\s\S]*cardsFlick\.contentY = Model\.scrollToCard\(/);
});

// A card must show the whole command or be read-only (mapping.rs), so the
// panel may not cut the detail or the question text to a few rows.
test("the permission detail and the question text are not capped to a line count", () => {
  const panel = read("Panel.qml");
  for (const marker of ["agent.pending.detail", "Model.questionLines("]) {
    const at = panel.indexOf(marker);
    assert.ok(at > 0, `anchor not found in Panel.qml: ${marker}`);
    const start = panel.lastIndexOf("Label {", at);
    const end = panel.indexOf("\n            }\n", at);
    assert.ok(start >= 0 && end > at, `Label block around ${marker} not found`);
    const label = panel.slice(start, end);
    assert.match(label, /wrapMode:\s*Text\.Wrap\b/, `${marker}: must wrap`);
    assert.doesNotMatch(label, /maximumLineCount|elide:|clip:|\bheight:|\bmaximumHeight|NoWrap|\bmaximumWidth|\bimplicitHeight:|\bLayout\.(maximum|preferred)Height/, `${marker}: hides text`);
  }
});

// UNVERIFIED in a real Quickshell: only the wiring is pinned here.
test("Panel reveals the card on a notification click", () => {
  const panel = read("Panel.qml");
  assert.match(panel, /function selectAndReveal\(key\) \{[\s\S]*?Qt\.callLater\(\(\) => revealCard\(key\)\);[\s\S]*?\n    \}/);
  assert.doesNotMatch(panel, /bottomHidden|revealBottom|revealCardBottom|scrollToCardBottom|cardBottomHidden/, "the bottom-seen state is gone");
});

// A card is answered from the panel only while it is wholly in the viewport.
// Every path that can send an answer is listed here and must go through the
// gate (Model.pickOption / Model.planAnswer with the live Panel.cardFits).
// Returns the violations found in a copy of Panel.qml / BarWidget.qml.
function gateViolations(panel, bar) {
  const bad = [];
  const fn = (src, name) => {
    const at = src.indexOf(`function ${name}(`);
    if (at < 0) return "";
    const end = src.indexOf("\n    }\n", at);
    return src.slice(at, end < 0 ? src.length : end);
  };
  // the live geometry check
  const fits = fn(panel, "cardFits");
  if (!/Model\.cardFullyVisible\(cardsFlick\.contentY, cardsFlick\.height, item\.y, item\.height\)/.test(fits)) bad.push("cardFits does not read live geometry");
  if (/\bproperty\b/.test(fits)) bad.push("cardFits caches");
  // keys: digits and Enter ask the live geometry through the event
  if (!/ev\.fits = cardFits;/.test(fn(panel, "keyEvent"))) bad.push("keyEvent does not pass cardFits");
  // mouse and typed answers
  const pick = fn(panel, "pickOption");
  if (!/const fits = cardFits\(card\.key\);\s*const r = Model\.pickOption\(keyState, card, optionId, Date\.now\(\), fits\);/.test(pick)) bad.push("pickOption wrapper not gated");
  if (!/Model\.planAnswer\(answerEditor, cards, text, answerEditor !== null && cardFits\(answerEditor\.cardKey\)\)/.test(fn(panel, "submitAnswer"))) bad.push("submitAnswer not gated");
  // Model.pickOption / planAnswer are called once each, only in those wrappers
  if ((panel.match(/Model\.pickOption\(/g) ?? []).length !== 1) bad.push("Model.pickOption called outside the wrapper");
  if ((panel.match(/Model\.planAnswer\(/g) ?? []).length !== 1) bad.push("Model.planAnswer called outside submitAnswer");
  // the raw sends exist once and only in run() / submitAnswer
  if (!/case "pick":\s*service\.send\("pick"/.test(fn(panel, "run"))) bad.push("pick send not in run()");
  if ((panel.match(/send\("pick"/g) ?? []).length !== 1) bad.push("another pick send");
  if ((panel.match(/send\("answer_text"/g) ?? []).length !== 1 || !/send\("answer_text"/.test(fn(panel, "submitAnswer"))) bad.push("answer_text send outside submitAnswer");
  if (/run\(\{\s*kind:\s*"(pick|submitAnswer)"/.test(panel)) bad.push("run() called with a hand-made pick");
  // every handler line that can send an option goes through the Panel wrapper
  for (const src of [panel, bar]) {
    for (const l of src.split("\n")) {
      if (!/onClicked|onTapped|onPressed|onAccepted|onActivated|Keys\./.test(l)) continue;
      if (/Model\.pickOption|send\("(pick|answer_text)"|\brun\(|\.run\(/.test(l)) bad.push("ungated handler: " + l.trim());
      if (/pickOption\(/.test(l) && !/cardItem\.inbox\.pickOption\(cardItem\.card, optionRow\.modelData\.id\)/.test(l)) bad.push("pick handler not via the wrapper: " + l.trim());
    }
  }
  if (/send\("(pick|answer_text|reply)"|pickOption|Model\.planAnswer/.test(bar)) bad.push("BarWidget sends an answer");
  return bad;
}

test("every Panel path that can send an answer goes through the whole-card-visible gate", () => {
  const panel = read("Panel.qml");
  const bar = read("BarWidget.qml");
  assert.deepEqual(gateViolations(panel, bar), []);
  // The tall-card note and disabled buttons
  assert.match(panel, /Too long to show here\. Answer it in the terminal\./);
  assert.match(panel, /enabled: optionRow\.modelData\.pickable && !cardItem\.inbox\.cardTooTall\(cardItem\)/);
  const mutations = {
    "click calls the model directly": [/onClicked: cardItem\.inbox\.pickOption\(cardItem\.card, optionRow\.modelData\.id\)/, "onClicked: cardItem.inbox.keyState = Model.pickOption(cardItem.inbox.keyState, cardItem.card, optionRow.modelData.id, Date.now(), true).state"],
    "click sends the pick itself": [/onClicked: cardItem\.inbox\.pickOption\(cardItem\.card, optionRow\.modelData\.id\)/, 'onClicked: cardItem.inbox.service.send("pick", {})'],
    "wrapper passes a constant": [/Date\.now\(\), fits\)/, "Date.now(), true)"],
    "keys lose the geometry": [/ev\.fits = cardFits;/, ""],
    "geometry is not live": [/Model\.cardFullyVisible\(cardsFlick\.contentY,/, "Model.cardFullyVisible(0,"],
    "typed answer ungated": [/, answerEditor !== null && cardFits\(answerEditor\.cardKey\)\)/, ", true)"],
    "second pick send": [/function markSeen|case "markSeen":/, 'case "markSeen": service.send("pick", {}); '],
  };
  for (const [name, [from, to]] of Object.entries(mutations)) {
    const mutated = panel.replace(from, to);
    assert.notEqual(mutated, panel, `mutation did not apply: ${name}`);
    assert.notDeepEqual(gateViolations(mutated, bar), [], `guard misses: ${name}`);
  }
  assert.notDeepEqual(gateViolations(panel, bar + '\nonClicked: svc.send("pick", {})'), [], "BarWidget sending a pick");
});
