<script lang="ts">
  import { MAX_ANSWER_BYTES, byteLength } from "../lib/limits";

  let {
    value = $bindable(""),
    onsubmit,
    disabled = false,
  }: { value?: string; onsubmit: () => void; disabled?: boolean } = $props();

  const over = $derived(byteLength(value) > MAX_ANSWER_BYTES);
</script>

<div class="answer" class:over>
  <!-- Enter and Shift+Enter are handled by the panel (keys.ts), IME-safe. -->
  <textarea
    data-field="answer"
    bind:value
    rows="1"
    placeholder="Type an answer…"
    aria-label="Type an answer"
    aria-invalid={over}
    {disabled}
  ></textarea>
  <button class="send" type="button" {disabled} onclick={onsubmit} aria-label="Send answer"><kbd>↵</kbd> Send</button>
  {#if over}<span class="hint" role="note">Too long to send ({MAX_ANSWER_BYTES.toLocaleString()} bytes max).</span>{/if}
</div>

<style>
  .answer {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: flex-start;
  }
  textarea {
    flex: 1 1 12em;
    min-height: 30px;
    padding: 6px 10px;
    resize: vertical;
    font: inherit;
    color: var(--text);
    background: var(--code-bg);
    border: 1px solid var(--stroke);
    border-radius: var(--r);
  }
  textarea::placeholder {
    color: var(--text-dim);
  }
  textarea:focus {
    outline: none;
    border-color: rgba(6, 182, 212, 0.55);
    box-shadow: var(--glow);
  }
  .over textarea {
    border-color: var(--crimson);
  }
  .send {
    display: inline-flex;
    gap: 6px;
    align-items: center;
    height: 30px;
    padding: 0 10px;
    font: 500 12px var(--font-sans);
    color: var(--text);
    background: var(--fill-subtle);
    border: 1px solid var(--stroke);
    border-radius: var(--r);
    cursor: pointer;
  }
  .send:hover:not(:disabled) {
    background: var(--fill-hover);
  }
  kbd {
    font: 500 10px/1 var(--font-mono);
    color: rgba(255, 255, 255, 0.5);
  }
  .hint {
    flex-basis: 100%;
    font-size: 11px;
    color: var(--crimson-text);
  }
</style>
