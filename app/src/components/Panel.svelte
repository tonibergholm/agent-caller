<script lang="ts">
  import { onMount } from "svelte";
  import type { CardView } from "../lib/bindings/CardView";
  import type { InboxView } from "../lib/bindings/InboxView";
  import type { Api } from "../lib/ipc";
  import { statusKind, statusReason, statusText } from "../lib/format";
  import { fromEvent, normalize, onKey, type Effect, type KeyState } from "../lib/keys";
  import { dotKind, listedMachines, sections } from "../lib/panel";
  import type { Presentation } from "../lib/voice";
  import Card from "./Card.svelte";
  import ReplyBox from "./ReplyBox.svelte";
  import Row from "./Row.svelte";

  import { MAX_ANSWER_BYTES, MAX_REPLY_BYTES, byteLength } from "../lib/limits";

  let {
    view,
    api,
    hotkey = "Control+Option+Space",
    voiceInput = false,
  }: { view: InboxView; api: Api; hotkey?: string; voiceInput?: boolean } = $props();

  // svelte-ignore state_referenced_locally
  let st = $state<KeyState>(normalize(view, { selected: null, replyFor: null }));
  let replyText = $state("");
  // Typed answers, one per request: keyed by card and request id, so a draft
  // typed for one request can never be sent to a newer one on the same card.
  let answers = $state<Record<string, string>>({});
  const draftKey = (c: CardView) => `${c.key}\0${c.agent.pending?.id ?? ""}`;
  let topH = $state(0);
  let footH = $state(0);
  let now = $state(Date.now());
  let error = $state<string | null>(null);
  // Session state: the machine filter (null = All) and whether Done is open.
  let machineFilter = $state<string | null>(null);
  let doneOpen = $state(false);
  let voiceText = $state("");

  // The error is cleared only by the next user key or click, never by a
  // view push, so a rejection stays visible until the user acts again.

  const machines = $derived(listedMachines(view.machines));
  // A filter on a machine that no longer exists falls back to All.
  const filter = $derived(machines.some((m) => m.key === machineFilter) ? machineFilter : null);
  const secs = $derived(sections(view.cards, filter));
  // What J/K, the digits and the other shortcuts see: only the cards on screen.
  // A Done card with a countdown stays out of the collapse, so Esc has a target on screen.
  const doneShown = $derived(doneOpen ? secs.done : secs.done.filter((c) => c.action !== null));
  const visible = $derived([...secs.needs, ...secs.working, ...doneShown]);
  const nav = $derived<InboxView>({ ...view, cards: visible });
  // The summary counts what the sections show (all machines, no filter).
  const overall = $derived(sections(view.cards, null));
  const needCount = $derived(overall.needs.length);
  const workingCount = $derived(overall.working.length);
  const countFor = (key: string) => view.cards.filter((c) => c.machine_key === key).length;

  $effect(() => {
    const next = normalize(nav, st);
    if (next !== st) st = next;
  });

  $effect(() => {
    if (st.replyFor === null) replyText = "";
  });

  // A typed command is voice text: drop it when the field goes away.
  $effect(() => {
    if (!voiceInput) voiceText = "";
  });

  // Drop drafts whose request is gone (answered, or replaced by a newer one).
  $effect(() => {
    const live = new Set(view.cards.map(draftKey));
    for (const k of Object.keys(answers)) if (!live.has(k)) delete answers[k];
  });

  // Keep the J/K selection in view in the scrolling panel.
  $effect(() => {
    const key = st.selected;
    if (key === null) return;
    const el = [...document.querySelectorAll<HTMLElement>("[data-key]")].find((e) => e.dataset.key === key);
    el?.scrollIntoView?.({ block: "nearest" });
  });

  // Voice (spec §4.3): what voice may target, published on every change.
  // The epoch is new per mount, so a reloaded panel starts afresh.
  const published = { epoch: `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`, version: 0 };
  $effect(() => {
    published.version += 1;
    const presentation: Presentation = {
      epoch: published.epoch,
      version: published.version,
      selected: st.selected,
      visible: visible.map((c) => c.key),
      machine_filter: filter,
      done_collapsed: !doneOpen,
    };
    api.publishPresentation(presentation).catch(() => {});
  });

  onMount(() => {
    let off: (() => void) | undefined;
    let closed = false;
    api
      .onVoiceSelect((key) => {
        // A card hidden only by the machine filter can be named (spec §5.2): show it.
        if (!visible.some((c) => c.key === key)) machineFilter = null;
        st = { ...st, selected: key };
      })
      .then((un) => (closed ? un() : (off = un)), () => {});
    return () => {
      closed = true;
      off?.();
    };
  });

  onMount(() => {
    const timer = setInterval(() => (now = Date.now()), 100);
    return () => clearInterval(timer);
  });

  const answerField = (cardKey: string) =>
    [...document.querySelectorAll<HTMLElement>("[data-key]")]
      .find((e) => e.dataset.key === cardKey)
      ?.querySelector<HTMLTextAreaElement>("textarea[data-field=answer]") ?? null;

  async function run(effect: Effect) {
    switch (effect.kind) {
      case "select":
      case "none":
        return;
      case "pick":
        await api.pick(effect.cardKey, effect.requestId, effect.optionId, effect.revision);
        return;
      case "openReply":
        st = { ...st, selected: effect.cardKey, replyFor: effect.cardKey };
        return;
      case "focusAnswer":
        answerField(effect.cardKey)?.focus();
        return;
      case "markSeen":
        await api.markSeen(effect.cardKey);
        return;
      case "submitReply": {
        const text = replyText;
        if (text.trim() === "" || byteLength(text) > MAX_REPLY_BYTES) return;
        await api.reply(effect.cardKey, text);
        st = { ...st, replyFor: null };
        return;
      }
      case "submitAnswer": {
        // Id and revision come from the request on screen now, and so does the draft.
        const c = view.cards.find((x) => x.key === effect.cardKey);
        const pending = c?.agent.pending;
        if (!c?.free_text_answerable || !pending) return;
        // A countdown is already running for this card: nothing more to send.
        if (c.action !== null || c.busy) return;
        const text = answers[draftKey(c)] ?? "";
        if (text.trim() === "" || byteLength(text) > MAX_ANSWER_BYTES) return;
        await api.answerText(effect.cardKey, pending.id, pending.revision, text);
        // The draft stays until the request resolves, so an undo gets it back.
        answerField(effect.cardKey)?.blur();
        return;
      }
      case "submitVoice": {
        const text = voiceText;
        if (text.trim() === "") return;
        voiceText = "";
        await api.voiceTextInput(text);
        return;
      }
      case "escape":
        // The decision never reads view.actions: it may be stale.
        if (await api.escape()) return;
        if (st.replyFor) {
          st = { ...st, replyFor: null };
          return;
        }
        // Esc in a text field leaves the field first; the next one closes.
        {
          const field = document.activeElement instanceof HTMLElement ? document.activeElement.dataset.field : undefined;
          if (field === "answer" || field === "voice") {
            (document.activeElement as HTMLElement).blur();
            return;
          }
        }
        await api.hide();
        return;
    }
  }

  function handle(event: KeyboardEvent) {
    error = null;
    const { state, effect } = onKey(nav, st, fromEvent(event));
    st = state;
    if (effect.kind === "none") return;
    event.preventDefault();
    act(run(effect), false);
  }

  /** Click-driven calls: clear the old message, then show any rejection. */
  function act(p: Promise<unknown>, clear = true) {
    if (clear) error = null;
    p.catch((e) => {
      error = `Failed: ${e instanceof Error ? e.message : String(e)}`;
    });
  }

  // Transient states read fine as just the kind; the rest show their reason.
  const quiet = new Set(["online", "connecting", "syncing"]);
  const alerts = $derived(machines.filter((m) => !quiet.has(m.status.kind)));
</script>

<svelte:window onkeydown={handle} />

{#snippet cardFor(c: CardView)}
  <div class="slot">
    {#if c.group === "request"}
      <Card
        card={c}
        selected={st.selected === c.key}
        {now}
        bind:answer={() => answers[draftKey(c)] ?? "", (v) => (answers[draftKey(c)] = v)}
        onpick={(optionId, requestId, revision) => {
          // Id and revision come from the request the card rendered at click time.
          act(run({ kind: "pick", cardKey: c.key, requestId, optionId, revision }));
        }}
        oncancel={(actionId) => act(api.cancel(actionId))}
        onreply={() => act(run({ kind: "openReply", cardKey: c.key }))}
        onmarkseen={() => act(run({ kind: "markSeen", cardKey: c.key }))}
        onanswer={() => act(run({ kind: "submitAnswer", cardKey: c.key }))}
      />
    {:else}
      <Row
        card={c}
        selected={st.selected === c.key}
        {now}
        oncancel={(actionId) => act(api.cancel(actionId))}
        onreply={() => act(run({ kind: "openReply", cardKey: c.key }))}
        onmarkseen={() => act(run({ kind: "markSeen", cardKey: c.key }))}
      />
    {/if}
    {#if st.replyFor === c.key}
      <ReplyBox bind:value={replyText} />
    {/if}
  </div>
{/snippet}

<main
  class="panel"
  style:scroll-padding-top="{topH}px"
  style:scroll-padding-bottom="{footH}px"
>
  <!-- The panel is borderless: this strip (and the header's empty space) drags it.
       It sticks to the top so the handle never scrolls away. Tauri's drag script
       toggles maximize from a document-level mouseup with detail 2 (a double click),
       which a panel refuses; stopping that mouseup here keeps it from firing. -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="top"
    bind:clientHeight={topH}
    onmouseup={(e) => {
      if (e.detail >= 2) e.stopPropagation();
    }}
  >
    <div class="grip" data-tauri-drag-region aria-hidden="true"></div>
    <header data-tauri-drag-region>
      <h1>Inbox</h1>
      <span class="summary">{needCount} need you · {workingCount} working</span>
      <span class="dots">
        {#each machines as m (m.key)}
          <span
            class="dot"
            role="img"
            data-status={m.status.kind}
            data-dot={dotKind(m.status)}
            title="{m.name}: {statusText(m.status)}"
            aria-label="{m.name}: {statusText(m.status)}"
          ></span>
        {/each}
      </span>
      <button class="gear" aria-label="Settings" title="Settings" onclick={() => act(api.openSettings())}>
        <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3h0a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5h0a1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8v0a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
        </svg>
      </button>
    </header>
    {#if machines.length > 1}
      <nav class="filters" aria-label="Machines">
        <button class="fchip" aria-pressed={filter === null} onclick={() => (machineFilter = null)}>
          All <span class="n">{view.cards.length}</span>
        </button>
        {#each machines as m (m.key)}
          <button class="fchip" aria-pressed={filter === m.key} onclick={() => (machineFilter = m.key)}>
            {m.name} <span class="n">{countFor(m.key)}</span>
          </button>
        {/each}
      </nav>
    {/if}
  </div>

  {#if error}
    <div class="error" role="alert">{error}</div>
  {/if}

  {#each alerts as m (m.key)}
    {@const reason = statusReason(m.status)}
    <div class="machine-alert" role="status" data-status={m.status.kind}>
      {m.name}: {statusKind(m.status)}{#if reason}<span class="reason">: {reason}</span>{/if}
    </div>
  {/each}

  {#if secs.needs.length + secs.working.length + secs.done.length === 0}
    <p class="empty">No agents need you.</p>
    <p class="hint">Press {hotkey} to open this panel.</p>
  {/if}

  {#if secs.needs.length > 0}
    <section aria-label="Needs you">
      <h2 class="sec needs">Needs you <span class="count">{secs.needs.length}</span></h2>
      {#each secs.needs as c (c.key)}{@render cardFor(c)}{/each}
    </section>
  {/if}

  {#if secs.working.length > 0}
    <section aria-label="Working">
      <h2 class="sec working">Working <span class="count">{secs.working.length}</span></h2>
      {#each secs.working as c (c.key)}{@render cardFor(c)}{/each}
    </section>
  {/if}

  {#if secs.done.length > 0}
    <section aria-label="Done">
      <h2 class="sec done">
        <button class="toggle" aria-expanded={doneOpen} onclick={() => (doneOpen = !doneOpen)}>
          <span class="chev" class:open={doneOpen} aria-hidden="true">›</span>
          Done <span class="count">{secs.done.length}</span>
        </button>
      </h2>
      {#each doneShown as c (c.key)}{@render cardFor(c)}{/each}
    </section>
  {/if}

  {#if voiceInput}
    <!-- The step-1 stand-in for speech (voice plan D13). A textarea, so
         keys.ts treats it as a field and J/K/digits stay text. -->
    <textarea
      class="voice"
      rows="1"
      data-field="voice"
      aria-label="Voice command"
      placeholder="Type a voice command, then Enter"
      bind:value={voiceText}
    ></textarea>
  {/if}

  <footer bind:clientHeight={footH}>
    <span class="keys">
      <span><kbd>J</kbd><kbd>K</kbd> move</span>
      <span><kbd>1</kbd>–<kbd>9</kbd> answer</span>
      <span><kbd>T</kbd> type</span>
      <span><kbd>R</kbd> reply</span>
      <span><kbd>Esc</kbd> close / undo</span>
    </span>
    {#if view.show_idle}
      <button class="idle" onclick={() => act(api.setShowIdle(false))}>Hide idle</button>
    {:else if view.hidden_idle > 0}
      <button class="idle" onclick={() => act(api.setShowIdle(true))}>Show {view.hidden_idle} idle</button>
    {/if}
  </footer>
</main>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 0 12px 0;
    /* The window is user-resizable: fill it and scroll the cards. */
    height: 100vh;
    overflow-y: auto;
    background: var(--canvas);
  }
  .top {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 0 -12px;
    padding: 0 12px 8px;
    background: var(--glass-strong);
    backdrop-filter: blur(24px);
    border-bottom: 1px solid var(--hairline);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.12);
  }
  .grip {
    flex: none;
    height: 12px;
    margin: 0 -12px;
    cursor: grab;
    background: linear-gradient(transparent 4px, rgba(255, 255, 255, 0.14) 4px 6px, transparent 6px) center / 36px 12px no-repeat;
  }
  header {
    display: flex;
    gap: 10px;
    align-items: center;
    min-height: 28px;
  }
  /* Only the bare header takes the drag: Tauri ignores children. */
  header > h1,
  header > .summary {
    pointer-events: none;
  }
  h1 {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
    letter-spacing: -0.02em;
  }
  .summary {
    flex: 1;
    font-size: 12px;
    color: var(--text-dim);
  }
  .dots {
    display: flex;
    gap: 7px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--slate);
  }
  .dot[data-dot="online"] {
    background: var(--emerald);
    box-shadow: 0 0 6px rgba(16, 185, 129, 0.7);
  }
  .dot[data-dot="syncing"] {
    background: var(--cyan);
    animation: pulse 1.6s ease-in-out infinite;
  }
  .dot[data-dot="error"] {
    background: var(--crimson);
    box-shadow: 0 0 6px rgba(239, 68, 68, 0.6);
  }
  @keyframes pulse {
    50% {
      opacity: 0.3;
    }
  }
  .gear {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    padding: 0;
    color: var(--text-muted);
    background: none;
    border: 0;
    border-radius: var(--r);
    cursor: pointer;
  }
  .gear:hover {
    background: var(--fill-hover);
  }
  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .fchip {
    display: inline-flex;
    gap: 6px;
    align-items: center;
    height: 22px;
    padding: 0 9px;
    font: 500 12px var(--font-sans);
    color: var(--text-muted);
    background: var(--fill-subtle);
    border: 1px solid var(--stroke);
    border-radius: var(--r-full);
    cursor: pointer;
  }
  .fchip[aria-pressed="true"] {
    color: var(--cyan-bright);
    background: var(--cyan-fill);
    border-color: rgba(6, 182, 212, 0.4);
  }
  .n {
    font: 500 10.5px var(--font-mono);
    opacity: 0.7;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .sec {
    display: flex;
    gap: 6px;
    align-items: center;
    margin: 0;
    font-size: 10.5px;
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-dim);
  }
  .sec.needs {
    color: var(--amber-text);
  }
  .sec.working {
    color: var(--cyan-bright);
  }
  .count {
    padding: 0 6px;
    font: 500 10.5px/16px var(--font-mono);
    background: var(--fill-subtle);
    border-radius: var(--r-full);
  }
  .toggle {
    display: flex;
    gap: 6px;
    align-items: center;
    padding: 0;
    font: inherit;
    letter-spacing: inherit;
    text-transform: inherit;
    color: inherit;
    background: none;
    border: 0;
    cursor: pointer;
  }
  .chev {
    display: inline-block;
    transition: transform 120ms;
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .slot {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .error {
    color: var(--crimson-text);
    background: var(--crimson-fill);
    padding: 6px 10px;
    border-radius: var(--r);
  }
  .machine-alert {
    font-size: 12px;
    color: var(--crimson-text);
    background: var(--crimson-fill);
    padding: 6px 10px;
    border-radius: var(--r);
  }
  .empty {
    margin: 24px 0 0;
    text-align: center;
    font-size: 15px;
    font-weight: 500;
  }
  .hint {
    margin: -8px 0 0;
    text-align: center;
    color: var(--text-dim);
  }
  footer {
    position: sticky;
    bottom: 0;
    display: flex;
    gap: 10px;
    align-items: center;
    justify-content: space-between;
    margin: auto -12px 0;
    padding: 8px 12px;
    font-size: 11px;
    color: var(--text-dim);
    background: var(--canvas);
    border-top: 1px solid var(--hairline);
  }
  .keys {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
  }
  .keys span {
    display: inline-flex;
    gap: 3px;
    align-items: center;
  }
  kbd {
    display: inline-grid;
    place-items: center;
    min-width: 16px;
    height: 16px;
    padding: 0 3px;
    font: 500 10px var(--font-mono);
    color: rgba(255, 255, 255, 0.5);
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: var(--r-sm);
  }
  .idle {
    flex: none;
    padding: 2px 8px;
    font: 500 11px var(--font-sans);
    color: var(--text-muted);
    background: var(--fill-subtle);
    border: 1px solid var(--stroke);
    border-radius: var(--r);
    cursor: pointer;
  }
  .voice {
    flex: none;
    min-height: 32px;
    padding: 6px 10px;
    font: 13px var(--font-mono);
    color: var(--text);
    background: var(--fill-subtle);
    border: 1px solid rgba(6, 182, 212, 0.4);
    border-radius: var(--r);
    resize: none;
  }
  .voice:focus {
    outline: none;
    box-shadow: 0 0 0 1px rgba(6, 182, 212, 0.4), var(--glow);
  }
</style>
