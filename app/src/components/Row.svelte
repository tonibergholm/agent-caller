<script lang="ts">
  import type { CardView } from "../lib/bindings/CardView";
  import { since } from "../lib/format";
  import { isMobile } from "../lib/platform";
  import { age } from "../lib/panel";
  import UndoBar from "./UndoBar.svelte";

  // Touch layout (iOS build): no hover, no keyboard selection to reveal actions.
  const mobile = isMobile();

  let {
    card,
    selected,
    now,
    oncancel,
    onreply = () => {},
    onmarkseen = () => {},
  }: {
    card: CardView;
    selected: boolean;
    now: number;
    oncancel: (actionId: string) => void;
    onreply?: () => void;
    onmarkseen?: () => void;
  } = $props();

  const sourceLabel = $derived(
    card.agent.source === "external"
      ? card.agent.title
      : { herdr: "herdr", t3: "T3", hooks: "hooks", unknown: "unknown" }[card.agent.source],
  );
  const state = $derived(card.agent.state);
  const changed = $derived(age(card.agent.state_since, now));
</script>

<div class="row" class:selected class:mobile class:offline={!card.live} class:dim={card.group === "done" || card.group === "idle"} data-key={card.key} aria-busy={card.busy}>
  <div class="line">
    <span class="dot" data-state={state} aria-hidden="true"></span>
    <span class="title" title={mobile ? undefined : (card.agent.cwd ?? undefined)}>{card.agent.title}</span>
    {#if card.agent.role === "subagent"}
      <span class="subagent">subagent</span>
      {#if card.agent.parent_title}<span class="parent" title={mobile ? undefined : card.agent.parent_title}>↳ {card.agent.parent_title}</span>{/if}
    {/if}
    {#if card.agent.last_message}<span class="last">{card.agent.last_message}</span>{/if}
    <span class="machine">{card.machine_name}</span>
    <span class="source" data-source={card.agent.source}>{sourceLabel}</span>
    <span class="time">
      {#if !card.live}
        {card.offline_since_ms !== null ? `offline since ${since(card.offline_since_ms, now)}` : "connecting…"}
      {:else if changed}{changed}{/if}
    </span>
  </div>
  {#if mobile && card.agent.cwd}<div class="path">{card.agent.cwd}</div>{/if}
  {#if card.live && (selected || mobile) && (card.can_reply || card.can_mark_seen)}
    <div class="more-actions">
      {#if card.can_reply}<button class="ghost" onclick={onreply}><kbd>R</kbd> Reply</button>{/if}
      {#if card.can_mark_seen}<button class="ghost" onclick={onmarkseen}><kbd>E</kbd> Mark seen</button>{/if}
    </div>
  {/if}
  {#if card.live && card.action}
    <UndoBar action={card.action} {now} {oncancel} />
  {/if}
  {#if card.notice}
    <div class="notice {card.notice.tone}" role="status" data-tone={card.notice.tone}>{card.notice.text}</div>
  {/if}
</div>

<style>
  .row {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 7px 12px;
    background: var(--fill-subtle);
    border: 1px solid transparent;
    border-radius: var(--r);
  }
  .row.selected {
    border-color: rgba(6, 182, 212, 0.4);
    box-shadow: var(--glow);
  }
  .row.dim .line {
    opacity: 0.6;
  }
  /* Dim the content only: the offline note stays at full contrast. */
  .row.offline .line > :not(.time) {
    opacity: 0.5;
    filter: grayscale(1);
  }
  .row.offline .time {
    color: var(--text-muted);
  }
  .subagent {
    flex: none;
    padding: 0 6px;
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.04em;
    line-height: 16px;
    color: #a5b4fc;
    background: rgba(99, 102, 241, 0.16);
    border-radius: var(--r-sm);
  }
  .parent {
    flex: 0 1 12em;
    min-width: 0;
    overflow: hidden;
    font-size: 11px;
    color: var(--text-dim);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .line {
    display: flex;
    gap: 8px;
    align-items: center;
    min-width: 0;
  }
  .dot {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--slate);
  }
  .dot[data-state="working"] {
    background: var(--cyan);
    box-shadow: 0 0 6px var(--cyan);
    animation: pulse 1.6s ease-in-out infinite;
  }
  .dot[data-state="done"] {
    background: var(--emerald);
    box-shadow: 0 0 6px rgba(16, 185, 129, 0.6);
  }
  @keyframes pulse {
    50% {
      opacity: 0.35;
    }
  }
  .title {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    font-weight: 500;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .last {
    flex: 1 1 0;
    min-width: 0;
    overflow: hidden;
    font-size: 12px;
    color: var(--text-dim);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .title + .machine {
    margin-left: auto;
  }
  .machine,
  .source {
    flex: none;
    font-size: 10.5px;
    color: var(--text-muted);
  }
  .machine {
    padding: 0 6px;
    font-family: var(--font-mono);
    line-height: 16px;
    background: var(--fill-subtle);
    border: 1px solid var(--stroke);
    border-radius: var(--r-sm);
  }
  .source {
    padding: 0 6px;
    font-weight: 600;
    letter-spacing: 0.04em;
    line-height: 16px;
    border: 1px solid currentColor;
    border-radius: var(--r-full);
    opacity: 0.7;
  }
  .time {
    flex: none;
    min-width: 5em;
    font-size: 11px;
    text-align: right;
    color: var(--text-dim);
  }
  .more-actions {
    display: flex;
    gap: 6px;
  }
  .ghost {
    display: inline-flex;
    gap: 7px;
    align-items: center;
    min-height: 26px;
    padding: 2px 10px 2px 5px;
    font: 500 12px var(--font-sans);
    color: var(--text-muted);
    background: var(--fill-subtle);
    border: 1px solid var(--stroke);
    border-radius: var(--r);
    cursor: pointer;
  }
  .ghost:hover {
    background: var(--fill-hover);
  }
  kbd {
    display: inline-grid;
    place-items: center;
    min-width: 18px;
    height: 18px;
    padding: 0 4px;
    font: 500 10.5px var(--font-mono);
    color: rgba(255, 255, 255, 0.5);
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: var(--r-sm);
  }
  .notice {
    font-size: 12px;
  }
  .notice.warning {
    color: var(--amber-text);
  }
  /* Touch layout (iOS build): text wraps, and every button is a 44 pt target. */
  .path {
    font: 400 11px/1.3 var(--font-mono);
    color: var(--text-dim);
    overflow-wrap: anywhere;
  }
  .row.mobile .line {
    flex-wrap: wrap;
  }
  .row.mobile .title,
  .row.mobile .last {
    overflow: visible;
    white-space: normal;
    overflow-wrap: anywhere;
  }
  .row.mobile .parent {
    max-width: none;
    overflow: visible;
    white-space: normal;
    overflow-wrap: anywhere;
  }
  .row.mobile :global(button) {
    min-height: 44px;
  }
</style>
