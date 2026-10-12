import test from "node:test";
import assert from "node:assert/strict";
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
