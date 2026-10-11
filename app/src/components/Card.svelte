<script lang="ts">
  import type { CardView } from "../lib/bindings/CardView";
  import { since } from "../lib/format";

  let {
    card,
    selected,
    now,
    onpick,
    oncancel,
  }: {
    card: CardView;
    selected: boolean;
    now: number;
    onpick: (optionId: string, requestId: string, revision: number) => void;
    oncancel: (actionId: string) => void;
  } = $props();

  const pending = $derived(card.agent.pending);
  const seconds = $derived(card.action ? Math.max(0, Math.ceil((card.action.fires_at_ms - now) / 1000)) : 0);
  const hints = $derived(
    [card.can_reply ? "R reply" : null, card.can_mark_seen ? "E mark seen" : null].filter((h) => h !== null),
  );
  // For a question, acd copies the first question's text into `detail`, which
  // is already shown as monospace lines. Show the question text separately
  // only when it carries something the detail does not.
  const question = $derived.by(() => {
    const text = pending?.questions?.[0]?.text ?? null;
    if (text === null || text.trim() === "") return null;
    return text.trim() === pending?.detail.trim() ? null : text;
  });
  // Where the user answers a read-only request depends on where the agent lives.
  const readonlyLabel = $derived(card.agent.source === "t3" ? "Answer in T3" : "Answer in terminal");
</script>

<div
  class="card"
  class:selected
  class:offline={!card.live}
  data-key={card.key}
  aria-busy={card.busy}
>
  <div class="title">
    {card.agent.title}
    {#if card.busy}<span class="spinner" aria-hidden="true"></span>{/if}
  </div>
  <div class="meta">
    {card.machine_name}{card.agent.project ? ` / ${card.agent.project}` : ""}{card.agent.cwd
      ? ` (${card.agent.cwd})`
      : ""}
  </div>
  <div class="state">{card.agent.state.replace(/_/g, " ")}</div>
  {#if card.agent.last_message}
    <div class="last">{card.agent.last_message}</div>
  {/if}

  {#if !card.live}
    <div class="offline-note">
      {card.offline_since_ms !== null ? `offline since ${since(card.offline_since_ms, now)}` : "connecting…"}
    </div>
  {/if}
  {#if pending}
    <div class="request">{pending.title}</div>
    {#if pending.detail}
      <div class="detail">
        {#each pending.detail.split("\n") as line, i (i)}<div class="line">{line}</div>{/each}
      </div>
    {/if}
    {#if question}<div class="question">{question}</div>{/if}
    {#if pending.kind === "blocked_unparsed" && pending.raw_screen !== null}
      <pre>{pending.raw_screen}</pre>
    {/if}
    {#if card.live && card.answerable}
      <ol class="options">
        {#each pending.options as o, i (o.id)}
          {@const ok = card.selectable_options.includes(o.id)}
          <li>
            <button
              class="option"
              disabled={!ok}
              title={ok ? undefined : "Not supported here"}
              onclick={() => onpick(o.id, pending.id, pending.revision)}
            >
              {i + 1}. {o.label}{o.description ? ` (${o.description})` : ""}
            </button>
          </li>
        {/each}
      </ol>
    {:else if card.live && !card.busy && pending.status !== "answering"}
      <div class="readonly">{readonlyLabel}</div>
    {/if}
  {/if}

  {#if card.live && card.action}
    {@const action = card.action}
    <button class="undo" onclick={() => oncancel(action.id)}>
      Sending {action.label} in {seconds}…
      {#if action.detail}<span class="rule">{action.detail}</span>{/if}
    </button>
  {/if}

  {#if selected && hints.length > 0}
    <div class="keys">{hints.join(" · ")}</div>
  {/if}

  {#if card.notice}
    <div class="notice {card.notice.tone}" role="status" data-tone={card.notice.tone}>
      {card.notice.text}
    </div>
  {/if}
</div>

<style>
  .card {
    border: 1px solid transparent;
    border-radius: 6px;
    padding: 6px;
  }
  .card.selected {
    border-color: #48f;
  }
  .card.offline {
    opacity: 0.5;
    filter: grayscale(1);
  }
  .title {
    font-weight: 600;
  }
  .meta,
  .state {
    font-size: 11px;
    opacity: 0.7;
  }
  .keys {
    font-size: 11px;
    opacity: 0.6;
  }
  pre {
    max-height: 160px;
    overflow: auto;
    white-space: pre-wrap;
    font-size: 11px;
  }
  .options {
    list-style: none;
    margin: 4px 0;
    padding: 0;
  }
  .option {
    width: 100%;
    text-align: left;
  }
  .undo {
    display: block;
    width: 100%;
    text-align: left;
  }
  .detail {
    font-family: ui-monospace, monospace;
    font-size: 12px;
  }
  /* One block per logical line with a hanging indent: a wrapped row is
     indented, so only a real line starts at the left edge. */
  .detail .line {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    padding-left: 2ch;
    text-indent: -2ch;
    min-height: 1.2em;
  }
  .rule {
    display: block;
    font-family: ui-monospace, monospace;
    font-size: 11px;
  }
  .notice.warning {
    color: #b60;
  }
  .spinner {
    display: inline-block;
    width: 10px;
    height: 10px;
    border: 2px solid currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
