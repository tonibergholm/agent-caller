import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import * as Model from "../model.mjs";
import { parseQml, idOf, findAll, ancestorsToFlick, extractFn, compileFns } from "./qml_helpers.mjs";

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

// A card must show the whole command or be read-only (mapping.rs), so nothing
// between the detail / question text and the card list may cut it: not the
// Label, not the card, not any Item on the way up to the Flickable (the one
// legitimate clip). The QML is parsed into its object tree, not grepped.
const CUTS = /(?:^|[\n;{])\s*(?:maximumLineCount|elide|clip|height|maximumHeight|maximumWidth|implicitHeight|Layout\.maximumHeight|Layout\.preferredHeight|Layout\.maximumWidth)\s*:/;
const CARD_IMPLICIT_HEIGHT = "body.implicitHeight + Commons.Style.spacing.lg * 2";
function detailViolations(panel) {
  const bad = [];
  const tree = parseQml(panel);
  for (const marker of ["agent.pending.detail", "Model.questionLines("]) {
    const labels = findAll(tree, (n) => n.type === "Label" && n.props.includes(marker));
    if (labels.length !== 1) {
      bad.push(`${marker}: expected one Label, found ${labels.length}`);
      continue;
    }
    const label = labels[0];
    if (!/wrapMode:\s*Text\.Wrap\b/.test(label.props)) bad.push(`${marker}: must wrap`);
    if (!/Layout\.fillWidth:\s*true/.test(label.props)) bad.push(`${marker}: must fill the width`);
    if (CUTS.test(label.props) || /NoWrap|Elide|Truncate/.test(label.props)) bad.push(`${marker}: the label hides text`);
    const { chain, reached } = ancestorsToFlick(tree, label, "cardsFlick");
    if (!reached) bad.push(`${marker}: some path does not end in cardsFlick`);
    for (const item of chain) {
      const what = `${marker}: ${item.type}${idOf(item) ? "#" + idOf(item) : item.name ? " (" + item.name + ")" : ""}`;
      const own = item.props.replace(new RegExp(`(?:^|[\\n;{])\\s*implicitHeight\\s*:\\s*${CARD_IMPLICIT_HEIGHT.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*(?=\\n|;|$)`), "\n");
      if (CUTS.test(own)) bad.push(`${what} clips or sizes the card`);
    }
  }
  return bad;
}

// The oversized-card note names a card that does not fit, options or not.
function noteViolations(panel) {
  const bad = [];
  const tree = parseQml(panel);
  const notes = findAll(tree, (n) => n.type === "Label" && n.props.includes("tooLongNote("));
  if (notes.length !== 1) return [`expected one note Label, found ${notes.length}`];
  if (!/visible:\s*note !== ""/.test(notes[0].props)) bad.push("note not shown whenever it is non-empty");
  if (/rows|options|pickable/.test(notes[0].props)) bad.push("note depends on the option rows");
  if (!/text:\s*note\b/.test(notes[0].props)) bad.push("note text is not the model's");
  const fn = extractFn(panel, "tooLongNote") ?? "";
  if (!/return Model\.tooLongNote\(item\.card, cardsFlick\.height, item\.height\);/.test(fn)) bad.push("tooLongNote is not the model's");
  return bad;
}

test("the permission detail and the question text are shown whole, to the card list", () => {
  assert.deepEqual(detailViolations(read("Panel.qml")), []);
});

test("the oversized-card note does not wait for option rows", () => {
  assert.deepEqual(noteViolations(read("Panel.qml")), []);
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
// Structural checks of a copy of Panel.qml / BarWidget.qml.
function gateViolations(panel, bar) {
  const bad = [];
  const fn = (name) => extractFn(panel, name) ?? "";
  // the live geometry check: one statement to look the card up, one return, nothing before it
  const fits = fn("cardFits");
  const body = fits.slice(fits.indexOf("{") + 1, fits.lastIndexOf("}")).trim().split(/\s*;\s*/).filter(Boolean).map((x) => x.replace(/\s+/g, " "));
  if (body.length !== 2 || body[0] !== "const item = cardItemFor(key)" || !/^return Model\.cardFitsView\(item !== null && cardsFlick\.visible, cardsFlick\.contentY, cardsFlick\.height, item \? item\.y : NaN, item \? item\.height : NaN\)$/.test(body[1])) bad.push("cardFits is not exactly the lookup and the model's gate");
  if ((fits.match(/\breturn\b/g) ?? []).length !== 1) bad.push("cardFits has an early return");
  // keys: digits and Enter ask the live geometry through the event
  if (!/ev\.fits = cardFits;/.test(fn("keyEvent"))) bad.push("keyEvent does not pass cardFits");
  // mouse and typed answers
  const pick = fn("pickOption");
  if (!/const fits = cardFits\(card\.key\);\s*const r = Model\.pickOption\(keyState, card, optionId, Date\.now\(\), fits\);/.test(pick)) bad.push("pickOption wrapper not gated");
  if (!/const plan = Model\.planAnswer\(answerEditor, cards, text, answerEditor !== null && cardFits\(answerEditor\.cardKey\)\);/.test(fn("submitAnswer"))) bad.push("submitAnswer not gated");
  // Model.pickOption / planAnswer are called once each, only in those wrappers
  if ((panel.match(/Model\.pickOption\(/g) ?? []).length !== 1) bad.push("Model.pickOption called outside the wrapper");
  if ((panel.match(/Model\.planAnswer\(/g) ?? []).length !== 1) bad.push("Model.planAnswer called outside submitAnswer");
  // the raw sends exist once and only in run() / submitAnswer
  if (!/case "pick":\s*service\.send\("pick"/.test(fn("run"))) bad.push("pick send not in run()");
  if ((panel.match(/send\("pick"/g) ?? []).length !== 1) bad.push("another pick send");
  if ((panel.match(/send\("answer_text"/g) ?? []).length !== 1 || !/send\("answer_text"/.test(fn("submitAnswer"))) bad.push("answer_text send outside submitAnswer");
  // run() is defined once and called only by pickOption and keyEvent (plus its own cases)
  if ((panel.match(/\brun\(/g) ?? []).length !== 3) bad.push("run() called from somewhere else");
  // every handler that can send an option goes through the Panel wrapper
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

// Runs Panel.qml's own gate functions (cardItemFor, cardFits, run, pickOption,
// submitAnswer, keyEvent, tooLongNote) against stubs and real model.mjs, and
// counts what would reach the service.
const sampleCard = (extra = {}) => ({
  key: "m1/a", answerable: true, free_text_answerable: true, selectable_options: ["allow", "deny"], busy: false, action: null,
  can_reply: false, can_mark_seen: false,
  agent: { pending: { id: "r1", revision: 1, kind: "permission", detail: "x", questions: null, options: [
    { id: "allow", label: "Yes", description: null, effect: "allow" },
    { id: "deny", label: "No", description: null, effect: "deny", confirm: true },
  ] } },
  ...extra,
});
const OK = { contentY: 0, flickHeight: 100, y: 10, height: 50 };
function trial(panel, geo, act) {
  const g = { visible: true, hasItem: true, ...OK, ...geo };
  const card = sampleCard();
  const sent = [];
  const ctx = {
    Model, keyState: { selected: card.key, replyFor: null }, answerEditor: null, cards: [card], visibleCards: [card], message: "", now: 0,
    cardsFlick: { contentY: g.contentY, height: g.flickHeight, visible: g.visible },
    sectionsColumn: { children: g.hasItem ? [{ card, y: g.y, height: g.height }] : [] },
    service: { send: (type, fields) => sent.push(type) },
    started: () => () => {}, releaseFocus() {}, revealCard() {}, close() {}, submitReply() {}, submitPairing() {}, report() {}, cardFor: () => card,
  };
  compileFns(panel, ["cardItemFor", "cardFits", "run", "pickOption", "submitAnswer", "keyEvent", "tooLongNote", "cardTooTall"], ctx);
  const press = (text, key = 0) => ctx.keyEvent({ key, text, modifiers: 0, accepted: false }, null, false, null);
  act({ ctx, card, press, scroll: (y) => { ctx.cardsFlick.contentY = y; }, resize: (h) => { ctx.cardsFlick.height = h; }, grow: (h) => { ctx.sectionsColumn.children[0].height = h; } });
  return { sent: sent.filter((t) => t === "pick" || t === "answer_text").length, ctx, card };
}
const ACTS = {
  click: ({ ctx, card }) => ctx.pickOption(card, "allow"),
  digit: ({ press }) => press("1"),
  armedEnter: ({ press }) => { press("2"); press("", Model.QT.Key_Return); },
  armedThenScrolledEnter: ({ press, scroll }) => { press("2"); scroll(40); press("", Model.QT.Key_Return); },
  armedThenResizedEnter: ({ press, resize }) => { press("2"); resize(40); press("", Model.QT.Key_Return); },
  armedThenGrewEnter: ({ press, grow }) => { press("2"); grow(150); press("", Model.QT.Key_Return); },
  armedThenClick: ({ ctx, card, scroll }) => { ctx.pickOption(card, "deny"); scroll(40); ctx.pickOption(card, "deny"); },
  answer: ({ ctx, card }) => { ctx.answerEditor = Model.answerEditor(card); ctx.submitAnswer("hello"); },
};
function gateBehaviour(panel) {
  const bad = [];
  try {
    for (const [name, act] of Object.entries(ACTS)) {
      const sentWhenOk = trial(panel, {}, act).sent;
      const mustSend = !/Scrolled|Resized|Grew|Click$/.test(name) || name === "click";
      if (sentWhenOk !== (mustSend ? 1 : 0)) bad.push(`${name}: sent ${sentWhenOk} while the card fits`);
    }
    const refused = {
      "half off the bottom": { y: 70 }, "half off the top": { contentY: 40 }, "far above the view": { contentY: 2000 },
      "taller than the panel": { height: 150, y: 0 }, "101 px in a 100 px panel": { height: 101, y: 0 },
      "list hidden": { visible: false }, "card gone": { hasItem: false }, "no viewport": { flickHeight: 0 },
    };
    for (const [why, geo] of Object.entries(refused)) {
      for (const name of ["click", "digit", "armedEnter", "answer"]) {
        const n = trial(panel, geo, ACTS[name]).sent;
        if (n !== 0) bad.push(`${name} sent ${n} while ${why}`);
      }
    }
    // the note: free-text-only cards too
    const note = (card, h) => trial(panel, { height: h }, ({ ctx }) => { ctx.note = ctx.tooLongNote({ card, height: h }); }).ctx.note;
    const textOnly = sampleCard({ answerable: false });
    textOnly.agent.pending.options = [];
    if (note(textOnly, 300) !== Model.TOO_LONG_NOTE) bad.push("no note on a free-text-only card taller than the panel");
    if (note(sampleCard(), 300) !== Model.TOO_LONG_NOTE) bad.push("no note on a tall card with options");
    if (note(textOnly, 50) !== "") bad.push("note on a card that fits");
  } catch (e) {
    bad.push("could not run Panel.qml's functions: " + e.message);
  }
  return bad;
}

const families = (panel, bar) => ({ gate: gateViolations(panel, bar), behaviour: gateBehaviour(panel), detail: detailViolations(panel), note: noteViolations(panel) });

test("every Panel path that can send an answer goes through the whole-card-visible gate", () => {
  const panel = read("Panel.qml");
  const bar = read("BarWidget.qml");
  assert.deepEqual(families(panel, bar), { gate: [], behaviour: [], detail: [], note: [] });
  assert.match(panel, /enabled: optionRow\.modelData\.pickable && !cardItem\.inbox\.cardTooTall\(cardItem\)/);
});

// Each mutation is applied in memory and must be caught by the named guard.
const insertAfter = (anchor, text) => [anchor, anchor + text];
const MUTATIONS = {
  "click calls the model directly": [/onClicked: cardItem\.inbox\.pickOption\(cardItem\.card, optionRow\.modelData\.id\)/, "onClicked: cardItem.inbox.keyState = Model.pickOption(cardItem.inbox.keyState, cardItem.card, optionRow.modelData.id, Date.now(), true).state", ["gate"]],
  "multi-line click calls the model directly": [/onClicked: cardItem\.inbox\.pickOption\(cardItem\.card, optionRow\.modelData\.id\)/, "onClicked: {\n                            cardItem.inbox.keyState = Model.pickOption(cardItem.inbox.keyState, cardItem.card, optionRow.modelData.id, Date.now(), true).state;\n                        }", ["gate"]],
  "click sends the pick itself": [/onClicked: cardItem\.inbox\.pickOption\(cardItem\.card, optionRow\.modelData\.id\)/, 'onClicked: cardItem.inbox.service.send("pick", {})', ["gate"]],
  "click runs a hand-made pick": [/onClicked: cardItem\.inbox\.pickOption\(cardItem\.card, optionRow\.modelData\.id\)/, 'onClicked: { cardItem.inbox.run({ kind: "pick", cardKey: cardItem.card.key }, null) }', ["gate"]],
  "wrapper passes a constant": [/Date\.now\(\), fits\)/, "Date.now(), true)", ["gate", "behaviour"]],
  "keys lose the geometry": [/ev\.fits = cardFits;/, "", ["gate", "behaviour"]],
  "keys pass a constant gate": [/ev\.fits = cardFits;/, "ev.fits = () => true;", ["behaviour"]],
  "geometry is not live": [/cardsFlick\.contentY, cardsFlick\.height, item \?/, "0, cardsFlick.height, item ?", ["gate", "behaviour"]],
  "typed answer ungated": [/, answerEditor !== null && cardFits\(answerEditor\.cardKey\)\)/, ", true)", ["gate", "behaviour"]],
  "second pick send": [/function markSeen|case "markSeen":/, 'case "markSeen": service.send("pick", {}); ', ["gate"]],
  "cardFits starts with return true": [/function cardFits\(key\) \{/, "function cardFits(key) {\n        return true;", ["gate", "behaviour"]],
  "cardFits returns early on a condition": [/function cardFits\(key\) \{/, "function cardFits(key) {\n        if (key !== '') return true;", ["gate", "behaviour"]],
  "cardFits ignores the list being hidden": [/item !== null && cardsFlick\.visible,/, "item !== null,", ["gate", "behaviour"]],
  "clip + height on the parent card": [/radius: Commons\.Style\.cornerRadius\n/, "radius: Commons.Style.cornerRadius\n        clip: true\n        height: 160\n", ["detail"]],
  "clip on the card body": [/id: body\n/, "id: body\n            clip: true\n", ["detail"]],
  "maximumHeight on the card": [/radius: Commons\.Style\.cornerRadius\n/, "radius: Commons.Style.cornerRadius\n        Layout.maximumHeight: 120\n", ["detail"]],
  "height on the card list column": [/id: sectionsColumn\n/, "id: sectionsColumn\n                    height: 300\n", ["detail"]],
  "height on one card instance": [/inbox: panel; card: modelData/, "inbox: panel; height: 120; card: modelData", ["detail"]],
  "another implicitHeight on the card": [/implicitHeight: body\.implicitHeight \+ Commons\.Style\.spacing\.lg \* 2/, "implicitHeight: 90", ["detail"]],
  "maximumLineCount on the detail": [/text: cardItem\.card\.agent\.pending \? cardItem\.meta\.tag/, "maximumLineCount: 6\n                text: cardItem.card.agent.pending ? cardItem.meta.tag", ["detail"]],
  "elide on the detail": [/text: cardItem\.card\.agent\.pending \? cardItem\.meta\.tag/, "elide: Text.ElideRight\n                text: cardItem.card.agent.pending ? cardItem.meta.tag", ["detail"]],
  "NoWrap on the question": [/wrapMode: Text\.Wrap\n(\s+)text: lines \?/, "wrapMode: Text.NoWrap\n$1text: lines ?", ["detail"]],
  "NoWrap on the detail": [/wrapMode: Text\.Wrap\n(\s+)text: cardItem\.card\.agent\.pending \?/, "wrapMode: Text.NoWrap\n$1text: cardItem.card.agent.pending ?", ["detail"]],
  "note waits for option rows": [/visible: note !== ""/, "visible: cardItem.rows.length > 0 && note !== \"\"", ["note"]],
  "note ignores the model": [/return Model\.tooLongNote\(item\.card, cardsFlick\.height, item\.height\);/, 'return item.card.answerable ? "" : "x";', ["note", "behaviour"]],
};

test("each guard fails on its mutated copy of the panel", () => {
  const panel = read("Panel.qml");
  const bar = read("BarWidget.qml");
  for (const [name, [from, to, expect]] of Object.entries(MUTATIONS)) {
    const mutated = panel.replace(from, to);
    assert.notEqual(mutated, panel, `mutation did not apply: ${name}`);
    const found = families(mutated, bar);
    for (const family of expect) assert.notDeepEqual(found[family], [], `${family} guard misses: ${name}`);
    if (process.env.SHOW_MUTATIONS) console.log(`MUT ${name}: ` + expect.map((f) => `${f}=${found[f].length} [${found[f][0]}]`).join("; "));
  }
  const withBar = families(panel, bar + '\nonClicked: svc.send("pick", {})');
  assert.notDeepEqual(withBar.gate, [], "BarWidget sending a pick");
});
