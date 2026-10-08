<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount } from "svelte";
  import Panel from "./components/Panel.svelte";
  import type { InboxView } from "./lib/bindings/InboxView";
  import { tauriApi, type Api } from "./lib/ipc";

  const windowLabel = () => getCurrentWindow().label;

  let { api = tauriApi, label }: { api?: Api; label?: string } = $props();

  // svelte-ignore state_referenced_locally
  const resolvedLabel = label ?? windowLabel();

  let v = $state<InboxView | null>(null);

  onMount(() => {
    let off: (() => void) | undefined;
    let closed = false;
    // Subscribe first, then load, so no update falls between the two.
    void api.onView((next) => (v = next)).then((un) => {
      if (closed) un();
      else off = un;
    });
    void api.getView().then((initial) => {
      v ??= initial;
    });
    return () => {
      closed = true;
      off?.();
    };
  });
</script>

{#if resolvedLabel === "settings"}
  <!-- Placeholder until Task 17 adds Settings. -->
  <main><h1>Settings</h1></main>
{:else if v}
  <Panel view={v} {api} />
{/if}
