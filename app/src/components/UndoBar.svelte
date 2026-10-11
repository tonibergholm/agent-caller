<script lang="ts">
  import type { ActionView } from "../lib/bindings/ActionView";

  /** The undo window the daemon applies to every answer (spec §8). */
  const WINDOW_MS = 2000;

  let {
    action,
    now,
    oncancel,
  }: { action: ActionView; now: number; oncancel: (actionId: string) => void } = $props();

  const remaining = $derived(Math.max(0, action.fires_at_ms - now));
  const seconds = $derived((remaining / 1000).toFixed(1));
  const fraction = $derived(Math.min(1, remaining / WINDOW_MS));
</script>

<button class="undo" onclick={() => oncancel(action.id)}>
  <span class="line">
    <span class="what">
      <span class="lbl">Sending “{action.label}”</span>
      <span class="secs">{seconds}s</span>
    </span>
    <span class="esc"><kbd>Esc</kbd> to undo</span>
  </span>
  {#if action.detail}<span class="rule">{action.detail}</span>{/if}
  <span class="track" aria-hidden="true"><span class="bar" style:width="{fraction * 100}%"></span></span>
</button>

<style>
  .undo {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    padding: 6px 10px 9px;
    overflow: hidden;
    text-align: left;
    font: inherit;
    color: var(--cyan-bright);
    background: var(--cyan-fill);
    border: 1px solid rgba(6, 182, 212, 0.4);
    border-radius: var(--r);
    cursor: pointer;
  }
  .line {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    font-weight: 500;
  }
  .what {
    display: flex;
    flex: 1 1 auto;
    gap: 6px;
    min-width: 0;
  }
  .lbl {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .secs {
    flex: none;
    min-width: 3.4ch;
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .esc {
    flex: none;
    white-space: nowrap;
    color: var(--text-dim);
    font-size: 11px;
  }
  kbd {
    font: 500 10px/1 var(--font-mono);
    padding: 1px 4px;
    border: 1px solid var(--stroke);
    border-radius: var(--r-sm);
  }
  .rule {
    font: 500 11px/1rem var(--font-mono);
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .track {
    position: absolute;
    inset: auto 0 0 0;
    height: 3px;
    background: var(--canvas-deep);
  }
  .bar {
    display: block;
    height: 100%;
    background: var(--cyan);
    transition: width 120ms linear;
  }
</style>
