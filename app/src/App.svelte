<script lang="ts">
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onMount } from "svelte";
  import Hud from "./components/Hud.svelte";
  import Panel from "./components/Panel.svelte";
  import Settings from "./components/Settings.svelte";
  import type { InboxView } from "./lib/bindings/InboxView";
  import { hideLocal } from "./lib/panel";
  import { isMobile } from "./lib/platform";
  import type { VoiceSettings } from "./lib/voice";
  import { tauriApi, type Api } from "./lib/ipc";

  const windowLabel = () => getCurrentWindow().label;

  let { api = tauriApi, label }: { api?: Api; label?: string } = $props();

  // svelte-ignore state_referenced_locally
  const resolvedLabel = label ?? windowLabel();

  // Touch layout (iOS build): no local daemon, no hotkey, no voice.
  const mobile = isMobile();
  // Touch: Settings is shown in place of the inbox (the phone has one window).
  let settingsOpen = $state(false);

  let v = $state<InboxView | null>(null);
  // The phone has no "This Mac": its machine, cards and banner are left out.
  const shown = $derived(v && mobile ? hideLocal(v) : v);
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
    if (!mobile) api.getHotkey().then((h) => (hotkey = h), () => {});
    // Subscribe first, then load; an event after the load started wins.
    let voiceEvents = 0;
    // The voice field is a macOS feature: the phone never reads the voice settings.
    if (!mobile)
      void (async () => {
        try {
          const un = await api.onVoiceSettings((s) => {
            voiceEvents += 1;
            applyVoice(s);
          });
          if (closed) un();
          else offVoice = un;
        } catch {
          // No events: the load below still sets the field.
        }
        const seen = voiceEvents;
        try {
          const s = await api.getVoiceSettings();
          if (!closed && voiceEvents === seen) applyVoice(s);
        } catch {
          // Keep the field off.
        }
      })();
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
{:else if shown && mobile && settingsOpen}
  <Settings {api} onback={() => (settingsOpen = false)} />
{:else if shown}
  <Panel
    view={shown}
    {api}
    {hotkey}
    {voiceInput}
    onsettings={mobile ? () => (settingsOpen = true) : undefined}
  />
{:else if loadError}
  <main role="alert">{loadError}</main>
{/if}
