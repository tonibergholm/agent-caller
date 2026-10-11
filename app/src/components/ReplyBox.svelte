<script lang="ts">
  import { MAX_REPLY_BYTES as MAX_BYTES, byteLength } from "../lib/limits";

  let { value = $bindable(""), label = "Reply" }: { value?: string; label?: string } = $props();

  const bytes = $derived(byteLength(value));
  const over = $derived(bytes > MAX_BYTES);

  function focus(node: HTMLTextAreaElement) {
    node.focus();
  }
</script>

<div class="reply">
  <textarea use:focus data-field="reply" bind:value aria-label={label} rows="3" aria-invalid={over}></textarea>
  <span class="count" class:over>{bytes.toLocaleString()} / {MAX_BYTES.toLocaleString()} bytes</span>
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
</style>
