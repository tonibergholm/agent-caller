<script lang="ts">
  import { onMount } from "svelte";
  import type { InboxView } from "../lib/bindings/InboxView";
  import type { Api } from "../lib/ipc";
  import { fromEvent, normalize, onKey, type Effect, type KeyState } from "../lib/keys";
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

  $effect(() => {
    void view;
    error = null;
  });

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
        await api.pick(effect.cardKey, effect.requestId, effect.optionId);
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
    run(effect).catch((e) => {
      error = `Could not send: ${e instanceof Error ? e.message : String(e)}`;
    });
  }

  function remaining(firesAt: number): string {
    return (Math.max(0, firesAt - now) / 1000).toFixed(1);
  }

  const statusText = (s: InboxView["machines"][number]["status"]) => s.kind.replace(/_/g, " ");
</script>

<svelte:window onkeydown={handle} />

<main class="panel">
  <header>
    {#each view.machines as m (m.key)}
      <span class="chip" data-status={m.status.kind}>{m.name}: {statusText(m.status)}</span>
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
    <div class="card" data-key={c.key} class:selected={st.selected === c.key} aria-busy={c.busy}>
      <div class="title">{c.agent.title}</div>
      <div class="meta">{c.machine_name}{c.agent.project ? ` / ${c.agent.project}` : ""}</div>
      {#if c.agent.pending}
        <div class="request">{c.agent.pending.title}</div>
        <ol class="options">
          {#each c.agent.pending.options as o, i (o.id)}
            <li class:disabled={!c.selectable_options.includes(o.id)}>
              {i + 1}. {o.label}{o.description ? ` (${o.description})` : ""}
            </li>
          {/each}
        </ol>
      {/if}
      {#if c.action}
        <button class="undo" onclick={() => api.cancel(c.action!.id)}>
          {c.action.label} in {remaining(c.action.fires_at_ms)}s. Click or Esc to cancel
        </button>
      {/if}
      {#if c.notice}
        <div class="notice" data-tone={c.notice.tone}>{c.notice.text}</div>
      {/if}
      {#if st.replyFor === c.key}
        <ReplyBox bind:value={replyText} />
      {/if}
    </div>
  {/each}

  <footer>
    {#if view.show_idle}
      <button onclick={() => api.setShowIdle(false)}>Hide idle</button>
    {:else if view.hidden_idle > 0}
      <button onclick={() => api.setShowIdle(true)}>Show {view.hidden_idle} idle</button>
    {/if}
    <button onclick={() => api.openSettings()}>Settings</button>
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
  .card {
    border: 1px solid transparent;
    border-radius: 6px;
    padding: 6px;
  }
  .card.selected {
    border-color: #48f;
  }
  .options {
    list-style: none;
    margin: 4px 0;
    padding: 0;
  }
  .disabled {
    opacity: 0.5;
  }
  .error {
    color: #c00;
  }
  .notice[data-tone="warning"] {
    color: #b60;
  }
  footer {
    display: flex;
    gap: 8px;
  }
</style>
