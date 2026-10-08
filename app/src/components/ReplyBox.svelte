<script lang="ts">
  const MAX_BYTES = 64 * 1024;

  let { value = $bindable(""), label = "Reply" }: { value?: string; label?: string } = $props();

  const bytes = $derived(new TextEncoder().encode(value).length);
  const over = $derived(bytes > MAX_BYTES);

  function focus(node: HTMLTextAreaElement) {
    node.focus();
  }
</script>

<div class="reply">
  <textarea use:focus bind:value aria-label={label} rows="3" aria-invalid={over}></textarea>
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
    box-sizing: border-box;
    font: inherit;
  }
  .count {
    font-size: 11px;
    opacity: 0.6;
    align-self: flex-end;
  }
  .over {
    color: #c00;
    opacity: 1;
  }
</style>
