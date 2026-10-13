import type { InboxView } from "./bindings/InboxView";

/** How long an option that needs a second tap stays armed. */
export const CONFIRM_MS = 5000;
/** An option with `confirm` waiting for its second tap, fixed to the request the user saw. */
export type Armed = {
  cardKey: string;
  requestId: string;
  revision: number;
  optionId: string;
  at: number;
};
export type KeyState = { selected: string | null; replyFor: string | null; armed?: Armed | null };
export type KeyInput = {
  key: string;
  shift: boolean;
  meta: boolean;
  ctrl: boolean;
  alt: boolean;
  composing: boolean;
  keyCode: number;
  /** Event time in ms; absent means 0. */
  at?: number;
  inField: boolean;
  /** Which text field has focus: the reply box, a card's typed-answer input, or the voice command field. */
  field?: { kind: "reply" | "answer" | "voice"; cardKey: string | null };
  /** The typed voice command field is on screen. */
  voiceField?: boolean;
};
export type Effect =
  | { kind: "none" }
  | { kind: "select"; key: string }
  | {
      kind: "pick";
      cardKey: string;
      requestId: string;
      optionId: string;
      /** Revision of the request the user saw when the effect was created. */
      revision: number;
    }
  | { kind: "openReply"; cardKey: string }
  | { kind: "submitReply"; cardKey: string }
  | { kind: "submitAnswer"; cardKey: string }
  | { kind: "focusAnswer"; cardKey: string }
  | { kind: "markSeen"; cardKey: string }
  | { kind: "submitVoice" }
  | { kind: "focusVoice" }
  | { kind: "escape" };

const NONE: Effect = { kind: "none" };

export function fromEvent(e: KeyboardEvent): KeyInput {
  const target = e.target as HTMLElement | null;
  const kind = target?.dataset?.field;
  return {
    key: e.key,
    shift: e.shiftKey,
    meta: e.metaKey,
    ctrl: e.ctrlKey,
    alt: e.altKey,
    composing: e.isComposing,
    keyCode: e.keyCode,
    at: Date.now(),
    inField: target?.tagName === "TEXTAREA",
    field:
      kind === "reply" || kind === "answer" || kind === "voice"
        ? { kind, cardKey: target?.closest<HTMLElement>("[data-key]")?.dataset.key ?? null }
        : undefined,
    voiceField: document.querySelector("[data-field=voice]") !== null,
  };
}

/** Whether `armed` still names an option of the request on screen. */
export function armedHolds(view: InboxView, armed: Armed | null | undefined): armed is Armed {
  if (!armed) return false;
  const pending = view.cards.find((c) => c.key === armed.cardKey)?.agent.pending;
  return (
    pending !== undefined &&
    pending !== null &&
    pending.id === armed.requestId &&
    pending.revision === armed.revision &&
    pending.options.some((o) => o.id === armed.optionId)
  );
}

/** Keep the selection on a card that still exists; fall back to the first. */
export function normalize(view: InboxView, st: KeyState): KeyState {
  const keys = view.cards.map((c) => c.key);
  const selected =
    st.selected !== null && keys.includes(st.selected) ? st.selected : (keys[0] ?? null);
  const replyFor = st.replyFor !== null && keys.includes(st.replyFor) ? st.replyFor : null;
  const armed = armedHolds(view, st.armed) ? st.armed : null;
  if (selected === st.selected && replyFor === st.replyFor && armed === (st.armed ?? null)) return st;
  return armed === null ? { selected, replyFor } : { selected, replyFor, armed };
}

export function onKey(
  view: InboxView,
  st: KeyState,
  ev: KeyInput,
): { state: KeyState; effect: Effect } {
  const none = { state: st, effect: NONE };
  // Guards first: IME composition and modifiers are never shortcuts.
  if (ev.composing || ev.keyCode === 229 || ev.meta || ev.ctrl || ev.alt) return none;

  // Any key but the second tap of an armed option disarms it.
  const rest: KeyState = st.armed ? { ...st, armed: null } : st;

  if (ev.key === "Escape") return { state: rest, effect: { kind: "escape" } };

  if (ev.inField) {
    if (ev.key === "Enter" && !ev.shift && ev.field?.kind === "voice") {
      return { state: rest, effect: { kind: "submitVoice" } };
    }
    if (ev.key === "Enter" && !ev.shift && ev.field?.kind === "answer" && ev.field.cardKey !== null) {
      return { state: rest, effect: { kind: "submitAnswer", cardKey: ev.field.cardKey } };
    }
    if (ev.key === "Enter" && !ev.shift && ev.field?.kind !== "answer" && st.replyFor !== null) {
      return { state: rest, effect: { kind: "submitReply", cardKey: st.replyFor } };
    }
    return { state: rest, effect: NONE };
  }

  if (ev.key === "/" && ev.voiceField) return { state: rest, effect: { kind: "focusVoice" } };

  const key = ev.key.length === 1 ? ev.key.toLowerCase() : ev.key;
  const idx = view.cards.findIndex((c) => c.key === st.selected);
  const card = idx >= 0 ? view.cards[idx] : undefined;

  if (key === "j" || key === "ArrowDown" || key === "k" || key === "ArrowUp") {
    if (view.cards.length === 0) return none;
    const down = key === "j" || key === "ArrowDown";
    const next = idx < 0 ? 0 : Math.min(view.cards.length - 1, Math.max(0, idx + (down ? 1 : -1)));
    const selected = view.cards[next].key;
    return {
      state: { ...rest, selected },
      effect: selected === st.selected ? NONE : { kind: "select", key: selected },
    };
  }

  if (!card) return { state: rest, effect: NONE };

  if (/^[1-9]$/.test(key)) {
    const pending = card.agent.pending;
    const idle = { state: rest, effect: NONE };
    if (!card.answerable || !pending) return idle;
    const option = pending.options[Number(key) - 1];
    if (!option || !card.selectable_options.includes(option.id)) return idle;
    const at = ev.at ?? 0;
    const second =
      st.armed?.cardKey === card.key &&
      st.armed.requestId === pending.id &&
      st.armed.revision === pending.revision &&
      st.armed.optionId === option.id &&
      at - st.armed.at <= CONFIRM_MS;
    if (option.confirm && !second) {
      const armed: Armed = {
        cardKey: card.key,
        requestId: pending.id,
        revision: pending.revision,
        optionId: option.id,
        at,
      };
      return { state: { ...st, armed }, effect: NONE };
    }
    return {
      state: rest,
      effect: {
        kind: "pick",
        cardKey: card.key,
        requestId: pending.id,
        optionId: option.id,
        revision: pending.revision,
      },
    };
  }
  if (key === "t" && card.free_text_answerable) {
    return { state: rest, effect: { kind: "focusAnswer", cardKey: card.key } };
  }
  if (key === "r" && card.can_reply) {
    return { state: rest, effect: { kind: "openReply", cardKey: card.key } };
  }
  if (key === "e" && card.can_mark_seen) {
    return { state: rest, effect: { kind: "markSeen", cardKey: card.key } };
  }
  return { state: rest, effect: NONE };
}
