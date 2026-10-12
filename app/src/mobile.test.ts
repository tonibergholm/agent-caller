// Touch-first (iOS) layout. Every test here runs with VITE_PLATFORM=ios, the
// value `npm run build:ios` sets; the macOS defaults are covered by the other
// test files, which run without the stub.
import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import App from "./App.svelte";
import Card from "./components/Card.svelte";
import Panel from "./components/Panel.svelte";
import Row from "./components/Row.svelte";
import Settings from "./components/Settings.svelte";
import { card, fakeApi, view } from "./lib/fixtures";
import type { CardView } from "./lib/bindings/CardView";
import type { MachineView } from "./lib/bindings/MachineView";
import { VOICE_OFF } from "./lib/voice";
import panelSource from "./components/Panel.svelte?raw";

beforeEach(() => vi.stubEnv("VITE_PLATFORM", "ios"));
afterEach(() => {
  cleanup();
  vi.unstubAllEnvs();
});

const settle = () => new Promise((r) => setTimeout(r, 20));

const cardProps = (c: CardView, now = 0) => ({
  card: c,
  selected: false,
  now,
  onpick: () => {},
  oncancel: () => {},
});

const localMachine: MachineView = {
  key: "local",
  name: "This Mac",
  machine_id: "m1",
  status: { kind: "local_unavailable", message: "socket /Users/dev/Library/Application Support/agent-caller/acd.sock" },
  is_local: true,
  duplicate_of: null,
};
const devMachine: MachineView = {
  key: "dev",
  name: "dev-box",
  machine_id: "m2",
  status: { kind: "online" },
  is_local: false,
  duplicate_of: null,
};

function workingCard(key: string, over: Partial<CardView> = {}): CardView {
  const c = card({ key, group: "working", live: true, can_reply: true, can_mark_seen: true, ...over });
  c.agent.pending = null;
  c.agent.state = "working";
  return c;
}

// App: the local machine is not a mobile machine.

test("mobile app: the local machine, its card and its banner are not shown", async () => {
  const api = fakeApi();
  const localCard = card({ key: "local/a", machine_key: "local", machine_name: "This Mac" });
  localCard.agent.title = "Local task";
  const devCard = card({ key: "dev/b", machine_key: "dev", machine_name: "dev-box" });
  devCard.agent.title = "Remote task";
  api.getView = async () => view({ machines: [localMachine, devMachine], cards: [localCard, devCard] });
  const { container } = render(App, { api, label: "panel" });
  expect(await screen.findByText("Remote task")).toBeTruthy();
  expect(screen.queryByText("Local task")).toBeNull();
  expect(container.querySelector('[data-status="local_unavailable"]')).toBeNull();
  expect(container.textContent).not.toMatch(/local unavailable/i);
  expect(container.textContent).not.toContain("This Mac");
});

test("mobile app: voice input is never shown, even when voice is enabled", async () => {
  const api = fakeApi({ voice: { ...VOICE_OFF, enabled: true, debug_input: true } });
  render(App, { api, label: "panel" });
  await settle();
  expect(screen.queryByRole("textbox", { name: "Voice command" })).toBeNull();
});

test("mobile app: no hotkey is read", async () => {
  const api = fakeApi();
  render(App, { api, label: "panel" });
  await settle();
  expect(api.calls.map((c) => c[0])).not.toContain("getHotkey");
});

// Panel: footer, hints, drag and Esc.

test("mobile panel: the footer shows no key hints but keeps Show idle", () => {
  const { container } = render(Panel, { view: view({ hidden_idle: 2 }), api: fakeApi() });
  expect(container.querySelector("footer .keys")).toBeNull();
  expect(screen.getByRole("button", { name: "Show 2 idle" })).toBeTruthy();
});

test("mobile panel: the empty state has no hotkey hint", () => {
  render(Panel, { view: view(), api: fakeApi(), hotkey: "Control+Option+Space" });
  expect(screen.getByText("No agents need you.")).toBeTruthy();
  expect(screen.queryByText(/Press /)).toBeNull();
});

test("mobile panel: no drag region and no grip", () => {
  const { container } = render(Panel, { view: view(), api: fakeApi() });
  expect(container.querySelector("[data-tauri-drag-region]")).toBeNull();
  expect(container.querySelector(".grip")).toBeNull();
});

test("mobile panel: Esc with nothing to cancel does not ask the window to hide", async () => {
  const api = fakeApi();
  render(Panel, { view: view(), api });
  await userEvent.keyboard("{Escape}");
  expect(api.calls.map((c) => c[0])).not.toContain("hide");
});

// Panel: touch reaches every action.

test("mobile panel: a working row offers Reply without being selected", () => {
  const api = fakeApi();
  // A needs-you card comes first, so it is the selection; the row is not selected.
  render(Panel, { view: view({ cards: [card({ key: "dev/a" }), workingCard("dev/w")] }), api });
  expect(screen.getByRole("button", { name: /reply/i })).toBeTruthy();
  expect(screen.getByRole("button", { name: /mark seen/i })).toBeTruthy();
});

test("mobile panel: Reply sends the typed text; Cancel closes the box without sending", async () => {
  const api = fakeApi();
  const { container } = render(Panel, { view: view({ cards: [card({ key: "dev/a" }), workingCard("dev/w")] }), api });
  await userEvent.click(screen.getByRole("button", { name: /reply/i }));
  const box = container.querySelector<HTMLTextAreaElement>("textarea[data-field=reply]")!;
  expect(box).not.toBeNull();
  await userEvent.type(box, "hello");
  await userEvent.click(screen.getByRole("button", { name: "Send" }));
  expect(api.calls).toContainEqual(["reply", "dev/w", "hello"]);
  expect(container.querySelector("textarea[data-field=reply]")).toBeNull();

  await userEvent.click(screen.getByRole("button", { name: /reply/i }));
  await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(container.querySelector("textarea[data-field=reply]")).toBeNull();
  expect(api.calls.filter((c) => c[0] === "reply")).toHaveLength(1);
});

test("mobile panel: a machine dot is a button that shows its status as text", async () => {
  const { container } = render(Panel, {
    view: view({
      machines: [
        localMachine,
        { ...devMachine, name: "box", status: { kind: "offline", since_ms: 0, error: "refused" } },
      ],
    }),
    api: fakeApi(),
  });
  expect(container.querySelector(".dot-detail")).toBeNull();
  await userEvent.click(screen.getByRole("button", { name: "box: offline: refused" }));
  expect(container.querySelector(".dot-detail")?.textContent).toBe("box: offline: refused");
});

// Card: detail as text, option layout, undo.

test("mobile card: a disabled option says why in visible text, not a tooltip", () => {
  const c = card({ key: "m/x", answerable: true, selectable_options: ["allow", "deny"] });
  render(Card, { ...cardProps(c), onpick: () => {} });
  // "Yes, always" and "?" are not selectable here: two notes, no tooltips.
  expect(screen.getAllByText("Not supported here")).toHaveLength(2);
  const buttons = screen.getAllByRole("button");
  expect(buttons[2].getAttribute("title")).toBeNull();
  expect(buttons[3].getAttribute("title")).toBeNull();
});

test("mobile card: the directory is printed in the card, not only in a tooltip", () => {
  const { container } = render(Card, cardProps(card({ key: "m/x" })));
  expect(screen.getByText("/work/app")).toBeTruthy();
  expect(container.querySelector(".project")?.getAttribute("title")).toBeNull();
});

test("mobile card: a subagent's parent title is printed in full, not as a tooltip", () => {
  const c = card({ key: "m/x" });
  c.agent.role = "subagent";
  c.agent.parent_title = "Plan the OAuth2 rollout across every service";
  const { container } = render(Card, cardProps(c));
  expect(screen.getByText(/Plan the OAuth2 rollout across every service/)).toBeTruthy();
  expect(container.querySelector(".parent")?.getAttribute("title")).toBeNull();
});

test("mobile card: the countdown has a visible Cancel button and no Esc hint", async () => {
  const oncancel = vi.fn();
  const c = card({
    key: "m/x",
    action: { id: "a1", card_key: "m/x", label: "Yes", detail: null, fires_at_ms: 5000 },
  });
  const { container } = render(Card, { ...cardProps(c, 1000), oncancel });
  await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(oncancel).toHaveBeenCalledWith("a1");
  expect(container.textContent).not.toMatch(/esc/i);
  expect(container.querySelector("button.undo")).toBeNull();
});

// Row: same rules as the card.

test("mobile row: the title is not a tooltip and the directory is printed", () => {
  const { container } = render(Row, { card: workingCard("m/x"), selected: false, now: 0, oncancel: () => {} });
  expect(container.querySelector(".title")?.getAttribute("title")).toBeNull();
  expect(screen.getByText("/work/app")).toBeTruthy();
});

test("mobile panel: each machine dot is a 44 pt hit area with the 8 px dot drawn in its centre", () => {
  // jsdom does no layout or hit testing; the CSS contract is checked on the
  // source. The browser check (elementFromPoint at each dot centre) is the evidence.
  expect(panelSource).toMatch(/\.panel\.mobile \.dots button\.dot \{[^}]*width: 44px;[^}]*height: 44px;/);
  expect(panelSource).not.toMatch(/button\.dot::before/);
});

test("mobile panel: the reply Send button is disabled while the reply is blank", async () => {
  render(Panel, { view: view({ cards: [card({ key: "dev/a" }), workingCard("dev/w")] }), api: fakeApi() });
  await userEvent.click(screen.getByRole("button", { name: /reply/i }));
  expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(true);
});

test("mobile panel: no voice presentation or voice selection is wired, even with voice input on", async () => {
  const api = fakeApi();
  render(Panel, { view: view({ cards: [card({ key: "dev/a" })] }), api, voiceInput: true });
  await settle();
  expect(api.presentations).toEqual([]);
  expect(api.voiceSelect).toBeUndefined();
  expect(screen.queryByRole("textbox", { name: "Voice command" })).toBeNull();
});

test("mobile app: the gear opens Settings in the same window, and Back returns to the inbox", async () => {
  const api = fakeApi();
  api.getView = async () => view({ machines: [devMachine] });
  render(App, { api, label: "panel" });
  await userEvent.click(await screen.findByRole("button", { name: "Settings" }));
  expect(await screen.findByRole("heading", { name: "Settings" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Add from clipboard" })).toBeTruthy();
  expect(api.calls.map((c) => c[0])).not.toContain("openSettings");
  await userEvent.click(screen.getByRole("button", { name: "‹ Inbox" }));
  expect(await screen.findByText("No agents need you.")).toBeTruthy();
});

// Settings: no hotkey, notification or voice sections, and nothing read for them.

test("mobile settings: no shortcut, notification or voice section, and they are not read", async () => {
  const api = fakeApi();
  const { container } = render(Settings, { api });
  await settle();
  expect(container.querySelector('section[aria-label="Shortcut"]')).toBeNull();
  expect(container.querySelector('section[aria-label="Notifications"]')).toBeNull();
  expect(container.querySelector('section[aria-label="Voice"]')).toBeNull();
  expect(container.querySelector('section[aria-label="Machines"]')).not.toBeNull();
  const names = api.calls.map((c) => c[0]);
  expect(names).not.toContain("getHotkey");
  expect(names).not.toContain("notificationAuthorization");
});
