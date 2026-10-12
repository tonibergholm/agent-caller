// Dev-only: renders the panel and settings with fixture data in a plain
// browser (`npm run dev`, then /preview.html?scene=main). Not part of the
// production build, which only bundles index.html.
import { mount } from "svelte";
import "./styles/tokens.css";
import App from "./App.svelte";
import { card, fakeApi, view } from "./lib/fixtures";
import { setPreviewMobile } from "./lib/platform";
import type { CardView } from "./lib/bindings/CardView";
import type { InboxView } from "./lib/bindings/InboxView";
import type { MachineView } from "./lib/bindings/MachineView";

const params = new URLSearchParams(location.search);
const scene = params.get("scene") ?? "main";
// `?mobile` shows the touch layout without `npm run dev:ios`. Local cards move
// to dev-box, because the phone has no local machine.
const onPhone = params.has("mobile");
setPreviewMobile(onPhone ? true : null);
const now = Date.now();
const ago = (s: number) => new Date(now - s * 1000).toISOString();

const machines: MachineView[] = [
  { key: "local", name: "This Mac", machine_id: "m1", status: { kind: "online" }, is_local: true, duplicate_of: null },
  { key: "dev", name: "dev-box", machine_id: "m2", status: { kind: "online" }, is_local: false, duplicate_of: null },
  { key: "gpu", name: "gpu-node", machine_id: "m3", status: { kind: "syncing" }, is_local: false, duplicate_of: null },
];

function mk(key: string, machineIn: string, over: Partial<CardView>, agent: Partial<CardView["agent"]>): CardView {
  const machine = onPhone && machineIn === "local" ? "dev" : machineIn;
  const c = card({ key: `${machine}/${key}`, machine_key: machine, machine_name: machines.find((m) => m.key === machine)!.name, ...over });
  c.agent = { ...c.agent, ...agent };
  return c;
}

const bash = mk("a", "local", { answerable: true, selectable_options: ["allow", "deny", "always"] }, {
  title: "Fix the failing build",
  cwd: "/Users/dev/agent-caller",
  project: "agent-caller",
  pending: {
    ...card().agent.pending!,
    id: "r1",
    title: "Bash",
    detail: "$ cargo test --workspace --locked\n> && cargo clippy --workspace -- -D warnings\n\"timeout\": 120000",
    options: card().agent.pending!.options.filter((o) => o.id !== "weird"),
    opened_at: ago(12),
  },
});

const codex = mk("b", "dev", { answerable: true, selectable_options: ["allow", "deny"] }, {
  title: "Refactor the auth module",
  source: "herdr",
  kind: "codex",
  cwd: "/home/dev/auth-service",
  project: "auth-service",
  pending: {
    ...card().agent.pending!,
    id: "r2",
    title: "Bash",
    detail: "Reason: Install the dependency the tests need\n$ npm install --save-dev vitest",
    options: [
      { id: "allow", label: "Allow once", description: null, effect: "allow" },
      { id: "deny", label: "Deny", description: null, effect: "deny" },
    ],
    opened_at: ago(48),
  },
});

const t3q = mk("c", "local", { answerable: true, free_text_answerable: true }, {
  title: "Plan the OAuth2 rollout",
  source: "t3",
  cwd: "/Users/dev/portal",
  project: "portal",
  pending: {
    ...card().agent.pending!,
    id: "r3",
    kind: "question",
    title: "Provider",
    detail:
      "Which authentication provider should we prioritise for the OAuth2 client? The core identity endpoints are scaffolded and I am blocked on the initial callback token signature strategy. Name a provider, or describe the constraint you care about most, and I will pick the library to match. Please also say whether a self-hosted option is acceptable.",
    options: [],
    free_text: true,
    questions: [{ id: "q", header: "Provider", text: "", multi_select: false, options: [] }],
    opened_at: ago(190),
  },
});
t3q.agent.pending!.questions![0].text = t3q.agent.pending!.detail;

const choice = mk("d", "dev", { answerable: true, selectable_options: ["o1", "o2", "o3"] }, {
  title: "Pick a database",
  cwd: "/home/dev/orders",
  project: "orders",
  pending: {
    ...card().agent.pending!,
    id: "r4",
    kind: "question",
    title: "Database",
    detail: "Which database should the orders service use?",
    options: [
      { id: "o1", label: "PostgreSQL", description: null, effect: "choose" },
      { id: "o2", label: "SQLite", description: null, effect: "choose" },
      { id: "o3", label: "DynamoDB", description: null, effect: "choose" },
    ],
    questions: [{ id: "q", header: "Database", text: "Which database should the orders service use?", multi_select: false, options: [] }],
    opened_at: ago(3600 * 2),
  },
});

const work = (key: string, machine: string, title: string, last: string, secs: number) => {
  const c = mk(key, machine, { group: "working" }, { title, last_message: last, state: "working", pending: null, state_since: ago(secs) });
  return c;
};
const fin = (key: string, machine: string, title: string, secs: number) =>
  mk(key, machine, { group: "done", can_mark_seen: true, can_reply: true }, { title, state: "done", pending: null, state_since: ago(secs) });

const cards: CardView[] = [
  bash,
  codex,
  t3q,
  choice,
  work("w1", "local", "Migrate the sessions table", "Running the backfill (4 of 9 batches)", 240),
  work("w2", "dev", "Write the release notes", "Reading the merged PRs", 35),
  fin("f1", "local", "Update the README", 600),
  fin("f2", "dev", "Bump dependencies", 3600 * 5),
];

let v: InboxView = view({ machines, cards, hidden_idle: 2 });

if (scene === "undo") {
  const c = cards[0];
  v = { ...v, cards: [{ ...c, selectable_options: c.selectable_options, action: { id: "a1", card_key: c.key, label: "Yes, always", detail: "Bash(cargo test --workspace --locked)", fires_at_ms: now + 7300 /* headless shots run ~6 s of virtual time */ } }, ...cards.slice(1)] };
}
if (scene === "subagent") {
  const sub = mk("sa", "local", { answerable: true, selectable_options: ["allow", "deny"] }, {
    title: "Explore the auth module",
    source: "t3",
    role: "subagent",
    parent_title: "Plan the OAuth2 rollout",
    cwd: "/Users/dev/portal",
    project: "portal",
    pending: {
      ...card().agent.pending!,
      id: "r9",
      title: "Bash",
      detail: "$ grep -rn \"token\" src/auth",
      options: card().agent.pending!.options.filter((o) => o.id === "allow" || o.id === "deny"),
      opened_at: ago(20),
    },
  });
  const subw = work("sw", "local", "Index the docs", "Reading 40 files", 90);
  subw.agent.role = "subagent";
  subw.agent.parent_title = "Plan the OAuth2 rollout";
  v = { ...v, show_subagents: true, cards: [sub, bash, subw, ...cards.slice(4)] };
}
if (scene === "noreq") {
  // A needs-you card whose request is already gone: says where to answer.
  v = { ...v, cards: cards.map((c, i) => (i === 1 ? { ...c, agent: { ...c.agent, pending: null } } : c)) };
}
if (scene === "banner") {
  // Long unbroken paths in the banners: they must wrap, not run off the edge.
  const long = "/Users/dev/Library/Application Support/agent-caller/very/long/unbroken/path/without/spaces/acd.sock";
  const localBanner: MachineView["status"] = { kind: "local_unavailable", message: long };
  const remoteBanner: MachineView["status"] = { kind: "auth_failed", message: `token file ${long}` };
  v = {
    ...v,
    machines: machines.map((m) => (m.key === "local" ? { ...m, status: localBanner } : m.key === "gpu" ? { ...m, status: remoteBanner } : m)),
  };
}
if (scene === "many") {
  // Six remote machines: the header dots must scroll, not overflow the screen.
  const extra: MachineView[] = ["box-a", "box-b", "box-c", "box-d", "box-e", "box-f"].map((name, i) => ({
    key: `m${i}`,
    name,
    machine_id: `mm${i}`,
    status: i % 2 ? { kind: "offline", since_ms: now - 60_000, error: "refused" } : { kind: "online" },
    is_local: false,
    duplicate_of: null,
  }));
  v = { ...v, machines: [...machines, ...extra] };
}
if (scene === "empty") v = view({ machines });
if (scene === "offline") {
  const off = machines.map((m) => (m.key === "gpu" ? { ...m, status: { kind: "offline" as const, since_ms: now - 300_000, error: "connection refused" } } : m));
  v = { ...v, machines: off, cards: cards.map((c) => (c.machine_key === "dev" ? { ...c, live: false, offline_since_ms: now - 300_000, answerable: false } : c)) };
}

const api = fakeApi({
  machines: machines.map((m) => ({ key: m.key, name: m.name, endpoint: m.is_local ? { kind: "local" } : { kind: "remote", host: `${m.key}.tailnet.ts.net`, port: 7733 }, pairing: "paired" }) as never),
});
api.getView = async () => v;
api.onView = async () => () => {};
api.getHotkey = async () => "Control+Option+Space";
api.notificationAuthorization = async () => "not_determined";
api.requestNotificationAuthorization = async () => "granted";

document.documentElement.dataset.scene = scene;
mount(App, { target: document.getElementById("app")!, props: { api, label: scene === "settings" ? "settings" : "panel" } });

// Interactions for screenshots.
queueMicrotask(() => setTimeout(() => {
  if (params.get("done") === "1") document.querySelector<HTMLButtonElement>(".toggle")?.click();
  const chip = params.get("filter");
  if (chip) [...document.querySelectorAll<HTMLButtonElement>(".fchip")].find((b) => b.textContent?.includes(chip))?.click();
  if (params.get("type") === "1") document.querySelector<HTMLTextAreaElement>("textarea[data-field=answer]")?.focus();
  const typed = params.get("typed");
  if (typed) {
    const box = document.querySelector<HTMLTextAreaElement>("textarea[data-field=answer]");
    if (box) { box.value = typed; box.dispatchEvent(new Event("input", { bubbles: true })); }
  }
}, 200));
