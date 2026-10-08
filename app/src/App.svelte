<script lang="ts">
  import { invoke } from '@tauri-apps/api/core';

  let lastKey = 'No key pressed';
  let keychainResult = '';
  let notificationResult = '';

  function recordKey(event: KeyboardEvent) {
    lastKey = `key: ${event.key}, isComposing: ${event.isComposing}, keyCode: ${event.keyCode}`;
  }

  async function notify() {
    notificationResult = '';
    try {
      await invoke('gate_notify');
      notificationResult = 'Notification request passed.';
    } catch (error) {
      notificationResult = `Notification request failed: ${String(error)}`;
    }
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
  <!-- GATE (Task 13): remove in Task 16 -->
  <h1>Agent inbox</h1>
  <!-- GATE (Task 13): remove in Task 16 -->
  <textarea aria-label="IME test" onkeydown={recordKey}></textarea>
  <!-- GATE (Task 13): remove in Task 16 -->
  <p id="keys">{lastKey}</p>
  <!-- GATE (Task 13): remove in Task 16 -->
  <button onclick={notify}>Post gate notification</button>
  <!-- GATE (Task 13): remove in Task 16 -->
  <p aria-live="polite">{notificationResult}</p>
  <!-- GATE (Task 13): remove in Task 16 -->
  <button onclick={keychainRoundtrip}>Check Keychain roundtrip</button>
  <!-- GATE (Task 13): remove in Task 16 -->
  <p aria-live="polite">{keychainResult}</p>
</main>
