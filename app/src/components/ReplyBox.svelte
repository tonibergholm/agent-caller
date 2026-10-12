<script lang="ts">
  import { MAX_REPLY_BYTES as MAX_BYTES, byteLength } from "../lib/limits";
  import { isMobile } from "../lib/platform";

  let {
    value = $bindable(""),
    label = "Reply",
    onsend,
    oncancel,
  }: { value?: string; label?: string; onsend?: () => void; oncancel?: () => void } = $props();

  const mobile = isMobile();
  const bytes = $derived(byteLength(value));
  const over = $derived(bytes > MAX_BYTES);

  function focus(node: HTMLTextAreaElement) {
    node.focus();
  }
</script>

<div class="reply" class:mobile>
  <textarea use:focus data-field="reply" bind:value aria-label={label} rows="3" aria-invalid={over}></textarea>
  <span class="count" class:over>{bytes.toLocaleString()} / {MAX_BYTES.toLocaleString()} bytes</span>
  {#if mobile}
    <!-- Touch has no Enter-to-send or Esc-to-cancel: both are buttons here. -->
    <div class="actions">
      <button class="cancel" type="button" onclick={() => oncancel?.()}>Cancel</button>
      <button class="send" type="button" disabled={over || value.trim() === ""} onclick={() => onsend?.()}>Send</button>
    </div>
  {/if}
</div>

<style>
  .reply {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  textarea {
    width: 100%;
    padding: 8px 10px;
    font: inherit;
    color: var(--text);
    background: var(--code-bg);
    border: 1px solid var(--stroke);
    border-radius: var(--r);
  }
  textarea:focus {
    outline: none;
    border-color: rgba(6, 182, 212, 0.55);
    box-shadow: var(--glow);
  }
  .count {
    font-size: 11px;
    color: var(--text-dim);
    align-self: flex-end;
  }
  .over {
    color: var(--crimson-text);
  }
  textarea[aria-invalid="true"] {
    border-color: var(--crimson);
  }
  .actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
  }
  .actions button {
    min-height: 44px;
    padding: 0 14px;
    font: 500 13px var(--font-sans);
    color: var(--text);
    background: var(--fill-subtle);
    border: 1px solid var(--stroke);
    border-radius: var(--r);
    cursor: pointer;
  }
  .actions button:disabled {
    opacity: 0.4;
  }
  .actions .send {
    color: var(--canvas-deep);
    background: var(--cyan);
    border-color: var(--cyan);
  }
  /* 16 px: iOS Safari zooms the page on focus for smaller fields. */
  .reply.mobile textarea {
    font-size: 16px;
  }
</style>
