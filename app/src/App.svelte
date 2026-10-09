<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount } from "svelte";
  import Panel from "./components/Panel.svelte";
  import Settings from "./components/Settings.svelte";
  import type { InboxView } from "./lib/bindings/InboxView";
  import { tauriApi, type Api } from "./lib/ipc";

  const windowLabel = () => getCurrentWindow().label;

  let { api = tauriApi, label }: { api?: Api; label?: string } = $props();

  // svelte-ignore state_referenced_locally
  const resolvedLabel = label ?? windowLabel();

  let v = $state<InboxView | null>(null);
  let hotkey = $state<string | undefined>(undefined);
  let loadError = $state<string | null>(null);

  onMount(() => {
    let off: (() => void) | undefined;
    let closed = false;
    void (async () => {
      try {
        // Subscribe first, then load, so no update falls between the two.
        const un = await api.onView((next) => (v = next));
        if (closed) un();
        else off = un;
        v ??= await api.getView();
      } catch (e) {
        loadError = `Could not load the inbox: ${e instanceof Error ? e.message : String(e)}`;
      }
    })();
    api.getHotkey().then((h) => (hotkey = h), () => {});
    return () => {
      closed = true;
      off?.();
    };
  });
</script>

{#if resolvedLabel === "settings"}
  <Settings {api} />
{:else if v}
  <Panel view={v} {api} {hotkey} />
{:else if loadError}
  <main role="alert">{loadError}</main>
{/if}
