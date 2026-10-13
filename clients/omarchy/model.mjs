// Pure view logic for the Omarchy plugin (omarchy spec §7). No QML or I/O,
// so `node --test` covers it. Service.qml, BarWidget.qml and Panel.qml
// import it as `Model`.

export const MAX_LINE = 4 * 1024 * 1024;
export const ANSWER_MAX_BYTES = 16 * 1024;
export const REPLY_MAX_BYTES = 64 * 1024;
export const UNDO_MS = 2000;
const NONE = { kind: "none" };
export const UNCERTAIN_TEXT = "Check the terminal";
/** How long an option that needs a second tap stays armed. */
export const CONFIRM_MS = 5000;

/**
 * UTF-8 length of a JS string, in pure JS: QML's host has no TextEncoder
 * (plan D20). A lone surrogate counts 3, as its replacement character would.
 */
export function utf8Length(s) {
  let n = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x80) n += 1;
    else if (c < 0x800) n += 2;
    else if (c >= 0xd800 && c <= 0xdbff && i + 1 < s.length && (s.charCodeAt(i + 1) & 0xfc00) === 0xdc00) {
      n += 4;
      i += 1;
    } else n += 3;
  }
  return n;
}

// A UTF-16 unit is at least one UTF-8 byte and at most three.
const tooLong = (s, max) => s.length > max || (s.length * 3 > max && utf8Length(s) > max);

/**
 * Joins Quickshell SplitParser output into lines and enforces the 4 MiB
 * limit (plan D20). SplitParser splits bytes before decoding, so text
 * arrives as whole characters; this reader also copes with a line given in
 * pieces. On overflow the caller drops the connection.
 */
export class LineReader {
  constructor(max = MAX_LINE) {
    this.max = max;
    this.buf = "";
    this.dead = false;
  }

  overflowed(lines) {
    this.dead = true;
    this.buf = "";
    return { lines, overflow: true };
  }

  push(text) {
    // Overflow is final: each connection gets a new reader.
    if (this.dead) return { lines: [], overflow: true };
    this.buf += text;
    const lines = [];
    let i;
    while ((i = this.buf.indexOf("\n")) >= 0) {
      const line = this.buf.slice(0, i);
      this.buf = this.buf.slice(i + 1);
      if (tooLong(line, this.max)) return this.overflowed(lines);
      if (line.length > 0) lines.push(line);
    }
    return tooLong(this.buf, this.max) ? this.overflowed(lines) : { lines, overflow: false };
  }
}

export function parseMessage(line) {
  let v;
  try {
    v = JSON.parse(line);
  } catch (_e) {
    return null;
  }
  return v !== null && typeof v === "object" && !Array.isArray(v) && typeof v.type === "string" ? v : null;
}

export class Ids {
  constructor(prefix) {
    this.prefix = prefix;
    this.n = 0;
  }

  next() {
    this.n += 1;
    return `${this.prefix}-${this.n}`;
  }
}

/** The three sections, as app/src/lib/panel.ts: a countdown card stays whatever the machine filter says. */
export function sections(cards, machine) {
  const shown = machine === null ? cards : cards.filter((c) => c.machine_key === machine || c.action !== null);
  return {
    needs: shown.filter((c) => c.group === "request"),
    working: shown.filter((c) => c.group === "working" || c.group === "unknown"),
    done: shown.filter((c) => c.group === "done" || c.group === "idle"),
  };
}

export function headerText(view) {
  const working = sections(view.cards, null).working.length;
  return `Inbox · ${view.tray.needs_you} need you · ${working} working`;
}

export function barState(view, connected, overflow = -1) {
  if (!connected) return { count: 0, tone: "down", text: "" };
  // A view_overflow carries only the count of agents that need the user.
  const n = view ? view.tray.needs_you : overflow;
  if (n === undefined || n === null || n < 0) return { count: 0, tone: "down", text: "" };
  return { count: n, tone: n > 0 ? "urgent" : "dim", text: n > 0 ? String(n) : "" };
}

/** The bar label: a struck glyph when unreachable, else the glyph and the count. */
export function barLabel(bar) {
  if (bar.tone === "down") return "\u2298";
  return bar.count > 0 ? `\u25c9 ${bar.text}` : "\u25c9";
}

/** Reconnect delays double to a cap. Only a valid hello resets them (a service that accepts and drops must not be retried at 2 Hz). */
export class Backoff {
  constructor(start = 500, cap = 10000) {
    this.start = start;
    this.cap = cap;
    this.ms = start;
  }

  next() {
    const d = this.ms;
    this.ms = Math.min(this.ms * 2, this.cap);
    return d;
  }

  reset() {
    this.ms = this.start;
  }
}

function dotKind(status) {
  switch (status.kind) {
    case "online":
      return "online";
    case "connecting":
    case "syncing":
      return "syncing";
    case "offline":
      return "offline";
    default:
      return "error";
  }
}

const CHIP_TEXT = {
  online: "Online",
  syncing: "Connecting",
  offline: "Offline",
  error: "Error",
  locked: "Keyring locked",
  not_paired: "Not paired",
  unreadable: "Pair again",
  unknown: "Keyring state unknown. Retry",
};

/** Machine chips: connection status from the view, joined by key with secret status from settings (plan S1, S6). */
export function machineChips(view, settings) {
  const entries = new Map((settings?.machines ?? []).map((m) => [m.key, m]));
  return (view?.machines ?? [])
    .filter((m) => m.duplicate_of === null)
    .map((m) => {
      let status = dotKind(m.status);
      const e = entries.get(m.key);
      if (e && e.endpoint.kind === "tcp") {
        if (e.secret === "locked") status = "locked";
        else if (e.secret === "missing") status = "not_paired";
        else if (e.secret === "unreadable") status = "unreadable";
        else if (e.secret === "unknown") status = "unknown";
      }
      return { key: m.key, name: m.name, status, label: CHIP_TEXT[status] };
    });
}

export function keyringLocked(settings) {
  return (settings?.machines ?? []).some((m) => m.secret === "locked");
}

const TOOL_NAME = /^[A-Za-z][A-Za-z0-9_.:-]{0,39}$/;

export function requestTag(card) {
  const p = card.agent.pending;
  if (!p) return "NEEDS YOU";
  switch (p.kind) {
    case "permission":
      return TOOL_NAME.test(p.title) ? `${p.title.toUpperCase()} APPROVAL` : "APPROVAL";
    case "question":
      return "QUESTION";
    case "blocked_unparsed":
      return "BLOCKED";
    default:
      return "REQUEST";
  }
}

const KIND = { claude: "Claude", codex: "Codex" };

export function cardMeta(card) {
  const a = card.agent;
  const project = a.project ?? (a.cwd ? a.cwd.split("/").filter(Boolean).pop() ?? null : null);
  // An external producer names itself through the agent title, so the heading says what is asked.
  const external = a.source === "external";
  const badge = a.source === "herdr" ? "herdr" : a.source === "t3" ? "T3" : external ? a.title : null;
  const title = external && a.pending ? a.pending.title : a.title;
  return { kind: KIND[a.kind] ?? "agent", machine: card.machine_name, project, title, badge, tag: requestTag(card) };
}

/** Whether `armed` still names an option of the request this card shows. */
export function armedHolds(armed, card) {
  const p = card.agent.pending;
  return !!armed && !!p && card.key === armed.cardKey && p.id === armed.requestId && p.revision === armed.revision && p.options.some((o) => o.id === armed.optionId);
}

/**
 * Options as the panel lists them (plan D30). `armed` and `now` mark the
 * option of this card that waits for its second tap.
 */
export function optionRows(card, armed = null, now = 0) {
  const p = card.agent.pending;
  if (!p) return [];
  const live = armedHolds(armed, card) && now - armed.at <= CONFIRM_MS;
  return p.options.map((o, i) => {
    const rule = o.effect === "allow_always" && o.description?.trim() ? o.description : null;
    const pickable =
      card.answerable && i < 9 && card.selectable_options.includes(o.id) && (o.effect !== "allow_always" || rule !== null);
    const confirm = o.confirm === true;
    return { number: i + 1, id: o.id, label: o.label, rule, pickable, confirm, armed: pickable && confirm && live && armed.optionId === o.id };
  });
}

/**
 * Picking an option: one that needs confirmation arms on the first tap and
 * is sent by the second, on the same request and revision, within
 * `CONFIRM_MS`. Anything else the user does in between disarms it.
 */
export function pickOption(st, card, optionId, at) {
  const rest = st.armed ? Object.assign({}, st, { armed: null }) : st;
  const p = card.agent.pending;
  const row = optionRows(card).find((r) => r.id === optionId);
  if (!p || !row || !row.pickable) return { state: rest, effect: NONE };
  const second = armedHolds(st.armed, card) && st.armed.optionId === optionId && at - st.armed.at <= CONFIRM_MS;
  if (row.confirm && !second) {
    const armed = { cardKey: card.key, requestId: p.id, revision: p.revision, optionId, at };
    return { state: Object.assign({}, st, { armed }), effect: NONE };
  }
  return { state: rest, effect: { kind: "pick", cardKey: card.key, requestId: p.id, optionId, revision: p.revision } };
}

/**
 * A typed-answer editor remembers the request it was opened for (spec §7):
 * the answer goes to that request id and revision, never to a later one.
 */
export function answerEditor(card) {
  const p = card.agent.pending;
  if (!card.free_text_answerable || !p) return null;
  return { cardKey: card.key, requestId: p.id, revision: p.revision };
}

/** False once the card is gone, its request changed, or it no longer takes a typed answer: the editor closes. */
export function answerEditorValid(editor, cards) {
  if (!editor) return false;
  const card = cards.find((c) => c.key === editor.cardKey);
  const p = card?.agent.pending;
  return !!card && card.free_text_answerable && !!p && p.id === editor.requestId && p.revision === editor.revision;
}

export const EDITOR_CLOSED_TEXT = "The request changed. Your answer was not sent.";

/** The error state for a view the service could not fit (plan D9). */
export function overflowText(needsYou) {
  return `Too many requests to show here (${needsYou} need you). Answer some in the terminal.`;
}

/** Draft keys (plan D35): a draft belongs to a card and its request, or to a machine. */
export const draftKey = {
  answer: (editor) => `answer:${editor.cardKey}:${editor.requestId}:${editor.revision}`,
  reply: (card) => `reply:${card.key}:${card.agent.pending ? card.agent.pending.id : "-"}`,
  rename: (machineKey) => `rename:${machineKey}`,
};

/**
 * Typed text kept outside QML delegates, which a new view recreates
 * (plan D35). The pairing line is never stored here.
 */
export class Drafts {
  constructor() {
    this.texts = new Map();
  }

  get(key) {
    return this.texts.get(key) ?? "";
  }

  set(key, text) {
    if (text === "") this.texts.delete(key);
    else this.texts.set(key, text);
  }

  clear(key) {
    this.texts.delete(key);
  }

  /** Drops drafts whose card, request or machine is gone. */
  prune(view, settings) {
    const live = new Set();
    for (const c of view?.cards ?? []) {
      live.add(draftKey.reply(c));
      const e = answerEditor(c);
      if (e) live.add(draftKey.answer(e));
    }
    for (const m of settings?.machines ?? []) live.add(draftKey.rename(m.key));
    for (const key of [...this.texts.keys()]) if (!live.has(key)) this.texts.delete(key);
  }
}

/** "N not shown" for the cards a large view left out (plan D9). */
export function omittedText(omitted) {
  const parts = [];
  if (omitted?.done > 0) parts.push(`${omitted.done} done`);
  if (omitted?.working > 0) parts.push(`${omitted.working} working`);
  return parts.length === 0 ? "" : `${parts.join(" and ")} not shown`;
}

export function countdownFraction(action, nowMs) {
  return Math.max(0, Math.min(1, (action.fires_at_ms - nowMs) / UNDO_MS));
}

/** Qt enum values (Qt::Key, Qt::KeyboardModifier). */
export const QT = {
  Key_Escape: 0x01000000,
  Key_Return: 0x01000004,
  Key_Enter: 0x01000005,
  Key_Up: 0x01000013,
  Key_Down: 0x01000015,
  ShiftModifier: 0x02000000,
  ControlModifier: 0x04000000,
  AltModifier: 0x08000000,
  MetaModifier: 0x10000000,
};

/** `field`: null, or {kind: "reply"|"answer"|"pairing", cardKey}. `composing`: the focused field's inputMethodComposing. */
export function fromQt(key, text, modifiers, composing, field, repeat = false) {
  const name =
    key === QT.Key_Escape ? "Escape"
    : key === QT.Key_Return || key === QT.Key_Enter || text === "\r" || text === "\n" ? "Enter"
    : key === QT.Key_Up ? "ArrowUp"
    : key === QT.Key_Down ? "ArrowDown"
    : text;
  return {
    key: name,
    shift: (modifiers & QT.ShiftModifier) !== 0,
    ctrl: (modifiers & QT.ControlModifier) !== 0,
    alt: (modifiers & QT.AltModifier) !== 0,
    meta: (modifiers & QT.MetaModifier) !== 0,
    at: Date.now(),
    repeat: !!repeat,
    composing: !!composing,
    field: field ?? null,
  };
}

/** Keys (omarchy spec §7), as app/src/lib/keys.ts, over the cards in display order. */
export function onKey(visible, st, ev) {
  const none = { state: st, effect: NONE };
  if (ev.composing || ev.ctrl || ev.alt || ev.meta) return none;
  // Any key but the second tap of an armed option disarms it.
  const rest = st.armed ? Object.assign({}, st, { armed: null }) : st;
  if (ev.key === "Escape") return { state: rest, effect: { kind: "escape" } };
  if (ev.field) {
    if (ev.key !== "Enter" || ev.shift) return { state: rest, effect: NONE };
    if (ev.field.kind === "answer" && ev.field.cardKey) return { state: rest, effect: { kind: "submitAnswer", cardKey: ev.field.cardKey } };
    if (ev.field.kind === "reply" && st.replyFor) return { state: rest, effect: { kind: "submitReply", cardKey: st.replyFor } };
    if (ev.field.kind === "pairing") return { state: rest, effect: { kind: "submitPairing" } };
    return { state: rest, effect: NONE };
  }
  const k = ev.key.length === 1 ? ev.key.toLowerCase() : ev.key;
  // The host's key catcher reports no auto-repeat, so a digit only arms a
  // confirm option; Enter completes it. A digit pressed again is a no-op.
  if (k === "Enter" && st.armed) {
    const armedCard = visible.find((c) => c.key === st.armed.cardKey);
    const row = armedCard ? optionRows(armedCard).find((r) => r.id === st.armed.optionId) : undefined;
    if (armedCard && row && row.pickable && row.confirm && armedHolds(st.armed, armedCard) && (ev.at ?? 0) - st.armed.at <= CONFIRM_MS) {
      return pickOption(st, armedCard, row.id, st.armed.at);
    }
    return { state: rest, effect: NONE };
  }
  const idx = visible.findIndex((c) => c.key === st.selected);
  if (k === "j" || k === "ArrowDown" || k === "k" || k === "ArrowUp") {
    if (visible.length === 0) return none;
    const down = k === "j" || k === "ArrowDown";
    const next = idx < 0 ? 0 : Math.min(visible.length - 1, Math.max(0, idx + (down ? 1 : -1)));
    const selected = visible[next].key;
    return { state: Object.assign({}, rest, { selected }), effect: selected === st.selected ? NONE : { kind: "select", key: selected } };
  }
  const card = idx >= 0 ? visible[idx] : undefined;
  if (!card) return { state: rest, effect: NONE };
  if (/^[1-9]$/.test(k)) {
    const row = optionRows(card).find((r) => r.number === Number(k));
    if (!row) return { state: rest, effect: NONE };
    if (row.confirm) {
      if (st.armed && armedHolds(st.armed, card) && st.armed.optionId === row.id && (ev.at ?? 0) - st.armed.at <= CONFIRM_MS) return { state: st, effect: NONE };
      if (ev.repeat) return { state: st, effect: NONE };
      return pickOption(Object.assign({}, st, { armed: null }), card, row.id, ev.at ?? 0);
    }
    return pickOption(st, card, row.id, ev.at ?? 0);
  }
  if (k === "t" && card.free_text_answerable) return { state: rest, effect: { kind: "focusAnswer", cardKey: card.key } };
  if (k === "r" && card.can_reply) return { state: rest, effect: { kind: "openReply", cardKey: card.key } };
  if (k === "e" && card.can_mark_seen) return { state: rest, effect: { kind: "markSeen", cardKey: card.key } };
  return { state: rest, effect: NONE };
}

// acd_protocol::text: controls other than \n and \t, bidi controls, line and paragraph separators.
const FORBIDDEN = /[\u0000-\u0008\u000B-\u001F\u007F-\u009F\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069\u2028\u2029]/u;

/** As Inbox::answer_text: CRLF to LF, trimmed, 1 byte to 16 KiB, no forbidden characters. */
export function validateAnswer(text) {
  const t = text.replace(/\r\n/g, "\n").trim();
  if (!t) return { ok: false, reason: "empty" };
  if (utf8Length(t) > ANSWER_MAX_BYTES) return { ok: false, reason: "too_long" };
  if (FORBIDDEN.test(t)) return { ok: false, reason: "characters" };
  return { ok: true, text: t };
}

/** As Inbox::reply: not blank, at most 64 KiB; sent as typed. */
export function validateReply(text) {
  if (!text.trim()) return { ok: false, reason: "empty" };
  if (utf8Length(text) > REPLY_MAX_BYTES) return { ok: false, reason: "too_long" };
  return { ok: true, text };
}

const REASONS = {
  empty: "Type something first.",
  too_long: "That is too long to send.",
  characters: "The text has characters that cannot be sent.",
};

export function reasonText(reason) {
  return REASONS[reason] ?? "That cannot be sent.";
}

const UPDATE = "The inbox service did not understand that. Update the plugin or the service.";
const ERRORS = {
  stale: "The request changed. Review it again.",
  unknown_card: "That agent is gone.",
  not_answerable: "Answer this one in the terminal.",
  unknown_option: "That option cannot be picked here.",
  already_counting: "A countdown is already running for this card.",
  bad_text: "The text is empty, too long, or has characters that cannot be sent.",
  cannot_reply: "This agent cannot take a reply right now.",
  cannot_mark_seen: "This agent cannot be marked seen.",
  hidden: "The agent is hidden now. Nothing was sent.",
  unknown_machine: "That machine is not in the list.",
  not_paired: "This machine is not paired. Paste its pairing line.",
  bad_name: "Names are 1 to 64 characters.",
  bad_pairing: "That is not an acd pairing line. Run `acd pair` on the machine and copy its output.",
  bad_request: UPDATE,
  secret_store_locked: "The keyring is locked. Unlock it and try again.",
  secret_store_timeout: "The keyring is not responding. Press Retry on the machine once it answers.",
  secret_store_failed: "The keyring could not be used.",
  settings_failed: "Could not save the settings.",
  duplicate_id: "That was already sent.",
  unsupported: UPDATE,
  unreachable: "The inbox service is not running. Start it with: systemctl --user start acd-inbox",
};

export function errorText(code) {
  return ERRORS[code] ?? "Something went wrong.";
}

/** This view's actions without a terminal outcome yet. */
export class Tracker {
  constructor() {
    this.actions = new Map();
    this.fired = new Set();
    // Ended actions whose reply may still be in flight: the outcome can be
    // written before the `ok` that names the action.
    this.ended = new Set();
    // Actions whose outcome was lost: only their own terminal outcome
    // settles the card they marked uncertain.
    this.lostIds = new Set();
  }

  started(actionId, cardKey) {
    if (this.ended.has(actionId)) return;
    this.actions.set(actionId, cardKey);
  }

  outcome(msg) {
    if (msg.result.kind === "fired") {
      // Other views' outcomes arrive here too: `fired` counts for this view
      // only once `started` names the action (an outcome that beat its
      // reply is picked up there).
      this.fired.add(msg.action);
    } else {
      this.actions.delete(msg.action);
      this.fired.delete(msg.action);
      this.ended.add(msg.action);
      // Action ids are unique; a long run must not grow this without bound.
      if (this.ended.size > 256) this.ended.delete(this.ended.values().next().value);
    }
  }

  /** outcomes_lost: every action still open is uncertain; returns their cards. */
  lost() {
    const cards = [...new Set(this.actions.values())];
    for (const id of this.actions.keys()) this.lostIds.add(id);
    while (this.lostIds.size > 256) this.lostIds.delete(this.lostIds.values().next().value);
    this.actions.clear();
    this.fired.clear();
    return cards;
  }

  /** Whether `msg` is the terminal outcome of an action whose outcome was lost. */
  settles(msg) {
    return msg.result.kind !== "fired" && this.lostIds.delete(msg.action);
  }

  /**
   * The connection dropped. The service cancels this view's countdowns when
   * it closes, so only an action already fired can have a result we never
   * saw; returns those cards. The rest are forgotten.
   */
  disconnected() {
    const known = [...this.fired].filter((id) => this.actions.has(id));
    const cards = [...new Set(known.map((id) => this.actions.get(id)))];
    // Their results can still arrive on the next connection and settle the card.
    for (const id of known) this.lostIds.add(id);
    while (this.lostIds.size > 256) this.lostIds.delete(this.lostIds.values().next().value);
    this.actions.clear();
    this.fired.clear();
    return cards;
  }
}

/**
 * What submitting a typed answer does (spec §7, as Panel.svelte). The answer
 * goes to the request the editor was opened for, never the card's current
 * one. `none`: nothing to send and the text stays (a countdown runs or the
 * card is busy). `closed`: the request changed, so the editor closes and the
 * text is dropped. `invalid`: the text cannot be sent.
 */
export function planAnswer(editor, cards, text) {
  if (!answerEditorValid(editor, cards)) return { kind: "closed" };
  const card = cards.find((c) => c.key === editor.cardKey);
  if (card.action !== null || card.busy) return { kind: "none" };
  const r = validateAnswer(text);
  if (!r.ok) return { kind: "invalid", reason: r.reason };
  return { kind: "send", fields: { card: editor.cardKey, request: editor.requestId, revision: editor.revision, text: r.text } };
}

/** A reply box belongs to its card: it closes when the card leaves the view. */
export function pruneKeyState(st, cards) {
  let next = st;
  if (next.replyFor !== null && !cards.some((c) => c.key === next.replyFor)) next = Object.assign({}, next, { replyFor: null });
  if (next.armed && !armedHolds(next.armed, cards.find((c) => c.key === next.armed.cardKey) ?? { key: "", agent: { pending: null } })) {
    next = Object.assign({}, next, { armed: null });
  }
  return next;
}

/** Any action other than completing a confirm option drops the arm. */
export function disarm(st) {
  return st.armed ? Object.assign({}, st, { armed: null }) : st;
}

/** `openCard`: select that card and close any reply box. */
export function selectCard(key) {
  return { selected: key, replyFor: null };
}

/** The service answers `escape` with true when it cancelled this panel's newest countdown; else the panel closes. */
export function escapeCloses(reply) {
  return !(reply.type === "ok" && reply.result === true);
}

/** Identity of the text field that has focus, kept by the panel because every new view rebuilds the delegates. */
export const fieldId = {
  reply: (cardKey) => `reply:${cardKey}`,
  answer: (cardKey) => `answer:${cardKey}`,
  rename: (machineKey) => `rename:${machineKey}`,
};

/** True while the field `id` should exist and so may keep focus: its reply box, editor or machine is still there. */
export function fieldFocusValid(id, st, editor, settings) {
  const i = id.indexOf(":");
  if (i < 0) return false;
  const kind = id.slice(0, i);
  const key = id.slice(i + 1);
  if (kind === "reply") return st.replyFor === key;
  if (kind === "answer") return !!editor && editor.cardKey === key;
  if (kind === "rename") return (settings?.machines ?? []).some((m) => m.key === key);
  return false;
}

/** The editor's answer was accepted by the service; its text stays for an undo. */
export function markSent(editor) {
  return editor ? Object.assign({}, editor, { sent: true }) : editor;
}

/** Text shown when an editor closes because its request changed; a sent answer closes silently. */
export function editorClosedNotice(editor) {
  return editor && editor.sent ? "" : EDITOR_CLOSED_TEXT;
}

/** Whether the editor must close for this view. No view (disconnect, overflow) never closes it. */
export function editorShouldClose(editor, view) {
  return !!editor && view !== null && view !== undefined && !answerEditorValid(editor, view.cards);
}

const requestOf = (c) => (c.agent.pending ? c.agent.pending.id : "-");

/** Marks cards whose outcome is uncertain, each with the request it was uncertain for. */
export function markUncertain(prev, cards, keys) {
  const next = Object.assign({}, prev);
  for (const k of keys) {
    const c = cards.find((x) => x.key === k);
    next[k] = c ? requestOf(c) : "-";
  }
  return next;
}

/** A terminal outcome that arrives later settles a card marked uncertain. */
export function clearUncertain(marks, outcome) {
  if (outcome.result.kind === "fired" || !(outcome.card in marks)) return marks;
  const next = Object.assign({}, marks);
  delete next[outcome.card];
  return next;
}

/** The machine filter in force: none when its machine was removed (the inbox must not look empty). */
export function activeFilter(filter, settings) {
  if (filter === "") return null;
  // A bounded settings list may leave machines out: then absence proves nothing.
  if (settings && !(settings.omitted_machines > 0) && !settings.machines.some((m) => m.key === filter)) return null;
  return filter;
}

/** An uncertain mark ends when its card leaves the view or its request changes. */
export function pruneUncertain(marks, cards) {
  const next = {};
  for (const [k, req] of Object.entries(marks)) {
    const c = cards.find((x) => x.key === k);
    if (c && requestOf(c) === req) next[k] = req;
  }
  return Object.keys(next).length === Object.keys(marks).length ? marks : next;
}

/** `openCard` on a card the machine filter or a collapsed Done hides: show it. */
export function revealCard(key, cards, machineFilter, doneOpen) {
  const c = cards.find((x) => x.key === key);
  if (!c) return { machineFilter, doneOpen };
  const filter = machineFilter !== "" && c.machine_key !== machineFilter && c.action === null ? "" : machineFilter;
  const done = c.group === "done" || c.group === "idle" ? true : doneOpen;
  return { machineFilter: filter, doneOpen: done };
}
