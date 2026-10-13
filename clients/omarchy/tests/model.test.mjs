import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import * as M from "../model.mjs";

const pending = (opts = {}) => ({
  id: "r1", revision: 1, agent_id: "a1", kind: "permission", status: "open", answer_path: "hook",
  title: "Bash", detail: "npm test", free_text: false, raw_screen: null, questions: null,
  opened_at: "2026-10-08T00:00:00Z",
  options: [
    { id: "allow", label: "Yes", description: null, effect: "allow" },
    { id: "always", label: "Yes, always", description: "Bash(npm test:*)", effect: "allow_always" },
    { id: "deny", label: "No", description: null, effect: "deny" },
    { id: "odd", label: "Something", description: null, effect: "unknown" },
  ],
  ...opts,
});

const card = (key, group = "request", extra = {}) => ({
  key, machine_key: key.split("/")[0], machine_name: "Box", group, live: true, offline_since_ms: null,
  answerable: group === "request", free_text_answerable: false, selectable_options: ["allow", "always", "deny"],
  can_reply: true, can_mark_seen: group === "done", busy: false, notice: null, action: null,
  agent: { id: key.split("/")[1], source: "herdr", kind: "claude", title: "agent", project: "api", cwd: "/w/api",
    state: group === "request" ? "needs_you" : group, pending: group === "request" ? pending() : null },
  ...extra,
});

const view = (cards, machines = [{ key: "m1", name: "Box", machine_id: "x", status: { kind: "online" }, is_local: false, duplicate_of: null }]) =>
  ({ machines, cards, hidden_idle: 0, show_idle: false, show_subagents: false, show_hook_only: false,
     tray: { color: "amber", needs_you: cards.filter((c) => c.group === "request").length }, actions: [] });

const key = (k, extra = {}) => ({ key: k, shift: false, ctrl: false, alt: false, meta: false, composing: false, field: null, ...extra });

test("the line reader handles partial lines, several lines per chunk and split characters", () => {
  const r = new M.LineReader();
  assert.deepEqual(r.push('{"a":'), { lines: [], overflow: false });
  assert.deepEqual(r.push('1}\n{"b":2}\n{"c"'), { lines: ['{"a":1}', '{"b":2}'], overflow: false });
  assert.deepEqual(r.push(":3}\n"), { lines: ['{"c":3}'], overflow: false });
  const emoji = "😀";
  assert.deepEqual(r.push(`"${emoji[0]}`), { lines: [], overflow: false });
  assert.deepEqual(r.push(`${emoji[1]}"\n`), { lines: [`"${emoji}"`], overflow: false });
});

test("UTF-8 lengths are counted without TextEncoder", () => {
  assert.equal(M.utf8Length("a"), 1);
  assert.equal(M.utf8Length("é"), 2);
  assert.equal(M.utf8Length("€"), 3);
  assert.equal(M.utf8Length("😀"), 4);
  assert.equal(M.utf8Length("😀"[0]), 3, "a lone surrogate");
  const sample = "aé€😀\u{10FFFF}x";
  assert.equal(M.utf8Length(sample), Buffer.byteLength(sample, "utf8"));
});

test("a line over the limit overflows, counted in UTF-8 bytes", () => {
  const r = new M.LineReader(8);
  assert.equal(r.push("é".repeat(5)).overflow, true, "10 bytes without a newline");
  const s = new M.LineReader(8);
  assert.equal(s.push("12345678\n").overflow, false);
  assert.equal(s.push("123456789\n").overflow, true);
});

test("only JSON objects with a string type are messages", () => {
  assert.deepEqual(M.parseMessage('{"type":"hello","protocol":1}'), { type: "hello", protocol: 1 });
  for (const bad of ["nope", "[]", "null", '{"type":7}', "{}"]) assert.equal(M.parseMessage(bad), null, bad);
});

test("sections follow the Mac panel and a machine filter keeps countdowns", () => {
  const cards = [card("m1/a"), card("m2/b", "working"), card("m1/c", "done"), card("m2/d", "idle"),
    card("m2/e", "request", { action: { id: "x", card_key: "m2/e", label: "Yes", detail: null, fires_at_ms: 0 } })];
  const all = M.sections(cards, null);
  assert.deepEqual(all.needs.map((c) => c.key), ["m1/a", "m2/e"]);
  assert.deepEqual(all.working.map((c) => c.key), ["m2/b"]);
  assert.deepEqual(all.done.map((c) => c.key), ["m1/c", "m2/d"]);
  assert.deepEqual(M.sections(cards, "m1").needs.map((c) => c.key), ["m1/a", "m2/e"]);
  assert.equal(M.headerText(view([card("m1/a"), card("m2/b", "working")])), "Inbox · 1 need you · 1 working");
});

test("the bar is dim, urgent or down", () => {
  assert.deepEqual(M.barState(view([]), true), { count: 0, tone: "dim", text: "" });
  assert.deepEqual(M.barState(view([card("m1/a")]), true), { count: 1, tone: "urgent", text: "1" });
  assert.equal(M.barState(view([card("m1/a")]), false).tone, "down");
  assert.equal(M.barState(null, true).tone, "down");
  assert.deepEqual(M.barState(null, true, 3), { count: 3, tone: "urgent", text: "3" });
  assert.deepEqual(M.barState(null, true, 0), { count: 0, tone: "dim", text: "" });
  assert.equal(M.barState(null, false, 3).tone, "down");
});

test("a locked keyring is not shown as not paired", () => {
  const v = view([], [{ key: "m1", name: "Box", machine_id: null, status: { kind: "auth_failed", message: "Pair this machine: run `acd pair` there" }, is_local: false, duplicate_of: null }]);
  const settings = { machines: [{ key: "m1", name: "Box", endpoint: { kind: "tcp", host: "h", port: 1 }, pairing: "unreadable", secret: "locked" }] };
  const [chip] = M.machineChips(v, settings);
  assert.equal(chip.status, "locked");
  assert.equal(chip.label, "Keyring locked");
  assert.equal(M.keyringLocked(settings), true);
  settings.machines[0].secret = "missing";
  assert.equal(M.machineChips(v, settings)[0].status, "not_paired");
  settings.machines[0].secret = "unknown";
  assert.equal(M.machineChips(v, settings)[0].label, "Keyring state unknown. Retry");
  settings.machines[0].secret = "ok";
  assert.equal(M.machineChips(v, settings)[0].status, "error");
  assert.equal(M.keyringLocked(settings), false);
});

test("option rows follow the card's flags", () => {
  const rows = M.optionRows(card("m1/a"));
  assert.deepEqual(rows.map((r) => [r.number, r.id, r.pickable, r.rule]), [
    [1, "allow", true, null], [2, "always", true, "Bash(npm test:*)"], [3, "deny", true, null], [4, "odd", false, null],
  ]);
  const noRule = card("m1/a");
  noRule.agent.pending = pending({ options: [{ id: "always", label: "Always", description: null, effect: "allow_always" }] });
  noRule.selectable_options = ["always"];
  assert.equal(M.optionRows(noRule)[0].pickable, false, "an always-allow option without its rule cannot be picked");
  for (const description of ["", "   "]) {
    const blank = card("m1/a");
    blank.agent.pending = pending({ options: [{ id: "always", label: "Always", description, effect: "allow_always" }] });
    blank.selectable_options = ["always"];
    assert.equal(M.optionRows(blank)[0].rule, null, JSON.stringify(description));
    assert.equal(M.optionRows(blank)[0].pickable, false, "a blank rule cannot be picked: " + JSON.stringify(description));
  }
  const readOnly = card("m1/a", "request", { answerable: false });
  assert.ok(M.optionRows(readOnly).every((r) => !r.pickable));
});

test("keys move, pick, and open the reply and answer fields by the card's flags", () => {
  const cards = [card("m1/a"), card("m1/b", "done")];
  let st = { selected: "m1/a", replyFor: null };
  assert.deepEqual(M.onKey(cards, st, key("j")).effect, { kind: "select", key: "m1/b" });
  assert.deepEqual(M.onKey(cards, st, key("ArrowDown")).effect, { kind: "select", key: "m1/b" });
  assert.deepEqual(M.onKey(cards, st, key("2")).effect, { kind: "pick", cardKey: "m1/a", requestId: "r1", optionId: "always", revision: 1 });
  assert.equal(M.onKey(cards, st, key("4")).effect.kind, "none", "unknown effect");
  assert.equal(M.onKey(cards, st, key("9")).effect.kind, "none", "no ninth option");
  assert.equal(M.onKey(cards, st, key("t")).effect.kind, "none", "no typed answer for this request");
  const typed = card("m1/a", "request", { free_text_answerable: true });
  assert.deepEqual(M.onKey([typed], st, key("t")).effect, { kind: "focusAnswer", cardKey: "m1/a" });
  assert.deepEqual(M.onKey(cards, st, key("r")).effect, { kind: "openReply", cardKey: "m1/a" });
  assert.equal(M.onKey(cards, st, key("e")).effect.kind, "none");
  st = { selected: "m1/b", replyFor: null };
  assert.deepEqual(M.onKey(cards, st, key("e")).effect, { kind: "markSeen", cardKey: "m1/b" });
  assert.deepEqual(M.onKey(cards, st, key("Escape")).effect, { kind: "escape" });
});

test("in a text field letters and digits are text, Enter submits, and IME or modifiers are never shortcuts", () => {
  const cards = [card("m1/a")];
  const st = { selected: "m1/a", replyFor: "m1/a" };
  const reply = { kind: "reply", cardKey: "m1/a" };
  for (const k of ["j", "1", "r", "e", "t"]) assert.equal(M.onKey(cards, st, key(k, { field: reply })).effect.kind, "none", k);
  assert.deepEqual(M.onKey(cards, st, key("Enter", { field: reply })).effect, { kind: "submitReply", cardKey: "m1/a" });
  assert.equal(M.onKey(cards, st, key("Enter", { field: reply, shift: true })).effect.kind, "none");
  assert.deepEqual(M.onKey(cards, st, key("Enter", { field: { kind: "answer", cardKey: "m1/a" } })).effect, { kind: "submitAnswer", cardKey: "m1/a" });
  assert.deepEqual(M.onKey(cards, st, key("Enter", { field: { kind: "pairing", cardKey: null } })).effect, { kind: "submitPairing" });
  assert.equal(M.onKey(cards, st, key("Enter", { field: reply, composing: true })).effect.kind, "none");
  assert.equal(M.onKey(cards, { selected: "m1/a", replyFor: null }, key("1", { composing: true })).effect.kind, "none");
  assert.equal(M.onKey(cards, { selected: "m1/a", replyFor: null }, key("1", { ctrl: true })).effect.kind, "none");
  assert.deepEqual(M.onKey(cards, st, key("Escape", { field: reply })).effect, { kind: "escape" });
});

test("Qt key events become key inputs", () => {
  assert.deepEqual(M.fromQt(M.QT.Key_Escape, "", 0, false, null).key, "Escape");
  assert.equal(M.fromQt(M.QT.Key_Return, "\r", 0, false, null).key, "Enter");
  assert.equal(M.fromQt(M.QT.Key_Down, "", 0, false, null).key, "ArrowDown");
  const j = M.fromQt(0x4a, "J", M.QT.ShiftModifier, false, null);
  assert.equal(j.key, "J");
  assert.equal(j.shift, true);
  assert.equal(M.fromQt(0x31, "1", M.QT.ControlModifier, false, null).ctrl, true);
  assert.equal(M.fromQt(0x31, "1", 0, true, null).composing, true);
});

test("typed answers and replies are checked as acd-client checks them", () => {
  assert.deepEqual(M.validateAnswer("  yes\r\nplease  "), { ok: true, text: "yes\nplease" });
  assert.deepEqual(M.validateAnswer("   "), { ok: false, reason: "empty" });
  assert.deepEqual(M.validateAnswer("a\u202Eb"), { ok: false, reason: "characters" });
  assert.deepEqual(M.validateAnswer("a\rb"), { ok: false, reason: "characters" });
  assert.deepEqual(M.validateAnswer("a\tb"), { ok: true, text: "a\tb" });
  assert.deepEqual(M.validateAnswer("é".repeat(8193)), { ok: false, reason: "too_long" });
  assert.deepEqual(M.validateReply(" hi "), { ok: true, text: " hi " });
  assert.deepEqual(M.validateReply(" "), { ok: false, reason: "empty" });
  assert.deepEqual(M.validateReply("x".repeat(64 * 1024 + 1)), { ok: false, reason: "too_long" });
});

test("every service error code has text, and unknown codes fall back", () => {
  for (const code of ["stale", "unknown_card", "not_answerable", "unknown_option", "already_counting", "bad_text",
    "cannot_reply", "cannot_mark_seen", "hidden", "unknown_machine", "not_paired", "bad_name", "bad_pairing",
    "bad_request", "secret_store_locked", "secret_store_timeout", "secret_store_failed", "settings_failed",
    "duplicate_id", "unsupported", "unreachable"]) {
    assert.ok(M.errorText(code).length > 0, code);
    assert.notEqual(M.errorText(code), M.errorText("never-heard-of-it"), code);
  }
  assert.equal(M.errorText("stale"), "The request changed. Review it again.");
});

test("after outcomes_lost, this view's unfinished actions are uncertain", () => {
  const t = new M.Tracker();
  t.started("x1", "m1/a");
  t.started("x2", "m1/b");
  t.started("x3", "m1/c");
  t.outcome({ type: "outcome", action: "x1", card: "m1/a", result: { kind: "fired" } });
  t.outcome({ type: "outcome", action: "x2", card: "m1/b", result: { kind: "delivered" } });
  assert.deepEqual(t.lost().sort(), ["m1/a", "m1/c"]);
  assert.deepEqual(t.lost(), []);
  assert.equal(M.UNCERTAIN_TEXT, "Check the terminal");
});

test("a dropped connection leaves only fired actions uncertain", () => {
  const t = new M.Tracker();
  t.started("x1", "m1/a");
  t.started("x2", "m1/b");
  t.outcome({ type: "outcome", action: "x1", card: "m1/a", result: { kind: "fired" } });
  assert.deepEqual(t.disconnected(), ["m1/a"]);
  // A later outcomes_lost has nothing left to mark.
  assert.deepEqual(t.lost(), []);
  assert.deepEqual(t.disconnected(), []);
});

test("an outcome that arrives before the reply naming its action is not resurrected", () => {
  const t = new M.Tracker();
  t.outcome({ type: "outcome", action: "x1", card: "m1/a", result: { kind: "cancelled" } });
  t.started("x1", "m1/a");
  assert.deepEqual(t.lost(), []);
});

test("a fired outcome that beats its reply still counts once the reply names it", () => {
  const t = new M.Tracker();
  t.outcome({ type: "outcome", action: "x1", card: "m1/a", result: { kind: "fired" } });
  t.started("x1", "m1/a");
  assert.deepEqual(t.disconnected(), ["m1/a"]);
});

test("another view's fired action never marks a card here", () => {
  const t = new M.Tracker();
  t.outcome({ type: "outcome", action: "theirs", card: "m1/b", result: { kind: "fired" } });
  assert.deepEqual(t.disconnected(), []);
  t.outcome({ type: "outcome", action: "theirs2", card: "m1/b", result: { kind: "fired" } });
  assert.deepEqual(t.lost(), []);
});

test("a later terminal outcome settles an uncertain card; a fired one does not", () => {
  const marks = { "m1/a": "-", "m1/b": "r2" };
  assert.deepEqual(M.clearUncertain(marks, { card: "m1/a", result: { kind: "delivered" } }), { "m1/b": "r2" });
  assert.equal(M.clearUncertain(marks, { card: "m1/a", result: { kind: "fired" } }), marks);
  assert.equal(M.clearUncertain(marks, { card: "m9/z", result: { kind: "cancelled" } }), marks);
});

test("only the lost action's own terminal outcome settles its card", () => {
  const t = new M.Tracker();
  t.started("x1", "m1/a");
  t.lost();
  // Another action on the same card, or another view's, settles nothing.
  assert.equal(t.settles({ action: "other", card: "m1/a", result: { kind: "delivered" } }), false);
  assert.equal(t.settles({ action: "x1", card: "m1/a", result: { kind: "fired" } }), false);
  assert.equal(t.settles({ action: "x1", card: "m1/a", result: { kind: "delivered" } }), true);
  assert.equal(t.settles({ action: "x1", card: "m1/a", result: { kind: "delivered" } }), false, "once");
});

test("a fired action's result after a reconnect settles its card", () => {
  const t = new M.Tracker();
  t.started("x1", "m1/a");
  t.outcome({ type: "outcome", action: "x1", card: "m1/a", result: { kind: "fired" } });
  assert.deepEqual(t.disconnected(), ["m1/a"]);
  assert.equal(t.settles({ action: "x1", card: "m1/a", result: { kind: "delivered" } }), true);
});

test("a filter on a removed machine no longer hides the inbox", () => {
  const settings = { machines: [{ key: "m1" }, { key: "m2" }] };
  assert.equal(M.activeFilter("", settings), null);
  assert.equal(M.activeFilter("m2", settings), "m2");
  assert.equal(M.activeFilter("gone", settings), null);
  assert.equal(M.activeFilter("m2", null), "m2", "no settings yet: keep it");
  assert.equal(M.activeFilter("gone", { machines: [{ key: "m1" }], omitted_machines: 3 }), "gone", "the list is cut: keep it");
});

test("ids are unique and short", () => {
  const ids = new M.Ids("bar");
  const a = ids.next();
  const b = ids.next();
  assert.notEqual(a, b);
  assert.ok(a.length <= 64);
});

test("a typed answer stays bound to the request it was opened for", () => {
  const c = card("m1/a", "request", { free_text_answerable: true });
  const editor = M.answerEditor(c);
  assert.deepEqual(editor, { cardKey: "m1/a", requestId: "r1", revision: 1 });
  assert.equal(M.answerEditorValid(editor, [c]), true);
  const bumped = card("m1/a", "request", { free_text_answerable: true });
  bumped.agent.pending = pending({ revision: 2 });
  assert.equal(M.answerEditorValid(editor, [bumped]), false, "a new revision closes the editor");
  const replaced = card("m1/a", "request", { free_text_answerable: true });
  replaced.agent.pending = pending({ id: "r2" });
  assert.equal(M.answerEditorValid(editor, [replaced]), false, "a new request closes the editor");
  assert.equal(M.answerEditorValid(editor, []), false);
  assert.equal(M.answerEditor(card("m1/a")), null, "no editor without a typed-answer request");
});

test("omitted cards are counted for the panel", () => {
  assert.equal(M.omittedText({ done: 0, working: 0 }), "");
  assert.equal(M.omittedText({ done: 3, working: 0 }), "3 done not shown");
  assert.equal(M.omittedText({ done: 3, working: 2 }), "3 done and 2 working not shown");
  assert.equal(M.omittedText(undefined), "");
});

test("drafts survive an unrelated new snapshot and go with their request", () => {
  const drafts = new M.Drafts();
  const c = card("m1/a", "request", { free_text_answerable: true });
  const answerKey = M.draftKey.answer(M.answerEditor(c));
  drafts.set(answerKey, "half an answer");
  drafts.set(M.draftKey.reply(c), "half a reply");
  drafts.set(M.draftKey.rename("m1"), "New na");
  const settings = { machines: [{ key: "m1" }] };
  // An unrelated change: another card appears; the view is a new object.
  const next = view([card("m1/a", "request", { free_text_answerable: true }), card("m1/b", "working")]);
  drafts.prune(next, settings);
  assert.equal(drafts.get(M.draftKey.answer(M.answerEditor(next.cards[0]))), "half an answer");
  assert.equal(drafts.get(M.draftKey.reply(next.cards[0])), "half a reply");
  assert.equal(drafts.get(M.draftKey.rename("m1")), "New na");
  const changed = card("m1/a", "request", { free_text_answerable: true });
  changed.agent.pending = pending({ id: "r2" });
  drafts.prune(view([changed]), { machines: [] });
  assert.equal(drafts.get(answerKey), "", "a new request drops the old answer draft");
  assert.equal(drafts.get(M.draftKey.rename("m1")), "", "a removed machine drops its rename draft");
});

test("an overflowing view has its own text", () => {
  assert.equal(M.overflowText(7), "Too many requests to show here (7 need you). Answer some in the terminal.");
});

test("card meta and the countdown bar", () => {
  const c = card("m1/a");
  assert.deepEqual(M.cardMeta(c), { kind: "Claude", machine: "Box", project: "api", title: "agent", badge: "herdr", tag: "BASH APPROVAL" });
  assert.equal(M.countdownFraction({ fires_at_ms: 2000 }, 1000), 0.5);
  assert.equal(M.countdownFraction({ fires_at_ms: 0 }, 1000), 0);
});

test("a reader that overflowed stays overflowed and holds no old text", () => {
  const r = new M.LineReader(8);
  assert.equal(r.push("123456789\nabc\n").overflow, true);
  assert.deepEqual(r.push("ok\n"), { lines: [], overflow: true }, "each connection needs a new reader");
});

test("the header counts the same sections as the panel", () => {
  const v = view([card("m1/a"), card("m1/b", "working"), card("m1/c", "unknown")]);
  assert.equal(M.headerText(v), "Inbox · 1 need you · 2 working");
});

test("barLabel shows the count, a plain glyph, or the unreachable glyph", () => {
  assert.equal(M.barLabel({ count: 2, tone: "urgent", text: "2" }), "\u25c9 2");
  assert.equal(M.barLabel({ count: 0, tone: "dim", text: "" }), "\u25c9");
  assert.equal(M.barLabel({ count: 0, tone: "down", text: "" }), "\u2298");
});

test("Backoff doubles to the cap and resets only when told", () => {
  const b = new M.Backoff(500, 2000);
  assert.deepEqual([b.next(), b.next(), b.next(), b.next()], [500, 1000, 2000, 2000]);
  b.reset();
  assert.equal(b.next(), 500);
});

test("a typed answer is sent to the request its editor was opened for", () => {
  const c = card("m1/a", "request", { free_text_answerable: true, agent: { pending: { id: "r1", revision: 3 } } });
  const editor = M.answerEditor(c);
  const plan = M.planAnswer(editor, [c], "  yes\r\nplease ");
  assert.deepEqual(plan, { kind: "send", fields: { card: "m1/a", request: "r1", revision: 3, text: "yes\nplease" } });
});

test("a changed request closes the editor at submit time and nothing is sent", () => {
  const c = card("m1/a", "request", { free_text_answerable: true, agent: { pending: { id: "r1", revision: 3 } } });
  const editor = M.answerEditor(c);
  const bumped = { ...c, agent: { pending: { id: "r1", revision: 4 } } };
  assert.equal(M.planAnswer(editor, [bumped], "x").kind, "closed");
  assert.equal(M.planAnswer(editor, [], "x").kind, "closed");
  assert.equal(M.planAnswer(null, [c], "x").kind, "closed");
});

test("a card with a countdown or busy takes no second answer", () => {
  const base = card("m1/a", "request", { free_text_answerable: true, agent: { pending: { id: "r1", revision: 3 } } });
  const editor = M.answerEditor(base);
  assert.equal(M.planAnswer(editor, [{ ...base, action: { id: "a1", fires_at_ms: 1 } }], "x").kind, "none");
  assert.equal(M.planAnswer(editor, [{ ...base, busy: true }], "x").kind, "none");
  assert.equal(M.planAnswer(editor, [base], "   ").kind, "invalid");
});

test("the reply box closes when its card leaves the view", () => {
  const st = { selected: "m1/a", replyFor: "m1/a" };
  assert.deepEqual(M.pruneKeyState(st, [card("m1/a")]), st);
  assert.deepEqual(M.pruneKeyState(st, [card("m1/b")]), { selected: "m1/a", replyFor: null });
  assert.deepEqual(M.pruneKeyState({ selected: null, replyFor: null }, []), { selected: null, replyFor: null });
});

test("openCard selects its card and closes a reply box", () => {
  assert.deepEqual(M.selectCard("m1/a"), { selected: "m1/a", replyFor: null });
});

test("Esc closes the panel unless the service cancelled a countdown", () => {
  assert.equal(M.escapeCloses({ type: "ok", result: true }), false);
  assert.equal(M.escapeCloses({ type: "ok", result: false }), true);
  assert.equal(M.escapeCloses({ type: "error", code: "unreachable" }), true);
});

test("a focused field is remembered by identity and valid while its reply box, editor or machine exists", () => {
  const st = { selected: "m1/a", replyFor: "m1/a" };
  const c = card("m1/a", "request", { free_text_answerable: true });
  const editor = M.answerEditor({ ...c, agent: { ...c.agent, pending: { id: "r1", revision: 1 } } });
  const settings = { machines: [{ key: "m1" }] };
  assert.equal(M.fieldFocusValid(M.fieldId.reply("m1/a"), st, null, settings), true);
  assert.equal(M.fieldFocusValid(M.fieldId.reply("m1/a"), { ...st, replyFor: null }, null, settings), false);
  assert.equal(M.fieldFocusValid(M.fieldId.answer("m1/a"), st, editor, settings), true);
  assert.equal(M.fieldFocusValid(M.fieldId.answer("m1/a"), st, null, settings), false);
  assert.equal(M.fieldFocusValid(M.fieldId.rename("m1"), st, null, settings), true);
  assert.equal(M.fieldFocusValid(M.fieldId.rename("m2"), st, null, settings), false);
  assert.equal(M.fieldFocusValid("", st, editor, settings), false);
  assert.equal(M.fieldFocusValid("junk", st, editor, settings), false);
});

test("a sent editor closes silently; an unsent one says the answer was not sent", () => {
  const c = card("m1/a", "request", { free_text_answerable: true, agent: { pending: { id: "r1", revision: 1 } } });
  const editor = M.answerEditor(c);
  assert.equal(M.editorClosedNotice(editor), M.EDITOR_CLOSED_TEXT);
  assert.equal(M.editorClosedNotice(M.markSent(editor)), "");
  assert.equal(M.answerEditorValid(M.markSent(editor), [c]), true, "sent does not change validity");
  assert.equal(M.markSent(null), null);
});

test("no view never closes an editor", () => {
  const c = card("m1/a", "request", { free_text_answerable: true, agent: { pending: { id: "r1", revision: 1 } } });
  const editor = M.answerEditor(c);
  assert.equal(M.editorShouldClose(editor, null), false);
  assert.equal(M.editorShouldClose(editor, view([c])), false);
  assert.equal(M.editorShouldClose(editor, view([])), true);
  assert.equal(M.editorShouldClose(null, view([])), false);
});

test("an uncertain mark ends when the request changes or the card leaves", () => {
  const a = card("m1/a");
  const marks = M.markUncertain({}, [a], ["m1/a", "m1/gone"]);
  const next = M.pruneUncertain(marks, [a]);
  assert.deepEqual(Object.keys(next), ["m1/a"]);
  const changed = { ...a, agent: { ...a.agent, pending: { ...a.agent.pending, id: "other" } } };
  assert.deepEqual(M.pruneUncertain(next, [changed]), {});
  assert.deepEqual(M.pruneUncertain(next, []), {});
});

test("openCard reveals a card the machine filter or collapsed Done hides", () => {
  const cards = [card("m1/a"), card("m2/d", "done")];
  assert.deepEqual(M.revealCard("m1/a", cards, "m2", false), { machineFilter: "", doneOpen: false });
  assert.deepEqual(M.revealCard("m2/d", cards, "m2", false), { machineFilter: "m2", doneOpen: true });
  assert.deepEqual(M.revealCard("m1/a", cards, "m1", false), { machineFilter: "m1", doneOpen: false });
  assert.deepEqual(M.revealCard("zz", cards, "m2", false), { machineFilter: "m2", doneOpen: false });
});

const externalCard = (key = "m1/e", extra = {}) => ({
  ...card(key),
  free_text_answerable: true,
  selectable_options: ["go", "abandon"],
  agent: {
    id: "e", source: "external", kind: "other", title: "native autopilot", project: "engine", cwd: "/w/engine",
    state: "needs_you",
    pending: pending({
      kind: "question", answer_path: "external", title: "ENG-12 parked", detail: "Repair exhausted", free_text: true,
      options: [
        { id: "go", label: "Go", description: null, effect: "choose" },
        { id: "abandon", label: "Abandon", description: null, effect: "choose", confirm: true },
      ],
    }),
  },
  ...extra,
});

test("an external card is labelled with its producer and headed by what it asks", () => {
  const meta = M.cardMeta(externalCard());
  assert.equal(meta.badge, "native autopilot");
  assert.equal(meta.title, "ENG-12 parked");
  assert.equal(meta.tag, "QUESTION");
  const none = externalCard();
  none.agent.pending = null;
  assert.equal(M.cardMeta(none).title, "native autopilot", "without a request the producer's title is the heading");
});

test("a typed answer is offered on an external question", () => {
  const cards = [externalCard()];
  assert.deepEqual(M.onKey(cards, { selected: "m1/e", replyFor: null }, key("t")).effect, { kind: "focusAnswer", cardKey: "m1/e" });
});

test("an option that needs confirmation arms on the first press and is sent by the second", () => {
  const cards = [externalCard()];
  const first = M.onKey(cards, { selected: "m1/e", replyFor: null }, key("2", { at: 1000 }));
  assert.equal(first.effect.kind, "none");
  assert.deepEqual(first.state.armed, { cardKey: "m1/e", requestId: "r1", revision: 1, optionId: "abandon", at: 1000 });
  const rows = M.optionRows(cards[0], first.state.armed, 1500);
  assert.deepEqual(rows.map((r) => [r.id, r.confirm, r.armed]), [["go", false, false], ["abandon", true, true]]);
  const second = M.onKey(cards, first.state, key("Enter", { at: 2000 }));
  assert.deepEqual(second.effect, { kind: "pick", cardKey: "m1/e", requestId: "r1", optionId: "abandon", revision: 1 });
  assert.equal(second.state.armed, null);
  assert.equal(M.onKey(cards, { selected: "m1/e", replyFor: null }, key("1")).effect.kind, "pick", "an option without confirm sends at once");
});

test("a second press outside the window, on another option, or after any other key does not send", () => {
  const cards = [externalCard()];
  const armed = M.onKey(cards, { selected: "m1/e", replyFor: null }, key("2", { at: 1000 })).state;
  const late = M.onKey(cards, armed, key("Enter", { at: 1000 + M.CONFIRM_MS + 1 }));
  assert.equal(late.effect.kind, "none", "Enter after the window sends nothing");
  assert.equal(late.state.armed, null);
  assert.equal(M.optionRows(cards[0], armed.armed, 1000 + M.CONFIRM_MS + 1).find((r) => r.id === "abandon").armed, false);
  assert.equal(M.onKey(cards, armed, key("1", { at: 1100 })).effect.optionId, "go");
  assert.equal(M.onKey(cards, armed, key("1", { at: 1100 })).state.armed, null);
  for (const other of ["j", "Escape", "r", "t"]) {
    const after = M.onKey(cards, armed, key(other, { at: 1100 })).state;
    assert.equal(after.armed, null, other);
    assert.equal(M.onKey(cards, after, key("2", { at: 1200 })).effect.kind, "none", "the press after " + other + " arms again");
  }
});

test("a click goes through the same two steps and an armed option never outlives its request", () => {
  const c = externalCard();
  const st = { selected: "m1/e", replyFor: null };
  const first = M.pickOption(st, c, "abandon", 1000);
  assert.equal(first.effect.kind, "none");
  const second = M.pickOption(first.state, c, "abandon", 1500);
  assert.equal(second.effect.kind, "pick");
  const moved = externalCard();
  moved.agent.pending.revision = 2;
  assert.equal(M.pruneKeyState(first.state, [moved]).armed, null, "a changed request disarms");
  assert.equal(M.pruneKeyState(first.state, []).armed, null, "a gone card disarms");
  assert.equal(M.pruneKeyState(first.state, [c]).armed.optionId, "abandon");
  assert.equal(M.pickOption(first.state, moved, "abandon", 1500).effect.kind, "none", "the old arm cannot send the new revision");
  assert.equal(M.pickOption(st, c, "nope", 1000).effect.kind, "none");
});

test("the panel sends picks through the model so a click cannot skip the confirmation", () => {
  const panel = fs.readFileSync(new URL("../Panel.qml", import.meta.url), "utf8");
  assert.match(panel, /cardItem\.inbox\.pickOption\(cardItem\.card, optionRow\.modelData\.id\)/);
  assert.doesNotMatch(panel, /run\(\{ kind: "pick"/);
});

test("typing in a text field disarms an armed option", () => {
  const cards = [externalCard()];
  const armed = M.onKey(cards, { selected: "m1/e", replyFor: null }, key("2", { at: 1000 })).state;
  const typed = M.onKey(cards, armed, key("x", { field: { kind: "reply", cardKey: "m1/e" }, at: 1100 }));
  assert.equal(typed.state.armed, null);
  assert.equal(M.onKey(cards, typed.state, key("Enter", { at: 1200 })).effect.kind, "none");
});

test("a held key neither arms nor confirms an option", () => {
  const cards = [externalCard()];
  const fresh = { selected: "m1/e", replyFor: null };
  const held = M.onKey(cards, fresh, key("2", { at: 1000, repeat: true }));
  assert.equal(held.effect.kind, "none");
  assert.equal(held.state.armed ?? null, null);
  const armed = M.onKey(cards, fresh, key("2", { at: 1000 })).state;
  const repeat = M.onKey(cards, armed, key("2", { at: 1100, repeat: true }));
  assert.equal(repeat.effect.kind, "none");
  assert.deepEqual(repeat.state.armed, armed.armed);
  assert.equal(M.onKey(cards, armed, key("Enter", { at: 1100 })).effect.kind, "pick");
  assert.equal(M.fromQt(0x32, "2", 0, false, null, true).repeat, true);
  assert.equal(M.fromQt(0x32, "2", 0, false, null).repeat, false);
});

test("a digit only arms a confirm option: the same digit again is a no-op and Enter completes it", () => {
  const cards = [externalCard()];
  const fresh = { selected: "m1/e", replyFor: null };
  const armed = M.onKey(cards, fresh, key("2", { at: 1000 })).state;
  for (const at of [1001, 1500, 2000]) {
    const again = M.onKey(cards, armed, key("2", { at }));
    assert.equal(again.effect.kind, "none", "digit at " + at);
    assert.deepEqual(again.state.armed, armed.armed, "still armed, unmoved");
  }
  assert.deepEqual(M.onKey(cards, armed, key("Enter", { at: 1600 })).effect, { kind: "pick", cardKey: "m1/e", requestId: "r1", optionId: "abandon", revision: 1 });
  assert.equal(M.onKey(cards, fresh, key("Enter", { at: 1600 })).effect.kind, "none", "Enter alone sends nothing");
  const other = M.onKey(cards, armed, key("1", { at: 1100 }));
  assert.equal(other.effect.optionId, "go");
  assert.equal(M.fromQt(0, "\r", 0, false, null).key, "Enter", "a catcher text key of Return is Enter");
});

test("the same digit re-arms a confirm option once the window has passed", () => {
  const cards = [externalCard()];
  const armed = M.onKey(cards, { selected: "m1/e", replyFor: null }, key("2", { at: 1000 })).state;
  const later = 1000 + M.CONFIRM_MS + 1;
  const rearmed = M.onKey(cards, armed, key("2", { at: later })).state;
  assert.equal(rearmed.armed.at, later);
  assert.equal(M.onKey(cards, rearmed, key("Enter", { at: later + 100 })).effect.kind, "pick");
});

test("the armed row tells the user to press Enter", () => {
  const panel = fs.readFileSync(new URL("../Panel.qml", import.meta.url), "utf8");
  assert.match(panel, /Press Enter to confirm: /);
});

test("an unrelated click drops the arm, and every Panel.qml click but the option row goes through disarm", () => {
  const cards = [externalCard()];
  const armed = M.onKey(cards, { selected: "m1/e", replyFor: null }, key("2", { at: 1000 })).state;
  const after = M.disarm(armed);
  assert.equal(after.armed, null);
  assert.equal(after.selected, "m1/e");
  assert.equal(M.disarm(after), after, "nothing to drop");
  assert.equal(M.pickOption(after, cards[0], "abandon", 1100).effect.kind, "none", "the next tap arms again");
  const panel = fs.readFileSync(new URL("../Panel.qml", import.meta.url), "utf8");
  const clicks = panel.split("\n").filter((l) => l.includes("onClicked:"));
  assert.ok(clicks.length >= 10);
  for (const l of clicks) {
    if (l.includes("pickOption")) continue;
    assert.match(l, /disarm\(\)|onClicked: \{\s*$/, l.trim());
  }
  assert.match(panel, /onClicked: \{\n\s*panel\.disarm\(\);/);
});

test("a question card shows its header and text beyond the detail", () => {
  const c = externalCard();
  c.agent.pending.questions = [{ id: "q1", header: "Parked", text: "What now?", multi_select: false, options: [] }];
  assert.deepEqual(M.questionLines(c), { header: "Parked", text: "What now?" });
  c.agent.pending.detail = "What now?";
  assert.deepEqual(M.questionLines(c), { header: "Parked", text: null }, "the text the detail already holds is not repeated");
  c.agent.pending.questions = [{ id: "q1", header: "Question", text: "What now?", multi_select: false, options: [] }];
  assert.equal(M.questionLines(c), null);
  assert.equal(M.questionLines(card("m1/a")), null, "a permission has no question");
  const none = externalCard();
  none.agent.pending = null;
  assert.equal(M.questionLines(none), null);
  const panel = fs.readFileSync(new URL("../Panel.qml", import.meta.url), "utf8");
  assert.match(panel, /Model\.questionLines\(cardItem\.card\)/);
});
