<script lang="ts">
  import { onMount } from "svelte";
  import type { MachineEntryView } from "../lib/bindings/MachineEntryView";
  import type { MachineStatus } from "../lib/bindings/MachineStatus";
  import type { Api } from "../lib/ipc";
  import { statusText } from "../lib/format";
  import {
    HOLD_KEYS,
    VOICE_OFF,
    unavailableText,
    type HoldKey,
    type PermissionKind,
    type PermissionState,
    type VoiceSettings,
    type VoiceStatus,
  } from "../lib/voice";

  let { api }: { api: Api } = $props();

  let machines = $state<MachineEntryView[]>([]);
  let statuses = $state<Record<string, MachineStatus>>({});
  let showIdle = $state(false);
  let showSubagents = $state(false);
  let showHookOnly = $state(false);
  let hotkey = $state("");
  let voice = $state<VoiceSettings>(VOICE_OFF);
  let status = $state<VoiceStatus>({
    unavailable: "checking",
    streams_partials: false,
    locale: null,
    microphone: "not_determined",
    speech: null,
    input_monitoring: "not_determined",
    hold_key_active: false,
    relaunch_for_hold_key: false,
    toggle_error: null,
  });
  let toggleDraft = $state("");
  /** The stored voice settings arrived; until then the keys below would show defaults. */
  let voiceLoaded = $state(false);
  /** The first status arrived; until then the placeholder below must not show. */
  let statusLoaded = $state(false);
  let statusSeq = 0;

  const stateText: Record<PermissionState, string> = {
    granted: "Allowed",
    denied: "Denied",
    restricted: "Restricted",
    not_determined: "Not asked yet",
  };

  type Row = { kind: PermissionKind; name: string; state: PermissionState; why: string };
  const rows = $derived<Row[]>([
    {
      kind: "microphone",
      name: "Microphone",
      state: status.microphone,
      why: "Needed to hear you. Agent Caller listens only while you hold the key, or between two presses of the shortcut.",
    },
    ...(status.speech === null
      ? []
      : [{ kind: "speech" as const, name: "Speech Recognition", state: status.speech, why: "Needed by macOS for on-device transcription." }]),
    {
      kind: "input_monitoring",
      name: "Input Monitoring",
      state: status.input_monitoring,
      why: "Needed only for the hold key. The voice shortcut works without it.",
    },
  ]);

  // Only the latest request may apply, so overlapping loads cannot go back in time.
  const loadStatus = () => {
    const seq = ++statusSeq;
    return api.getVoiceStatus().then(
      (s) => {
        if (seq !== statusSeq) return;
        status = s;
        statusLoaded = true;
      },
      () => {},
    );
  };

  const withStop = (text: string) => (/[.!?]$/.test(text) ? text : `${text}.`);

  let names = $state<Record<string, string>>({});
  let error = $state<string | null>(null);
  let added = $state<string | null>(null);
  const PROBE_STEPS: [number, string][] = [
    [1, "1. Status"],
    [2, "2. Ask for the microphone"],
    [3, "3. Transcribe 4 s from the microphone"],
    [4, "4. Transcribe a spoken clip"],
    [5, "5. Ask for Speech Recognition"],
    [6, "6. Input Monitoring and Right Option (10 s)"],
    [7, "7. Foundation Models availability"],
  ];
  let probeReports = $state<string[]>([]);

  const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

  const pairingText = {
    paired: "paired",
    not_paired: "not paired",
    unreadable: "stored credentials unreadable",
  } as const;

  let hotkeyEdited = false;

  function applyView(v: { machines: { key: string; status: MachineStatus }[]; show_idle: boolean; show_subagents: boolean; show_hook_only: boolean }) {
    const known = new Set(machines.map((m) => m.key));
    if (v.machines.length !== known.size || v.machines.some((m) => !known.has(m.key))) {
      refresh().catch((e) => (error ??= `Could not list machines: ${message(e)}`));
    }
    statuses = Object.fromEntries(v.machines.map((m) => [m.key, m.status]));
    showIdle = v.show_idle;
    showSubagents = v.show_subagents;
    showHookOnly = v.show_hook_only;
  }

  async function refresh() {
    machines = await api.listMachines();
    for (const m of machines) names[m.key] ??= m.name;
  }

  /** Run a Settings action; any rejection is shown, success clears the old message. */
  async function act(work: () => Promise<unknown>, label: string) {
    error = null;
    added = null;
    try {
      await work();
    } catch (e) {
      error = `${label}: ${message(e)}`;
    }
    try {
      await refresh();
    } catch (e) {
      error ??= `Could not list machines: ${message(e)}`;
    }
  }

  onMount(() => {
    let off: (() => void) | undefined;
    let offProbe: (() => void) | undefined;
    let offStatus: (() => void) | undefined;
    let closed = false;
    void act(async () => {
      const stored = await api.getHotkey();
      if (!hotkeyEdited) hotkey = stored;
    }, "Could not read the hotkey");
    api.getVoiceSettings().then(
      (s) => {
        voice = s;
        voiceLoaded = true;
        toggleDraft = s.toggle_shortcut ?? "";
      },
      (e) => (error ??= `Could not read the voice settings: ${message(e)}`),
    );
    // Listen first, then load, so a status change in between is not lost.
    void api
      .onVoiceStatus(loadStatus)
      .then((un) => (closed ? un() : (offStatus = un)))
      .finally(() => void loadStatus());
    // A grant made in System Settings shows when this window regains focus.
    window.addEventListener("focus", loadStatus);
    void api.onVoiceProbe((r) => (probeReports = [...probeReports, r])).then((un) => (closed ? un() : (offProbe = un)));
    void (async () => {
      try {
        const un = await api.onView(applyView);
        if (closed) un();
        else off = un;
        applyView(await api.getView());
      } catch (e) {
        error ??= `Could not load machine status: ${message(e)}`;
      }
    })();
    return () => {
      closed = true;
      off?.();
      offProbe?.();
      offStatus?.();
      window.removeEventListener("focus", loadStatus);
    };
  });

  const endpointText = (m: MachineEntryView) =>
    m.endpoint.kind === "local" ? "This Mac (acd.sock)" : `${m.endpoint.host}:${m.endpoint.port}`;

  const setVoice = (box: HTMLInputElement, field: "enabled" | "debug_input") =>
    act(async () => {
      try {
        voice = await api.setVoiceSettings({ ...voice, [field]: box.checked });
      } catch (e) {
        box.checked = voice[field];
        throw e;
      }
    }, "Could not change the voice settings");

  const setHoldKey = (select: HTMLSelectElement) =>
    act(async () => {
      try {
        voice = await api.setVoiceSettings({ ...voice, hold_key: select.value as HoldKey });
      } catch (e) {
        select.value = voice.hold_key;
        throw e;
      }
    }, "Could not change the hold key");

  const saveToggle = () =>
    act(async () => {
      voice = await api.setVoiceSettings({ ...voice, toggle_shortcut: toggleDraft.trim() || null });
    }, "Could not save the voice shortcut");

  const importPairing = () =>
    act(async () => {
      const m = await api.importPairingFromClipboard();
      added = `Added ${m.name}`;
    }, "Could not add the machine");

  const rename = (m: MachineEntryView) => {
    const name = (names[m.key] ?? m.name).trim();
    if (name === "") {
      error = "Could not rename: the name cannot be empty";
      return;
    }
    return act(() => api.renameMachine(m.key, name), "Could not rename");
  };

  // window.confirm silently returns false in Tauri's macOS webview, so
  // removal confirms inline; one machine at a time.
  let confirming = $state<string | null>(null);
  /** Moves focus to Cancel, so a second Enter or Space cancels too. */
  const focusNow = (node: HTMLElement) => node.focus();

  const remove = (m: MachineEntryView) => {
    if (confirming !== m.key) {
      confirming = m.key;
      return;
    }
    confirming = null;
    return act(() => api.removeMachine(m.key), "Could not remove");
  };

  const saveHotkey = () => act(() => api.setHotkey(hotkey), "Could not set the hotkey");

  const toggleIdle = (box: HTMLInputElement) =>
    act(async () => {
      const on = box.checked;
      try {
        await api.setShowIdle(on);
        showIdle = on;
      } catch (e) {
        // `checked` is one-way bound, so put the DOM back explicitly.
        box.checked = showIdle;
        throw e;
      }
    }, "Could not change idle agents");

  const toggleSubagents = (box: HTMLInputElement) =>
    act(async () => {
      const on = box.checked;
      try {
        await api.setShowSubagents(on);
        showSubagents = on;
      } catch (e) {
        box.checked = showSubagents;
        throw e;
      }
    }, "Could not change subagent threads");

  const toggleHookOnly = (box: HTMLInputElement) =>
    act(async () => {
      const on = box.checked;
      try {
        await api.setShowHookOnly(on);
        showHookOnly = on;
      } catch (e) {
        box.checked = showHookOnly;
        throw e;
      }
    }, "Could not change hook-only agents");
</script>

<main class="settings">
  <h1>Settings</h1>

  {#if error}
    <div class="error" role="alert">{error}</div>
  {/if}
  {#if added}
    <div class="ok" role="status">{added}</div>
  {/if}

  <section aria-label="Machines">
    <ul>
      {#each machines as m (m.key)}
        <li class="machine" data-key={m.key}>
          <div class="name">{m.name}</div>
          <div class="endpoint">{endpointText(m)}</div>
          <div class="pairing" data-pairing={m.pairing}>{pairingText[m.pairing]}</div>
          {#if m.pairing === "unreadable"}
            <div class="hint">
              The stored credentials for this machine cannot be read. Remove it and add it again to re-pair.
            </div>
          {/if}
          <div class="status">{statusText(statuses[m.key])}</div>
          <input aria-label={`Name for ${m.name}`} bind:value={names[m.key]} />
          <button onclick={() => rename(m)}>Rename</button>
          {#if statuses[m.key]?.kind !== "online"}
            <button onclick={() => act(() => api.retryMachine(m.key), "Could not retry")}>Retry</button>
          {/if}
          {#if m.endpoint.kind !== "local"}
            {#if confirming === m.key}
              <!-- Cancel takes Remove's place at the same width, so a double click cancels. -->
              <button class="slot" use:focusNow onclick={() => (confirming = null)}>Cancel</button>
              <button onclick={() => remove(m)}>Confirm remove</button>
            {:else}
              <button class="slot" onclick={() => remove(m)}>Remove</button>
            {/if}
            <span aria-live="polite">{confirming === m.key ? `Remove ${m.name}?` : ""}</span>
          {/if}
        </li>
      {/each}
    </ul>
  </section>

  <section aria-label="Add a machine">
    <p>
      On the machine, run <code>acd pair</code> and copy its line. Then click Add from clipboard.
      Copy something else afterwards; the line contains a secret.
    </p>
    <button onclick={importPairing}>Add from clipboard</button>
  </section>

  <section aria-label="Shortcut">
    <label>
      Hotkey
      <input bind:value={hotkey} oninput={() => (hotkeyEdited = true)} />
    </label>
    <button onclick={saveHotkey}>Save hotkey</button>
  </section>

  <section aria-label="Idle agents">
    <label>
      <input type="checkbox" checked={showIdle} onchange={(e) => toggleIdle(e.currentTarget)} />
      Show idle agents
    </label>
  </section>

  <section aria-label="Subagent threads">
    <label>
      <input type="checkbox" checked={showSubagents} onchange={(e) => toggleSubagents(e.currentTarget)} />
      Show subagent threads
    </label>
    <p class="note">Threads another agent started usually cannot be answered here. They are hidden, with their requests, counts and notifications, unless this is on.</p>
  </section>

  <section aria-label="Voice">
    <label>
      <input type="checkbox" checked={voice.enabled} disabled={!voiceLoaded} onchange={(e) => setVoice(e.currentTarget, "enabled")} />
      Voice control
    </label>

    {#if !statusLoaded}
      <!-- Nothing permission-related until the first status arrives. -->
    {:else if status.unavailable}
      <p class="note" role="status">{unavailableText[status.unavailable]}</p>
      {#if status.unavailable === "assets_missing"}
        <button onclick={() => act(() => api.installSpeechAssets(), "Could not start the download")}>Download speech model</button>
      {/if}
    {:else if status.locale}
      <p class="note">Speech: {status.locale}, transcribed on this Mac{status.streams_partials ? ", with live text" : ""}.</p>
    {/if}

    {#if statusLoaded}
    <div class="permissions" role="group" aria-label="Permissions">
      {#each rows as row (row.kind)}
        <div class="permission">
          <span class="name">{row.name}</span>
          <span class="state" data-state={row.state}>{stateText[row.state]}</span>
          {#if row.state === "not_determined"}
            <button aria-label="Ask for {row.name}" onclick={() => act(() => api.requestVoicePermission(row.kind), "Could not ask for the permission")}>Ask</button>
          {:else if row.state !== "granted"}
            <button aria-label="Open System Settings for {row.name}" onclick={() => act(() => api.openVoicePrivacy(row.kind), "Could not open System Settings")}>Open System Settings</button>
          {/if}
          <p class="note">{row.why}</p>
        </div>
      {/each}
    </div>
    {/if}

    {#if voiceLoaded}
      <label>
        Hold key
        <select aria-label="Hold key" value={voice.hold_key} disabled={!voice.enabled} onchange={(e) => setHoldKey(e.currentTarget)}>
          {#each HOLD_KEYS as [key, label] (key)}
            <option value={key}>{label}</option>
          {/each}
        </select>
      </label>
      {#if statusLoaded && voice.enabled && voice.hold_key !== "off" && !status.hold_key_active}
        <p class="note">
          {status.relaunch_for_hold_key
            ? "Quit and reopen Agent Caller to use the hold key."
            : status.input_monitoring === "granted"
              ? "The hold key is off. Turn voice off and on to try again."
              : "The hold key needs Input Monitoring."}
        </p>
      {/if}

      <label>
        Voice shortcut
        <input aria-label="Voice shortcut" value={toggleDraft} placeholder="Off" disabled={!voice.enabled} oninput={(e) => (toggleDraft = e.currentTarget.value)} />
      </label>
      <button disabled={!voice.enabled} onclick={saveToggle}>Save shortcut</button>
      {#if status.toggle_error}
        <p class="note" role="status">{withStop(status.toggle_error)} The previous shortcut stays active.</p>
      {/if}
    {/if}

    <p class="note">
      Hold the key to talk, or press the shortcut once to start and again to stop. Recording stops after 60 seconds.
      Audio and text stay on this Mac and are never saved. English only; the system microphone is used.
    </p>
    <label>
      <input
        type="checkbox"
        checked={voice.debug_input}
        disabled={!voice.enabled}
        onchange={(e) => setVoice(e.currentTarget, "debug_input")}
      />
      Show the typed command field
    </label>
    <p class="note">The typed field in the inbox panel handles commands as if spoken. Nothing you type is logged or saved.</p>
    {#if voice.enabled && voice.debug_input}
      <div class="probe" role="group" aria-label="Voice probe">
        <p class="note">Checks for the signed app. Reports show permission states, counts and timings, never what was said.</p>
        {#each PROBE_STEPS as [step, label] (step)}
          <button onclick={() => act(() => api.voiceProbe(step), "Could not run the probe")}>{label}</button>
        {/each}
        {#each probeReports as report, i (i)}
          <pre class="report">{report}</pre>
        {/each}
      </div>
    {/if}
  </section>

  <section aria-label="Hook-only agents">
    <label>
      <input type="checkbox" checked={showHookOnly} onchange={(e) => toggleHookOnly(e.currentTarget)} />
      Show hook-only agents
    </label>
    <p class="note">Sessions known only through hooks cannot be messaged or opened from here. They appear only while they have a request for you, unless this is on.</p>
  </section>
</main>

<style>
  .settings {
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-height: 100vh;
    padding: 16px;
    font: 13px var(--font-sans);
    color: var(--text);
    background: var(--canvas);
  }
  h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 600;
    letter-spacing: -0.025em;
  }
  section {
    padding: 12px 14px;
    background: var(--glass);
    border: 1px solid var(--hairline);
    border-radius: var(--r-md);
  }
  .probe {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .permissions {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .permission {
    display: grid;
    grid-template-columns: 1fr auto auto;
    gap: 2px 8px;
    align-items: baseline;
  }
  .permission .note {
    grid-column: 1 / -1;
    margin: 0;
  }
  .state[data-state="denied"],
  .state[data-state="restricted"] {
    color: var(--crimson-text);
  }
  .report {
    margin: 0;
    font: 11px var(--font-mono);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .note {
    margin: 6px 0 0;
    font-size: 12px;
    color: var(--text-dim);
  }
  section > p:not(.note) {
    margin: 0 0 10px;
    color: var(--text-muted);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .machine {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 8px;
    align-items: center;
    padding: 10px 0;
    border-bottom: 1px solid var(--hairline);
  }
  .machine:first-child {
    padding-top: 0;
  }
  .machine:last-child {
    padding-bottom: 0;
    border-bottom: 0;
  }
  .name {
    font-weight: 600;
  }
  .endpoint,
  .status,
  .pairing {
    font-size: 11px;
    color: var(--text-dim);
  }
  .endpoint {
    font-family: var(--font-mono);
  }
  .hint {
    flex-basis: 100%;
    font-size: 12px;
    color: var(--amber-text);
  }
  .machine input {
    flex: 1 1 8em;
  }
  .slot {
    min-width: 6em;
  }
  code {
    padding: 1px 5px;
    font: 12px var(--font-mono);
    background: var(--code-bg);
    border-radius: var(--r-sm);
  }
  label {
    display: inline-flex;
    gap: 8px;
    align-items: center;
  }
  input:not([type="checkbox"]) {
    height: 28px;
    padding: 0 10px;
    font: inherit;
    color: var(--text);
    background: var(--code-bg);
    border: 1px solid var(--stroke);
    border-radius: var(--r);
  }
  input:not([type="checkbox"]):focus {
    outline: none;
    border-color: rgba(6, 182, 212, 0.55);
    box-shadow: var(--glow);
  }
  input[type="checkbox"] {
    accent-color: var(--cyan);
  }
  button {
    height: 28px;
    padding: 0 12px;
    font: 500 12px var(--font-sans);
    color: var(--text);
    background: var(--fill-subtle);
    border: 1px solid var(--stroke);
    border-radius: var(--r);
    cursor: pointer;
  }
  button:hover {
    background: var(--fill-hover);
  }
  .error {
    padding: 6px 10px;
    color: var(--crimson-text);
    background: var(--crimson-fill);
    border-radius: var(--r);
  }
  .ok {
    padding: 6px 10px;
    color: var(--emerald);
    background: var(--emerald-fill);
    border-radius: var(--r);
  }
</style>
