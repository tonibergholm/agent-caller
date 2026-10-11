<script lang="ts">
  import type { CardView } from "../lib/bindings/CardView";
  import { since } from "../lib/format";
  import { age, toolName, typeTag } from "../lib/panel";
  import AnswerBox from "./AnswerBox.svelte";
  import UndoBar from "./UndoBar.svelte";

  let {
    card,
    selected,
    now,
    answer = $bindable(""),
    onpick,
    oncancel,
    onreply = () => {},
    onmarkseen = () => {},
    onanswer = () => {},
  }: {
    card: CardView;
    selected: boolean;
    now: number;
    answer?: string;
    onpick: (optionId: string, requestId: string, revision: number) => void;
    oncancel: (actionId: string) => void;
    onreply?: () => void;
    onmarkseen?: () => void;
    onanswer?: () => void;
  } = $props();

  const pending = $derived(card.agent.pending);
  const opened = $derived(age(pending?.opened_at, now));
  // Where the user answers a read-only request depends on where the agent lives.
  const sourceLabel = $derived(
    { herdr: "herdr", t3: "T3", hooks: "hooks", unknown: "unknown" }[card.agent.source],
  );
  const readonlyLabel = $derived(card.agent.source === "t3" ? "Answer in T3" : "Answer in terminal");
  const place = $derived(card.agent.project ?? card.agent.cwd?.split("/").filter(Boolean).at(-1) ?? null);

  // Permission detail: a Codex "Reason:" line and a title that is not just the
  // tool name read as prose; every other line is shown as the monospace block.
  const lines = $derived((pending?.detail ?? "").split("\n"));
  const reasonLine = $derived(lines[0]?.startsWith("Reason: ") ? lines[0] : null);
  const codeLines = $derived(reasonLine === null ? lines : lines.slice(1));
  const reasonTitle = $derived(pending && pending.kind !== "question" && !toolName(card) ? pending.title : null);
  const isQuestion = $derived(pending?.kind === "question");

  // For a question, acd copies the first question's text into `detail`. Show
  // the question text separately only when it carries something the detail does not.
  const q0 = $derived(pending?.questions?.[0] ?? null);
  const questionExtra = $derived.by(() => {
    const text = q0?.text ?? null;
    if (text === null || text.trim() === "") return null;
    return text.trim() === pending?.detail.trim() ? null : text;
  });
  const questionHeader = $derived(q0?.header && q0.header !== "Question" ? q0.header : null);
  const questionParts = $derived(
    [pending?.detail.trim() ? pending.detail : null, questionExtra].filter((t) => t !== null),
  );

  let questionEl = $state<HTMLElement>();
  let expanded = $state(false);
  let overflowing = $state(false);
  $effect(() => {
    void questionParts;
    if (questionEl && !expanded) overflowing = questionEl.scrollHeight > questionEl.clientHeight + 1;
  });

  const marker = (line: string): { mk: string; rest: string } | null =>
    line.startsWith("$ ") || line.startsWith("> ") ? { mk: line.slice(0, 2), rest: line.slice(2) } : null;
  // The first allow option the app can send is the primary button.
  const primaryId = $derived(
    pending?.options.find((o) => o.effect === "allow" && card.selectable_options.includes(o.id))?.id ?? null,
  );
  const effectClass = (effect: string, ok: boolean) => (ok && effect === "deny" ? "deny" : "");
</script>

<article
  class="card"
  class:selected
  class:offline={!card.live}
  data-key={card.key}
  aria-busy={card.busy}
>
  <header class="head">
    <h3 class="title">
      {card.agent.title}
      {#if card.busy}<span class="spinner" aria-hidden="true"></span>{/if}
    </h3>
    {#if opened}<time class="time" datetime={pending?.opened_at}>{opened}</time>{/if}
  </header>
  <div class="meta">
    <span class="machine">{card.machine_name}</span>
    <span class="source" data-source={card.agent.source}>{sourceLabel}</span>
    {#if place}<span class="project" title={card.agent.cwd ?? undefined}>{place}</span>{/if}
    <span class="tag">{typeTag(card)}</span>
  </div>
  {#if card.agent.last_message}
    <div class="last">{card.agent.last_message}</div>
  {/if}

  {#if !card.live}
    <div class="offline-note">
      {card.offline_since_ms !== null ? `offline since ${since(card.offline_since_ms, now)}` : "connecting…"}
    </div>
  {/if}

  {#if pending}
    {#if isQuestion}
      {#if questionHeader}<div class="qhead">{questionHeader}</div>{/if}
      <div class="question" class:clamped={!expanded} bind:this={questionEl}>
        {#each questionParts as part, i (i)}<p>{part}</p>{/each}
      </div>
      {#if overflowing || expanded}
        <button class="more" aria-expanded={expanded} onclick={() => (expanded = !expanded)}>
          {expanded ? "Show less" : "Show more"}
        </button>
      {/if}
    {:else}
      {#if reasonTitle}<div class="reason">{reasonTitle}</div>{/if}
      {#if reasonLine}<div class="reason">{reasonLine}</div>{/if}
      {#if codeLines.some((l) => l !== "")}
        <div class="detail">
          {#each codeLines as line, i (i)}
            {@const m = marker(line)}
            <div class="line" class:kv={m === null && line.startsWith('"')}>
              {#if m}<span class="mk">{m.mk}</span>{m.rest}{:else}{line}{/if}
            </div>
          {/each}
        </div>
      {/if}
    {/if}
    {#if pending.kind === "blocked_unparsed" && pending.raw_screen !== null}
      <pre>{pending.raw_screen}</pre>
    {/if}

    {#if card.live && card.answerable}
      {#if pending.options.length > 0}
        <div class="options" role="group" aria-label="Answers">
          {#each pending.options as o, i (o.id)}
            {@const ok = card.selectable_options.includes(o.id)}
            <button
              class="option {effectClass(o.effect, ok)}"
              class:primary={o.id === primaryId}
              disabled={!ok}
              title={ok ? (o.description ?? undefined) : "Not supported here"}
              onclick={() => onpick(o.id, pending.id, pending.revision)}
            >
              <kbd>{i + 1}</kbd>
              <span class="label">{o.label}</span>
              {#if o.description}<span class="desc">{o.description}</span>{/if}
            </button>
          {/each}
        </div>
      {/if}
      {#if card.free_text_answerable}
        <AnswerBox bind:value={answer} onsubmit={onanswer} disabled={card.busy} />
      {/if}
    {:else if card.live && !card.busy && pending.status !== "answering"}
      <div class="readonly">{readonlyLabel}</div>
    {/if}
  {/if}

  {#if card.live && (card.can_reply || card.can_mark_seen)}
    <div class="more-actions">
      {#if card.can_reply}<button class="ghost" onclick={onreply}><kbd>R</kbd> Reply</button>{/if}
      {#if card.can_mark_seen}<button class="ghost" onclick={onmarkseen}><kbd>E</kbd> Mark seen</button>{/if}
    </div>
  {/if}

  {#if card.live && card.action}
    <UndoBar action={card.action} {now} {oncancel} />
  {/if}

  {#if card.notice}
    <div class="notice {card.notice.tone}" role="status" data-tone={card.notice.tone}>
      {card.notice.text}
    </div>
  {/if}
</article>

<style>
  .card {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px 12px 16px;
    overflow: hidden;
    background: var(--glass);
    border: 1px solid var(--hairline);
    border-radius: var(--r-md);
  }
  .card::before {
    content: "";
    position: absolute;
    inset: 0 auto 0 0;
    width: 3px;
    background: var(--amber);
  }
  .card.selected {
    border-color: rgba(6, 182, 212, 0.4);
    box-shadow: var(--glow);
  }
  .card.offline {
    opacity: 0.5;
    filter: grayscale(1);
  }
  .head {
    display: flex;
    gap: 8px;
    align-items: baseline;
    justify-content: space-between;
  }
  .title {
    flex: 1;
    min-width: 0;
    margin: 0;
    overflow: hidden;
    font-size: 14px;
    font-weight: 600;
    letter-spacing: -0.02em;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .time {
    flex: none;
    font-size: 11px;
    color: var(--text-dim);
  }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    font-size: 11px;
    color: var(--text-muted);
  }
  .machine,
  .source,
  .project,
  .tag {
    display: inline-flex;
    align-items: center;
    height: 18px;
    padding: 0 6px;
    border-radius: var(--r-sm);
  }
  .machine {
    font-family: var(--font-mono);
    background: var(--fill-subtle);
    border: 1px solid var(--stroke);
  }
  .source {
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.04em;
    border: 1px solid currentColor;
    border-radius: var(--r-full);
    opacity: 0.8;
  }
  .project {
    max-width: 14em;
    padding: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    display: inline-block;
    line-height: 18px;
  }
  .tag {
    font-size: 10px;
    font-weight: 700;
    letter-spacing: 0.06em;
    color: var(--amber-text);
    background: var(--amber-fill);
  }
  .last,
  .offline-note {
    font-size: 12px;
    color: var(--text-dim);
  }
  .reason {
    color: var(--text);
  }
  .qhead {
    font-weight: 600;
  }
  .question {
    font-size: 13.5px;
    line-height: 1.45;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .question p {
    margin: 0;
  }
  .question p + p {
    margin-top: 6px;
  }
  .question.clamped {
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .more {
    align-self: flex-start;
    padding: 0;
    font: 500 12px var(--font-sans);
    color: var(--cyan-bright);
    background: none;
    border: 0;
    cursor: pointer;
  }
  .detail {
    padding: 8px 10px;
    font: 400 12px/1.3rem var(--font-mono);
    letter-spacing: 0;
    background: var(--code-bg);
    border: 1px solid var(--hairline);
    border-radius: var(--r-sm);
  }
  /* One block per logical line with a hanging indent: a wrapped row is
     indented, so only a real line starts at the left edge. */
  .detail .line {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    padding-left: 2ch;
    text-indent: -2ch;
    min-height: 1.2em;
  }
  .detail .kv {
    color: var(--text-muted);
  }
  .mk {
    color: var(--cyan-bright);
  }
  pre {
    max-height: 160px;
    margin: 0;
    padding: 8px 10px;
    overflow: auto;
    white-space: pre-wrap;
    font: 11px/1rem var(--font-mono);
    background: var(--code-bg);
    border-radius: var(--r-sm);
  }
  .options {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .option,
  .ghost {
    display: inline-flex;
    gap: 7px;
    align-items: center;
    min-height: 28px;
    padding: 4px 10px 4px 5px;
    font: 500 12.5px var(--font-sans);
    color: var(--text);
    background: var(--fill-subtle);
    border: 1px solid var(--stroke);
    border-radius: var(--r);
    cursor: pointer;
    transition: background 120ms;
  }
  .ghost {
    padding-right: 10px;
    font-size: 12px;
    color: var(--text-muted);
  }
  .option:hover:not(:disabled),
  .ghost:hover {
    background: var(--fill-hover);
  }
  .option:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .option.primary {
    font-weight: 600;
    color: var(--canvas-deep);
    background: var(--cyan);
    border-color: var(--cyan);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.3);
  }
  .option.primary:hover:not(:disabled) {
    background: var(--cyan-bright);
  }
  .option.deny {
    color: var(--crimson-text);
    background: var(--crimson-fill);
    border-color: rgba(239, 68, 68, 0.35);
  }
  kbd {
    display: inline-grid;
    place-items: center;
    min-width: 18px;
    height: 18px;
    padding: 0 4px;
    font: 500 10.5px var(--font-mono);
    color: rgba(255, 255, 255, 0.5);
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: var(--r-sm);
  }
  .option.primary kbd {
    color: rgba(5, 8, 15, 0.7);
    border-color: rgba(5, 8, 15, 0.3);
  }
  .desc {
    max-width: 22em;
    font: 400 11px var(--font-mono);
    opacity: 0.65;
    overflow-wrap: anywhere;
    text-align: left;
  }
  .more-actions {
    display: flex;
    gap: 6px;
  }
  .readonly {
    font-size: 12px;
    color: var(--text-dim);
  }
  .notice {
    font-size: 12px;
  }
  .notice.warning {
    color: var(--amber-text);
  }
  .spinner {
    display: inline-block;
    width: 10px;
    height: 10px;
    vertical-align: -1px;
    border: 2px solid var(--cyan);
    border-right-color: transparent;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
