<script lang="ts">
  import { onMount } from "svelte";
  import type { MachineEntryView } from "../lib/bindings/MachineEntryView";
  import type { MachineStatus } from "../lib/bindings/MachineStatus";
  import type { Api } from "../lib/ipc";

  let { api }: { api: Api } = $props();

  let machines = $state<MachineEntryView[]>([]);
  let statuses = $state<Record<string, MachineStatus>>({});
  let showIdle = $state(false);
  let hotkey = $state("");
  let names = $state<Record<string, string>>({});
  let error = $state<string | null>(null);
  let added = $state<string | null>(null);

  const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

  const statusText = (s: MachineStatus | undefined) =>
    s ? s.kind.replace(/_/g, " ") + ("message" in s ? `: ${s.message}` : "") + ("error" in s ? `: ${s.error}` : "") : "";

  const pairingText = {
    paired: "paired",
    not_paired: "not paired",
    unreadable: "stored credentials unreadable",
  } as const;

  let hotkeyEdited = false;

  function applyView(v: { machines: { key: string; status: MachineStatus }[]; show_idle: boolean }) {
    const known = new Set(machines.map((m) => m.key));
    if (v.machines.length !== known.size || v.machines.some((m) => !known.has(m.key))) {
      refresh().catch((e) => (error ??= `Could not list machines: ${message(e)}`));
    }
    statuses = Object.fromEntries(v.machines.map((m) => [m.key, m.status]));
    showIdle = v.show_idle;
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
    let closed = false;
    void act(async () => {
      const stored = await api.getHotkey();
      if (!hotkeyEdited) hotkey = stored;
    }, "Could not read the hotkey");
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
    };
  });

  const endpointText = (m: MachineEntryView) =>
    m.endpoint.kind === "local" ? "This Mac (acd.sock)" : `${m.endpoint.host}:${m.endpoint.port}`;

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

  const remove = (m: MachineEntryView) => {
    if (!window.confirm(`Remove ${m.name}?`)) return;
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
          <button onclick={() => act(() => api.retryMachine(m.key), "Could not retry")}>Retry</button>
          {#if m.endpoint.kind !== "local"}
            <button onclick={() => remove(m)}>Remove</button>
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
</main>

<style>
  .settings {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 12px;
    font: 13px system-ui, sans-serif;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .machine {
    padding: 6px 0;
    border-bottom: 1px solid #8884;
  }
  .endpoint,
  .status {
    font-size: 11px;
    opacity: 0.7;
  }
  .error {
    color: #c00;
  }
</style>
