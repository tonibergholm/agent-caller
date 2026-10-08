<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';

  let lastKey = 'No key pressed';
  let keychainResult = '';

  function recordKey(event: KeyboardEvent) {
    lastKey = `key: ${event.key}, isComposing: ${event.isComposing}, keyCode: ${event.keyCode}`;
  }

  async function notify() {
    await invoke('gate_notify');
  }

  async function keychainRoundtrip() {
    keychainResult = '';
    try {
      await invoke('gate_keychain_roundtrip');
      keychainResult = 'Keychain roundtrip passed; the throwaway item was deleted.';
    } catch (error) {
      keychainResult = `Keychain roundtrip failed: ${String(error)}`;
    }
  }
</script>

<main>
  <h1>Agent inbox</h1>
  <textarea aria-label="IME test" onkeydown={recordKey}></textarea>
  <p id="keys">{lastKey}</p>
  <button onclick={notify}>Post gate notification</button>
  <button onclick={keychainRoundtrip}>Check Keychain roundtrip</button>
  <p aria-live="polite">{keychainResult}</p>
</main>
