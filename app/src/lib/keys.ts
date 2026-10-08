import type { InboxView } from "./bindings/InboxView";

export type KeyState = { selected: string | null; replyFor: string | null };
export type KeyInput = {
  key: string;
  shift: boolean;
  meta: boolean;
  ctrl: boolean;
  alt: boolean;
  composing: boolean;
  keyCode: number;
  inField: boolean;
};
export type Effect =
  | { kind: "none" }
  | { kind: "select"; key: string }
  | { kind: "pick"; cardKey: string; requestId: string; optionId: string }
  | { kind: "openReply"; cardKey: string }
  | { kind: "submitReply"; cardKey: string }
  | { kind: "markSeen"; cardKey: string }
  | { kind: "escape" };

const NONE: Effect = { kind: "none" };

export function fromEvent(e: KeyboardEvent): KeyInput {
  const target = e.target as HTMLElement | null;
  return {
    key: e.key,
    shift: e.shiftKey,
    meta: e.metaKey,
    ctrl: e.ctrlKey,
    alt: e.altKey,
    composing: e.isComposing,
    keyCode: e.keyCode,
    inField: target?.tagName === "TEXTAREA",
  };
}

/** Keep the selection on a card that still exists; fall back to the first. */
export function normalize(view: InboxView, st: KeyState): KeyState {
  const keys = view.cards.map((c) => c.key);
  const selected =
    st.selected !== null && keys.includes(st.selected) ? st.selected : (keys[0] ?? null);
  const replyFor = st.replyFor !== null && keys.includes(st.replyFor) ? st.replyFor : null;
  if (selected === st.selected && replyFor === st.replyFor) return st;
  return { selected, replyFor };
}

export function onKey(
  view: InboxView,
  st: KeyState,
  ev: KeyInput,
): { state: KeyState; effect: Effect } {
  const none = { state: st, effect: NONE };
  // Guards first: IME composition and modifiers are never shortcuts.
  if (ev.composing || ev.keyCode === 229 || ev.meta || ev.ctrl || ev.alt) return none;

  if (ev.key === "Escape") return { state: st, effect: { kind: "escape" } };

  if (ev.inField) {
    if (ev.key === "Enter" && !ev.shift && st.replyFor !== null) {
      return { state: st, effect: { kind: "submitReply", cardKey: st.replyFor } };
    }
    return none;
  }

  const key = ev.key.length === 1 ? ev.key.toLowerCase() : ev.key;
  const idx = view.cards.findIndex((c) => c.key === st.selected);
  const card = idx >= 0 ? view.cards[idx] : undefined;

  if (key === "j" || key === "ArrowDown" || key === "k" || key === "ArrowUp") {
    if (view.cards.length === 0) return none;
    const down = key === "j" || key === "ArrowDown";
    const next = idx < 0 ? 0 : Math.min(view.cards.length - 1, Math.max(0, idx + (down ? 1 : -1)));
    const selected = view.cards[next].key;
    return {
      state: { ...st, selected },
      effect: selected === st.selected ? NONE : { kind: "select", key: selected },
    };
  }

  if (!card) return none;

  if (/^[1-9]$/.test(key)) {
    const pending = card.agent.pending;
    if (!card.answerable || !pending) return none;
    const option = pending.options[Number(key) - 1];
    if (!option || !card.selectable_options.includes(option.id)) return none;
    return {
      state: st,
      effect: { kind: "pick", cardKey: card.key, requestId: pending.id, optionId: option.id },
    };
  }
  if (key === "r" && card.can_reply) {
    return { state: st, effect: { kind: "openReply", cardKey: card.key } };
  }
  if (key === "e" && card.can_mark_seen) {
    return { state: st, effect: { kind: "markSeen", cardKey: card.key } };
  }
  return none;
}
