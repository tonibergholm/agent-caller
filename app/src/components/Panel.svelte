<script lang="ts">
  import { onMount } from "svelte";
  import type { InboxView } from "../lib/bindings/InboxView";
  import type { Api } from "../lib/ipc";
  import { statusKind, statusReason, statusText } from "../lib/format";
  import { fromEvent, normalize, onKey, type Effect, type KeyState } from "../lib/keys";
  import Card from "./Card.svelte";
  import ReplyBox from "./ReplyBox.svelte";

  import { MAX_REPLY_BYTES, byteLength } from "../lib/limits";

  let {
    view,
    api,
    hotkey = "Control+Option+Space",
  }: { view: InboxView; api: Api; hotkey?: string } = $props();

  // svelte-ignore state_referenced_locally
  let st = $state<KeyState>(normalize(view, { selected: null, replyFor: null }));
  let replyText = $state("");
  let now = $state(Date.now());
  let error = $state<string | null>(null);

  // The error is cleared only by the next user key or click, never by a
  // view push, so a rejection stays visible until the user acts again.

  $effect(() => {
    const next = normalize(view, st);
    if (next !== st) st = next;
  });

  $effect(() => {
    if (st.replyFor === null) replyText = "";
  });

  onMount(() => {
    const timer = setInterval(() => (now = Date.now()), 100);
    return () => clearInterval(timer);
  });

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
      case "escape":
        // The decision never reads view.actions: it may be stale.
        if (await api.escape()) return;
        if (st.replyFor) {
          st = { ...st, replyFor: null };
          return;
        }
        await api.hide();
        return;
    }
  }

  function handle(event: KeyboardEvent) {
    error = null;
    const { state, effect } = onKey(view, st, fromEvent(event));
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
</script>

<svelte:window onkeydown={handle} />

<main class="panel">
  <header>
    {#each view.machines as m (m.key)}
      {@const reason = quiet.has(m.status.kind) ? null : statusReason(m.status)}
      <span class="chip" data-status={m.status.kind} title="{m.name}: {statusText(m.status)}"
        >{m.name}: {statusKind(m.status)}{#if reason}<span class="reason">: {reason}</span>{/if}</span
      >
    {/each}
  </header>

  {#if error}
    <div class="error" role="alert">{error}</div>
  {/if}

  {#if view.cards.length === 0}
    <p class="empty">No agents need you.</p>
    <p class="hint">Press {hotkey} to open this panel.</p>
  {/if}

  {#each view.cards as c (c.key)}
    <div class="slot">
      <Card
        card={c}
        selected={st.selected === c.key}
        {now}
        onpick={(optionId, requestId, revision) => {
          // Id and revision come from the request the card rendered at click time.
          act(run({ kind: "pick", cardKey: c.key, requestId, optionId, revision }));
        }}
        oncancel={(actionId) => {
          act(api.cancel(actionId));
        }}
      />
      {#if st.replyFor === c.key}
        <ReplyBox bind:value={replyText} />
      {/if}
    </div>
  {/each}

  <footer>
    {#if view.show_idle}
      <button onclick={() => act(api.setShowIdle(false))}>Hide idle</button>
    {:else if view.hidden_idle > 0}
      <button onclick={() => act(api.setShowIdle(true))}>Show {view.hidden_idle} idle</button>
    {/if}
    <button onclick={() => act(api.openSettings())}>Settings</button>
  </footer>
</main>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px;
    font: 13px system-ui, sans-serif;
  }
  .chip {
    margin-right: 6px;
    font-size: 11px;
    opacity: 0.8;
  }
  .reason {
    display: inline-block;
    max-width: 14em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    vertical-align: bottom;
  }
  .error {
    color: #c00;
  }
  footer {
    display: flex;
    gap: 8px;
  }
</style>
