<script lang="ts">
  import { onMount, tick } from "svelte";
  import type { InboxView } from "../lib/bindings/InboxView";
  import { awaitingDisplay, outcomeText, refusalText, wantsKey, type Hud, type ProposalView } from "../lib/hud";
  import type { Api } from "../lib/ipc";

  /** acd-client's undo window (spec §8): the bar's full width. */
  const WINDOW_MS = 2000;

  let { api, view = null }: { api: Api; view?: InboxView | null } = $props();

  let hud = $state<Hud>({ state: "hidden" });
  let now = $state(Date.now());
  // The last state shown; older events are dropped (they can arrive late).
  const seen = { seq: 0 };
  // The input id of the state on screen (`HudEvent.id`), sent back with Esc, clicks and digits.
  let shownId = $state<number | null>(null);

  // Rejections are dropped silently: the console never sees voice text.
  const quiet = (p: Promise<unknown>) => void p.catch(() => {});

  onMount(() => {
    const offs: (() => void)[] = [];
    let closed = false;
    const keep = (un: () => void) => (closed ? un() : offs.push(un));
    api
      .onHud((e) => {
        if (e.seq <= seen.seq) return;
        seen.seq = e.seq;
        hud = e.hud;
        shownId = e.id;
        const id = awaitingDisplay(e.hud);
        if (id === null) return;
        // Acknowledge only after the DOM shows it, and only if it is still
        // the state on screen: two events before one flush must not
        // acknowledge a proposal that never rendered (D22).
        quiet(
          tick().then(() => {
            if (seen.seq === e.seq && awaitingDisplay(hud) === id) return api.voiceShown(id);
          }),
        );
      })
      .then(keep, () => {});
    api
      .onHudClear((seq) => {
        if (seq !== seen.seq) return; // a stale clear
        hud = { state: "hidden" };
        shownId = null;
      })
      .then(keep, () => {});
    const timer = setInterval(() => (now = Date.now()), 100);
    return () => {
      closed = true;
      offs.forEach((un) => un());
      clearInterval(timer);
    };
  });

  const action = $derived.by(() => {
    if (hud.state !== "countdown" || hud.action_id === null) return null;
    // Copied out of `hud`: TypeScript does not keep the narrowing inside the callback.
    const id = hud.action_id;
    return view?.actions.find((a) => a.id === id) ?? null;
  });
  const remaining = $derived(action ? Math.max(0, action.fires_at_ms - now) : 0);
  const fraction = $derived(Math.min(1, remaining / WINDOW_MS));

  const confirmStep = (id: number) => quiet(api.voiceConfirm(id));
  // A state without an input id has nothing to cancel: send nothing.
  const cancel = () => {
    if (shownId !== null) quiet(api.voiceCancel(shownId));
  };
  const choose = (choiceId: number, i: number) => quiet(api.voiceChoose(choiceId, i));

  function verb(p: ProposalView): string {
    switch (p.action) {
      case "answer":
        return `Answer “${p.option ?? "?"}”`;
      case "answer_text":
        return "Type this answer";
      case "send":
        return "Send this message";
    }
  }

  function key(e: KeyboardEvent) {
    if (!wantsKey(hud) || e.isComposing || e.keyCode === 229 || e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === "Escape") {
      e.preventDefault();
      cancel();
    } else if (e.key === "Enter" && hud.state === "confirm") {
      e.preventDefault();
      confirmStep(hud.proposal_id);
    } else if (hud.state === "which_one" && /^[1-9]$/.test(e.key)) {
      const i = Number(e.key) - 1;
      if (i < hud.candidates.length) {
        e.preventDefault();
        choose(hud.choice_id, i);
      }
    }
  }
</script>

<svelte:window onkeydown={key} />

{#if hud.state !== "hidden"}
  <main class="hud" data-state={hud.state} aria-live="polite">
    <div class="body">
      {#if hud.state === "listening"}
        <p class="status"><span class="mic" aria-hidden="true"></span>Listening…</p>
      {:else if hud.state === "thinking"}
        <p class="status"><span class="pulse" aria-hidden="true"></span>Thinking</p>
        <p class="said">“{hud.transcript}”</p>
      {:else if hud.state === "confirm" || hud.state === "countdown" || hud.state === "sending"}
        {@const p = hud.proposal}
        <header class="target">
          <span class="agent">{p.target.agent}</span>
          <span class="machine" class:off-screen={p.target.off_screen}>@ {p.target.machine}</span>
          <span class="title">{p.target.title}</span>
        </header>
        <p class="what">{verb(p)}</p>
        {#if p.question}<pre class="block">{p.question}</pre>{/if}
        {#if p.rule}<p class="label">Always allow, from now on:</p><pre class="block rule">{p.rule}</pre>{/if}
        {#if p.text}<pre class="block payload">{p.text}</pre>{/if}
        {#if hud.state === "sending"}
          <p class="status">{hud.too_late_to_cancel ? "Already sent. It can't be undone." : "Sent. Waiting for the result."}</p>
        {/if}
      {:else if hud.state === "which_one"}
        {@const choiceId = hud.choice_id}
        <p class="what">Which one?</p>
        <ol class="candidates">
          {#each hud.candidates as c, i (c.card)}
            <li>
              <button onclick={() => choose(choiceId, i)}>
                <kbd>{i + 1}</kbd>
                <span class="agent">{c.agent}</span>
                <span class="machine" class:off-screen={c.off_screen}>@ {c.machine}</span>
                <span class="title">{c.title}</span>
                {#if c.request}<span class="tag">{c.request}</span>{/if}
              </button>
            </li>
          {/each}
        </ol>
      {:else if hud.state === "done"}
        <p class="status" data-outcome={hud.outcome.kind}>{outcomeText(hud.outcome)}</p>
      {:else if hud.state === "error"}
        <p class="status error">{refusalText[hud.refusal]}</p>
        {#if hud.transcript}<p class="said">“{hud.transcript}”</p>{/if}
      {:else if hud.state === "speaking"}
        <p class="speech">{hud.text}</p>
      {/if}
    </div>
    {#if hud.state === "confirm"}
      {@const id = hud.proposal_id}
      <div class="buttons">
        <button class="primary" onclick={() => confirmStep(id)}>Confirm <kbd>⏎</kbd></button>
        <button onclick={cancel}>Cancel <kbd>Esc</kbd></button>
      </div>
    {:else if hud.state === "countdown"}
      <div class="buttons">
        <button class="undo" onclick={cancel}>
          {#if hud.action_id === null}Starting…{:else}Sending in {(remaining / 1000).toFixed(1)}s{/if} · <kbd>Esc</kbd> to cancel
        </button>
      </div>
      {#if hud.action_id !== null}
        <span class="track" aria-hidden="true"><span class="bar" style:width="{fraction * 100}%"></span></span>
      {/if}
    {:else if hud.state === "which_one"}
      <div class="buttons"><button onclick={cancel}>Cancel <kbd>Esc</kbd></button></div>
    {/if}
  </main>
{/if}

<style>
  .hud {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 8px;
    height: 100vh;
    box-sizing: border-box;
    padding: 14px 16px 18px;
    overflow: hidden;
    color: var(--text);
    background: var(--glass-strong);
    backdrop-filter: blur(24px);
    border-radius: var(--r);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.12),
      0 0 0 1px rgba(6, 182, 212, 0.4);
  }
  /* Scrolls; the buttons below never do, so Cancel stays reachable. */
  .body {
    display: flex;
    flex: 1 1 auto;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
    overflow-y: auto;
  }
  .target {
    display: flex;
    gap: 8px;
    align-items: baseline;
    min-width: 0;
  }
  .agent {
    font-weight: 600;
  }
  .machine {
    font: 500 11px var(--font-mono);
    color: var(--text-dim);
  }
  .machine.off-screen {
    color: var(--amber-text);
  }
  .title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-muted);
  }
  .what {
    margin: 0;
    font-size: 15px;
    font-weight: 600;
    letter-spacing: -0.015em;
  }
  .label {
    margin: 0;
    font-size: 11px;
    color: var(--amber-text);
  }
  /* The whole payload, scrolling inside its own bounded block. */
  .block {
    flex: none;
    margin: 0;
    max-height: 96px;
    overflow: auto;
    padding: 6px 8px;
    font: 12px/1.3rem var(--font-mono);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    background: var(--code-bg);
    border-radius: var(--r-sm);
  }
  .rule {
    color: var(--amber-text);
  }
  .said {
    margin: 0;
    color: var(--text-muted);
  }
  .status {
    display: flex;
    gap: 8px;
    align-items: center;
    margin: 0;
    font-weight: 500;
  }
  .status.error {
    color: var(--crimson-text);
  }
  .speech {
    margin: 0;
    font-size: 15px;
    line-height: 1.45rem;
  }
  .buttons {
    display: flex;
    flex: none;
    gap: 8px;
  }
  button {
    display: inline-flex;
    gap: 6px;
    align-items: center;
    padding: 5px 10px;
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
  .primary {
    font-weight: 600;
    color: var(--canvas-deep);
    background: var(--cyan);
    border-color: var(--cyan);
  }
  .undo {
    color: var(--cyan-bright);
    background: var(--cyan-fill);
    border-color: rgba(6, 182, 212, 0.4);
  }
  kbd {
    font: 500 10px/1 var(--font-mono);
    padding: 1px 4px;
    color: rgba(255, 255, 255, 0.5);
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: var(--r-sm);
  }
  .candidates {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .candidates button {
    width: 100%;
    text-align: left;
  }
  .tag {
    margin-left: auto;
    font: 600 10px var(--font-sans);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--amber-text);
  }
  .track {
    position: absolute;
    inset: auto 0 0 0;
    height: 3px;
    background: var(--canvas-deep);
  }
  .bar {
    display: block;
    height: 100%;
    background: var(--cyan);
    transition: width 120ms linear;
  }
  .mic {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--crimson);
    box-shadow: 0 0 6px rgba(239, 68, 68, 0.6);
  }
  .pulse {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--cyan);
    animation: pulse 1.6s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.3;
    }
  }
</style>
