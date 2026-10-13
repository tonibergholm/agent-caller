import { describe, expect, test } from "vitest";
import { CONFIRM_MS, onKey, normalize, type KeyInput } from "./keys";
import { view, card } from "./fixtures";

const k = (key: string, o: Partial<KeyInput> = {}): KeyInput =>
  ({ key, shift: false, meta: false, ctrl: false, alt: false, composing: false, keyCode: 0, inField: false, ...o });

describe("keyboard", () => {
  const a = card({ key: "m/a", answerable: true, selectable_options: ["allow", "deny", "always"] });
  const b = card({ key: "m/b", can_reply: true, can_mark_seen: true });
  const v = view({ cards: [a, b] });

  test("a pick effect is fixed to the revision of the view it was created from", () => {
    const old = card({ key: "m/a", answerable: true, selectable_options: ["allow"] });
    const newer = structuredClone(old);
    newer.agent.pending!.revision = old.agent.pending!.revision + 1;
    const st = { selected: "m/a", replyFor: null };
    const e1 = onKey(view({ cards: [old] }), st, k("1")).effect;
    const e2 = onKey(view({ cards: [newer] }), st, k("1")).effect;
    expect(e1).toMatchObject({ kind: "pick", revision: 1 });
    expect(e2).toMatchObject({ kind: "pick", revision: 2 });
  });

  test("j/k move and clamp; first key selects the first card", () => {
    let st = normalize(v, { selected: null, replyFor: null });
    expect(st.selected).toBe("m/a");
    st = onKey(v, st, k("j")).state; expect(st.selected).toBe("m/b");
    st = onKey(v, st, k("j")).state; expect(st.selected).toBe("m/b");
    st = onKey(v, st, k("K")).state; expect(st.selected).toBe("m/a");
  });

  test("digits pick only selectable options of an answerable card", () => {
    const st = { selected: "m/a", replyFor: null };
    expect(onKey(v, st, k("1")).effect).toEqual({ kind: "pick", cardKey: "m/a", requestId: a.agent.pending!.id, optionId: "allow", revision: a.agent.pending!.revision });
    expect(onKey(v, st, k("4")).effect).toEqual({ kind: "none" });   // "weird": unknown effect
    expect(onKey(v, { selected: "m/b", replyFor: null }, k("1")).effect).toEqual({ kind: "none" });
  });

  test("r and e follow the card's abilities", () => {
    expect(onKey(v, { selected: "m/b", replyFor: null }, k("r")).effect).toEqual({ kind: "openReply", cardKey: "m/b" });
    expect(onKey(v, { selected: "m/a", replyFor: null }, k("r")).effect).toEqual({ kind: "none" });
    expect(onKey(v, { selected: "m/b", replyFor: null }, k("e")).effect).toEqual({ kind: "markSeen", cardKey: "m/b" });
  });

  test("Escape is always the single escape effect", () => {
    expect(onKey(v, { selected: "m/a", replyFor: "m/b" }, k("Escape")).effect).toEqual({ kind: "escape" });
    expect(onKey(view({ cards: [] }), { selected: null, replyFor: null }, k("Escape")).effect).toEqual({ kind: "escape" });
  });

  test("Enter submits only inside the reply field; Shift+Enter is a newline", () => {
    const st = { selected: "m/b", replyFor: "m/b" };
    expect(onKey(v, st, k("Enter", { inField: true })).effect).toEqual({ kind: "submitReply", cardKey: "m/b" });
    expect(onKey(v, st, k("Enter", { inField: true, shift: true })).effect).toEqual({ kind: "none" });
    expect(onKey(v, { selected: "m/a", replyFor: null }, k("Enter")).effect).toEqual({ kind: "none" });
    expect(onKey(v, st, k("j", { inField: true })).effect).toEqual({ kind: "none" });
  });

  test("IME composition and modifiers are never shortcuts", () => {   // finding 8
    const st = { selected: "m/b", replyFor: "m/b" };
    expect(onKey(v, st, k("Enter", { inField: true, composing: true })).effect).toEqual({ kind: "none" });
    expect(onKey(v, st, k("Enter", { inField: true, keyCode: 229 })).effect).toEqual({ kind: "none" });
    expect(onKey(v, st, k("Escape", { inField: true, composing: true })).effect).toEqual({ kind: "none" });
    const sa = { selected: "m/a", replyFor: null };
    expect(onKey(v, sa, k("1", { meta: true })).effect).toEqual({ kind: "none" });
    expect(onKey(v, sa, k("1", { ctrl: true })).effect).toEqual({ kind: "none" });
    expect(onKey(v, sa, k("1", { alt: true })).effect).toEqual({ kind: "none" });
    expect(onKey(v, { selected: "m/b", replyFor: null }, k("e", { meta: true })).effect).toEqual({ kind: "none" });
  });

  test("Enter in an answer field submits that card's answer, whatever the reply state", () => {
    const st = { selected: "m/a", replyFor: null };
    const field = { kind: "answer" as const, cardKey: "m/q" };
    expect(onKey(v, st, k("Enter", { inField: true, field })).effect).toEqual({ kind: "submitAnswer", cardKey: "m/q" });
    expect(onKey(v, st, k("Enter", { inField: true, field, shift: true })).effect).toEqual({ kind: "none" });
    expect(onKey(v, st, k("Enter", { inField: true, field, composing: true })).effect).toEqual({ kind: "none" });
    expect(onKey(v, st, k("Enter", { inField: true, field, keyCode: 229 })).effect).toEqual({ kind: "none" });
    expect(onKey(v, st, k("t", { inField: true, field })).effect).toEqual({ kind: "none" });
    // An open reply box elsewhere does not steal an answer field's Enter.
    const replying = { selected: "m/b", replyFor: "m/b" };
    expect(onKey(v, replying, k("Enter", { inField: true, field })).effect).toEqual({ kind: "submitAnswer", cardKey: "m/q" });
  });

  test("T focuses the answer field of a free-text card and does nothing elsewhere", () => {
    const q = card({ key: "m/q", answerable: true, free_text_answerable: true });
    const vq = view({ cards: [a, q] });
    expect(onKey(vq, { selected: "m/q", replyFor: null }, k("t")).effect).toEqual({ kind: "focusAnswer", cardKey: "m/q" });
    expect(onKey(vq, { selected: "m/q", replyFor: null }, k("T")).effect).toEqual({ kind: "focusAnswer", cardKey: "m/q" });
    expect(onKey(vq, { selected: "m/a", replyFor: null }, k("t")).effect).toEqual({ kind: "none" });
    expect(onKey(vq, { selected: "m/q", replyFor: null }, k("t", { meta: true })).effect).toEqual({ kind: "none" });
  });

  test("selection survives reordering and falls back when its card disappears", () => {
    const st = { selected: "m/b", replyFor: null };
    expect(normalize(view({ cards: [b, a] }), st).selected).toBe("m/b");
    expect(normalize(view({ cards: [a] }), st).selected).toBe("m/a");
    expect(normalize(view({ cards: [] }), st).selected).toBe(null);
  });

  test("Enter in the voice command field submits it; its letters are not shortcuts", () => {
    const st = { selected: "m/a", replyFor: "m/b" };
    const voice = { inField: true, field: { kind: "voice" as const, cardKey: null } };
    expect(onKey(v, st, k("Enter", voice)).effect).toEqual({ kind: "submitVoice" });
    expect(onKey(v, st, k("Enter", { ...voice, shift: true })).effect).toEqual({ kind: "none" });
    expect(onKey(v, st, k("j", voice))).toEqual({ state: st, effect: { kind: "none" } });
    expect(onKey(v, st, k("1", voice)).effect).toEqual({ kind: "none" });
  });
});

describe("options that need a second tap", () => {
  const ext = card({ key: "m/e", answerable: true, selectable_options: ["go", "abandon"] });
  ext.agent.pending = {
    ...ext.agent.pending!,
    options: [
      { id: "go", label: "Go", description: null, effect: "choose" },
      { id: "abandon", label: "Abandon", description: null, effect: "choose", confirm: true },
    ],
  };
  const other = card({ key: "m/o", answerable: true, selectable_options: ["allow"] });
  const v = view({ cards: [ext, other] });
  const rid = ext.agent.pending!.id;
  const armedState = { selected: "m/e", replyFor: null, armed: { cardKey: "m/e", requestId: rid, revision: 1, optionId: "abandon", at: 1000 } };

  test("the first press arms and sends nothing", () => {
    const r = onKey(v, { selected: "m/e", replyFor: null }, k("2", { at: 1000 }));
    expect(r.effect).toEqual({ kind: "none" });
    expect(r.state.armed).toEqual(armedState.armed);
  });

  test("the second press on the same option sends it and disarms", () => {
    const r = onKey(v, armedState, k("2", { at: 2000 }));
    expect(r.effect).toEqual({ kind: "pick", cardKey: "m/e", requestId: rid, optionId: "abandon", revision: 1 });
    expect(r.state.armed ?? null).toBeNull();
  });

  test("a second press after the window only arms again", () => {
    const r = onKey(v, armedState, k("2", { at: 1000 + CONFIRM_MS + 1 }));
    expect(r.effect).toEqual({ kind: "none" });
    expect(r.state.armed?.at).toBe(1000 + CONFIRM_MS + 1);
  });

  test("any other key disarms: another option, movement, Escape", () => {
    expect(onKey(v, armedState, k("1", { at: 1500 }))).toMatchObject({ effect: { kind: "pick", optionId: "go" }, state: { armed: null } });
    expect(onKey(v, armedState, k("j", { at: 1500 })).state.armed).toBeNull();
    const esc = onKey(v, armedState, k("Escape", { at: 1500 }));
    expect(esc.effect).toEqual({ kind: "escape" });
    expect(esc.state.armed).toBeNull();
    const after = onKey(v, onKey(v, armedState, k("j", { at: 1500 })).state, k("2", { at: 1600 }));
    expect(after.effect).toEqual({ kind: "none" });
  });

  test("typing in a text field disarms", () => {
    const r = onKey(v, { ...armedState, replyFor: "m/e" }, k("x", { inField: true, at: 1100 }));
    expect(r.state.armed ?? null).toBeNull();
    expect(onKey(v, r.state, k("2", { at: 1200 })).effect).toEqual({ kind: "none" });
  });

  test("an armed option never sends for a different request or revision", () => {
    const stale = { ...armedState, armed: { ...armedState.armed, revision: 0 } };
    expect(onKey(v, stale, k("2", { at: 1100 })).effect).toEqual({ kind: "none" });
    const changed = structuredClone(ext);
    changed.agent.pending!.revision = 2;
    expect(onKey(view({ cards: [changed, other] }), armedState, k("2", { at: 1100 })).effect).toEqual({ kind: "none" });
  });

  test("normalize drops an arm whose request changed or is gone", () => {
    expect(normalize(v, armedState).armed).toEqual(armedState.armed);
    const changed = structuredClone(ext);
    changed.agent.pending!.revision = 2;
    expect(normalize(view({ cards: [changed, other] }), armedState).armed ?? null).toBeNull();
    expect(normalize(view({ cards: [other] }), armedState).armed ?? null).toBeNull();
  });
});
