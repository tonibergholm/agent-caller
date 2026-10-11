<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount } from "svelte";
  import Hud from "./components/Hud.svelte";
  import Panel from "./components/Panel.svelte";
  import Settings from "./components/Settings.svelte";
  import type { InboxView } from "./lib/bindings/InboxView";
  import type { VoiceSettings } from "./lib/voice";
  import { tauriApi, type Api } from "./lib/ipc";

  const windowLabel = () => getCurrentWindow().label;

  let { api = tauriApi, label }: { api?: Api; label?: string } = $props();

  // svelte-ignore state_referenced_locally
  const resolvedLabel = label ?? windowLabel();

  let v = $state<InboxView | null>(null);
  let hotkey = $state<string | undefined>(undefined);
  let voiceInput = $state(false);
  const applyVoice = (s: VoiceSettings) => (voiceInput = s.enabled && s.debug_input);
  let loadError = $state<string | null>(null);

  onMount(() => {
    let off: (() => void) | undefined;
    let offVoice: (() => void) | undefined;
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
    api.onVoiceSettings(applyVoice).then((un) => (closed ? un() : (offVoice = un)), () => {});
    api.getVoiceSettings().then(applyVoice, () => {});
    return () => {
      closed = true;
      off?.();
      offVoice?.();
    };
  });
</script>

{#if resolvedLabel === "settings"}
  <Settings {api} />
{:else if resolvedLabel === "hud"}
  <!-- Never the panel: a second panel would publish its own presentation. -->
  <Hud {api} view={v} />
{:else if v}
  <Panel view={v} {api} {hotkey} {voiceInput} />
{:else if loadError}
  <main role="alert">{loadError}</main>
{/if}
