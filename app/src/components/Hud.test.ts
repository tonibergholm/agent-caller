import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { flushSync, tick } from "svelte";
import { afterEach, expect, test, vi } from "vitest";
import Hud from "./Hud.svelte";
import { fakeApi, view } from "../lib/fixtures";
import type { InboxView } from "../lib/bindings/InboxView";
import type { Hud as HudState, ProposalView, TargetView } from "../lib/hud";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const target: TargetView = {
  card: "minos/a",
  machine: "minos",
  agent: "Claude",
  title: "Fix the build",
  request: "bash approval",
  off_screen: false,
  disambiguator: null,
};

const proposal = (o: Partial<ProposalView> = {}): ProposalView => ({
  action: "answer",
  target,
  option: "Yes, always",
  effect: "allow_always",
  rule: "Bash(npm test:*)",
  question: "npm test",
  text: null,
  provenance: "selected",
  level: "confirm",
  ...o,
});

let seq = 0;

const inputId = (s: HudState): number | null =>
  s.state === "confirm" || s.state === "countdown"
    ? s.proposal_id
    : s.state === "thinking" || s.state === "listening"
      ? s.id
      : s.state === "which_one"
        ? s.choice_id
        : null;

async function show(state: HudState, v: InboxView = view()) {
  const api = fakeApi();
  render(Hud, { api, view: v });
  await tick();
  api.hud!({ seq: ++seq, id: inputId(state), hud: state });
  await tick();
  await tick();
  return api;
}

const others = (calls: unknown[][]) => calls.filter((c) => c[0] !== "voiceShown");

test("hidden shows nothing", () => {
  const { container } = render(Hud, { api: fakeApi(), view: view() });
  expect(container.querySelector(".hud")).toBeNull();
});

test("a rendered proposal is acknowledged with its id; other states are not", async () => {
  let api = await show({ state: "confirm", proposal: proposal(), proposal_id: 7 });
  expect(api.calls).toContainEqual(["voiceShown", 7]);
  cleanup();
  api = await show({ state: "countdown", proposal: proposal({ level: "countdown" }), proposal_id: 8, action_id: null });
  expect(api.calls).toContainEqual(["voiceShown", 8]);
  expect(screen.getByText(/Starting/)).toBeTruthy();
  cleanup();
  api = await show({ state: "countdown", proposal: proposal({ level: "countdown" }), proposal_id: 8, action_id: "act-1" });
  expect(api.calls.some((c) => c[0] === "voiceShown")).toBe(false);
});

test("a proposal replaced before the DOM flush is never acknowledged", async () => {
  const api = fakeApi();
  render(Hud, { api, view: view() });
  await tick();
  // Two events with no flush between them: only the second renders.
  api.hud!({ seq: ++seq, id: 20, hud: { state: "confirm", proposal: proposal(), proposal_id: 20 } });
  api.hud!({ seq: ++seq, id: 21, hud: { state: "confirm", proposal: proposal({ option: "Yes" }), proposal_id: 21 } });
  await tick();
  await tick();
  expect(api.calls.filter((c) => c[0] === "voiceShown")).toEqual([["voiceShown", 21]]);
  api.hud!({ seq: ++seq, id: 22, hud: { state: "countdown", proposal: proposal({ level: "countdown" }), proposal_id: 22, action_id: null } });
  api.hud!({ seq: ++seq, id: null, hud: { state: "speaking", text: "Nothing needs you." } });
  await tick();
  await tick();
  expect(api.calls.filter((c) => c[0] === "voiceShown")).toEqual([["voiceShown", 21]]);
});

test("an older state arriving late is dropped", async () => {
  const api = fakeApi();
  render(Hud, { api, view: view() });
  await tick();
  api.hud!({ seq: 10, id: null, hud: { state: "speaking", text: "newer" } });
  api.hud!({ seq: 9, id: null, hud: { state: "speaking", text: "older" } });
  await tick();
  expect(screen.getByText("newer")).toBeTruthy();
  expect(screen.queryByText("older")).toBeNull();
  api.hudClear!(9);
  await tick();
  expect(screen.getByText("newer")).toBeTruthy();
  api.hudClear!(10);
  await tick();
  expect(screen.queryByText("newer")).toBeNull();
});

test("confirm shows target, action and the whole payload; Enter confirms its id, Esc cancels", async () => {
  const api = await show({ state: "confirm", proposal: proposal(), proposal_id: 3 });
  expect(screen.getByText("Fix the build")).toBeTruthy();
  expect(screen.getByText("@ minos")).toBeTruthy();
  expect(screen.getByText("Bash(npm test:*)")).toBeTruthy();
  expect(screen.getByText("npm test")).toBeTruthy();
  await userEvent.keyboard("{Enter}");
  await userEvent.keyboard("{Escape}");
  expect(others(api.calls)).toEqual([["voiceConfirm", 3], ["voiceCancel", 3]]);
});

test("confirm and cancel have buttons too", async () => {
  const api = await show({ state: "confirm", proposal: proposal(), proposal_id: 4 });
  await userEvent.click(screen.getByRole("button", { name: /Confirm/ }));
  await userEvent.click(screen.getByRole("button", { name: /Cancel/ }));
  expect(others(api.calls)).toEqual([["voiceConfirm", 4], ["voiceCancel", 4]]);
});

test("a long payload scrolls inside its block and the buttons stay outside it", async () => {
  const text = `${"run the tests and then report back\n".repeat(400)}END`;
  await show({
    state: "confirm",
    proposal: proposal({ action: "send", option: null, effect: null, rule: null, question: null, text }),
    proposal_id: 5,
  });
  const payload = document.querySelector<HTMLElement>(".payload")!;
  expect(payload.textContent).toBe(text);
  expect(payload.classList.contains("block")).toBe(true);
  const body = document.querySelector<HTMLElement>(".body")!;
  for (const name of [/Confirm/, /Cancel/]) {
    expect(body.contains(screen.getByRole("button", { name }))).toBe(false);
  }
});

test("a target off screen is marked", async () => {
  await show({ state: "confirm", proposal: proposal({ target: { ...target, off_screen: true } }), proposal_id: 6 });
  expect(screen.getByText("@ minos").classList.contains("off-screen")).toBe(true);
});

test("look-alike targets show their disambiguator and full names", async () => {
  const long = `${"m".repeat(150)}-one`;
  await show({
    state: "which_one",
    transcript: "approve",
    choice_id: 41,
    candidates: [
      { ...target, machine: long, disambiguator: "api" },
      { ...target, card: "minos/b", disambiguator: "web" },
    ],
  });
  expect(screen.getByText(`@ ${long}`)).toBeTruthy();
  expect(screen.getByRole("button", { name: /api/ })).toBeTruthy();
  expect(screen.getByRole("button", { name: /web/ })).toBeTruthy();
  cleanup();
  await show({ state: "confirm", proposal: proposal({ target: { ...target, disambiguator: "minos/a" } }), proposal_id: 42 });
  expect(document.querySelector(".target .which")?.textContent).toBe("minos/a");
});

test("which-one lists numbered candidates; digits and clicks choose", async () => {
  const api = await show({
    state: "which_one",
    transcript: "approve",
    choice_id: 40,
    candidates: [target, { ...target, card: "minos/b", title: "Write docs" }],
  });
  await userEvent.keyboard("2");
  await userEvent.keyboard("9");
  await userEvent.click(screen.getByRole("button", { name: /Fix the build/ }));
  expect(api.calls).toEqual([["voiceChoose", 40, 1], ["voiceChoose", 40, 0]]);
});

test("the countdown reads the inbox action and cancels on Esc, not Enter", async () => {
  const v = view({ actions: [{ id: "act-1", card_key: "minos/a", label: "Yes", detail: null, fires_at_ms: Date.now() + 1500 }] });
  const api = await show(
    { state: "countdown", proposal_id: 9, action_id: "act-1", proposal: proposal({ level: "countdown", option: "Yes", effect: "allow", rule: null }) },
    v,
  );
  expect(screen.getByText(/Sending in 1\.\ds/)).toBeTruthy();
  await userEvent.keyboard("{Enter}");
  await userEvent.keyboard("{Escape}");
  expect(api.calls).toEqual([["voiceCancel", 9]]);
});

test("Esc or Cancel while thinking cancels that utterance; Enter and digits do nothing", async () => {
  const api = await show({ state: "thinking", transcript: "approve", id: 30 });
  await userEvent.keyboard("{Enter}1{Escape}");
  expect(api.calls).toEqual([["voiceCancel", 30]]);
  await userEvent.click(screen.getByRole("button", { name: /Cancel/ }));
  expect(api.calls).toEqual([["voiceCancel", 30], ["voiceCancel", 30]]);
});

test("states that do not wait for input ignore keys", async () => {
  const api = await show({ state: "speaking", text: "2 need you." });
  await userEvent.keyboard("{Escape}{Enter}1");
  expect(api.calls).toEqual([]);
});

test("after firing it says it is waiting for the result", async () => {
  await show({ state: "sending", proposal: proposal(), too_late_to_cancel: false });
  expect(screen.getByText("Sent. Waiting for the result.")).toBeTruthy();
});

test("errors are classified; only 'didn't understand' repeats the transcript", async () => {
  await show({ state: "error", refusal: "did_not_understand", transcript: "frobnicate the thing" });
  expect(screen.getByText("Didn't understand.")).toBeTruthy();
  expect(screen.getByText(/frobnicate the thing/)).toBeTruthy();
  cleanup();
  await show({ state: "error", refusal: "display_failed", transcript: null });
  expect(screen.getByText("The HUD could not show the step. Nothing was sent.")).toBeTruthy();
});

test("outcomes and spoken text are shown", async () => {
  await show({ state: "done", proposal: null, outcome: { kind: "not_sent", reason: "answered_elsewhere" } });
  expect(screen.getByText("Answered elsewhere.")).toBeTruthy();
  cleanup();
  await show({ state: "speaking", text: "2 need you. On minos: Claude, Fix: bash approval." });
  expect(screen.getByText(/2 need you/)).toBeTruthy();
});

test("voice text never reaches the console", async () => {
  const spies = (["log", "info", "warn", "error", "debug"] as const).map((m) => vi.spyOn(console, m));
  const api = await show({ state: "thinking", transcript: "SECRET-T", id: 31 });
  api.voiceConfirm = async () => {
    throw new Error("SECRET-E");
  };
  api.voiceShown = async () => {
    throw new Error("SECRET-S");
  };
  api.hud!({ seq: ++seq, id: 11, hud: { state: "confirm", proposal: proposal({ action: "send", text: "SECRET-P" }), proposal_id: 11 } });
  await tick();
  await userEvent.keyboard("{Enter}");
  await tick();
  for (const spy of spies) {
    expect(spy).not.toHaveBeenCalled();
  }
});

test("Esc on a state without an input id sends nothing, even when a stale id was shown before", async () => {
  const api = await show({ state: "confirm", proposal: proposal(), proposal_id: 60 });
  api.hud!({ seq: ++seq, id: null, hud: { state: "sending", proposal: proposal(), too_late_to_cancel: false } });
  await tick();
  await userEvent.keyboard("{Escape}");
  expect(others(api.calls)).toEqual([]);
});

test("Enter on a focused Cancel cancels and never confirms", async () => {
  const api = await show({ state: "confirm", proposal: proposal(), proposal_id: 61 });
  screen.getByRole("button", { name: /Cancel/ }).focus();
  await userEvent.keyboard("{Enter}");
  expect(others(api.calls)).toEqual([["voiceCancel", 61]]);
});

test("which-one: Esc and Cancel send the choice id; Enter on a focused candidate picks it", async () => {
  const api = await show({ state: "which_one", transcript: "x", choice_id: 62, candidates: [target, { ...target, card: "minos/b", title: "Write docs" }] });
  await userEvent.keyboard("{Escape}");
  await userEvent.click(screen.getByRole("button", { name: /Cancel/ }));
  screen.getByRole("button", { name: /Write docs/ }).focus();
  await userEvent.keyboard("{Enter}");
  expect(api.calls).toEqual([["voiceCancel", 62], ["voiceCancel", 62], ["voiceChoose", 62, 1]]);
});

test("a countdown waiting for its ack has no bar; a missing action reads Sending…", async () => {
  await show({ state: "countdown", proposal: proposal({ level: "countdown" }), proposal_id: 63, action_id: null });
  expect(document.querySelector(".track")).toBeNull();
  cleanup();
  await show({ state: "countdown", proposal: proposal({ level: "countdown" }), proposal_id: 64, action_id: "gone" });
  expect(screen.getByText(/Sending…/)).toBeTruthy();
  expect(screen.queryByText(/Sending in/)).toBeNull();
});

test("the proposal is in the DOM when the acknowledgement fires", async () => {
  const api = fakeApi();
  let seen: boolean | null = null;
  api.voiceShown = async () => {
    seen = document.querySelector(".hud") !== null && document.body.textContent!.includes("Fix the build");
  };
  render(Hud, { api, view: view() });
  await tick();
  api.hud!({ seq: ++seq, id: 65, hud: { state: "confirm", proposal: proposal(), proposal_id: 65 } });
  await tick();
  await tick();
  expect(seen).toBe(true);
});

test("a stale clear is ignored and the current clear hides the state", async () => {
  const api = fakeApi();
  render(Hud, { api, view: view() });
  await tick();
  api.hud!({ seq: 50, id: null, hud: { state: "speaking", text: "kept" } });
  await tick();
  api.hudClear!(49);
  api.hudClear!(51);
  await tick();
  expect(screen.getByText("kept")).toBeTruthy();
  api.hudClear!(50);
  await tick();
  expect(screen.queryByText("kept")).toBeNull();
});

test("each state asks the panel to fit its content", async () => {
  const api = await show({ state: "error", refusal: "did_not_understand", transcript: "x" });
  await tick();
  await tick();
  expect(api.fits.length).toBeGreaterThan(0);
  expect(api.calls).toEqual([]);
});

test("a fit is sent only for the state still on screen", async () => {
  const api = await show({ state: "error", refusal: "did_not_understand", transcript: "x" });
  await tick();
  await tick();
  const first = api.fits.at(-1)!;
  api.fits.length = 0;
  // A newer state arrives after the older one rendered, before its
  // measurement ran: the older measurement is dropped.
  api.hud!({ seq: first + 1, id: null, hud: { state: "error", refusal: "did_not_understand", transcript: "y" } });
  flushSync();
  api.hud!({ seq: first + 2, id: null, hud: { state: "speaking", text: "z" } });
  flushSync();
  await tick();
  await tick();
  expect(api.fits).toEqual([first + 2]);
});

test("listening shows the microphone, then the live text", async () => {
  const api = await show({ state: "listening", id: 4, partial: null, over: null });
  expect(screen.getByText("Listening…")).toBeTruthy();
  expect(document.querySelector(".mic")).not.toBeNull();
  api.hud!({ seq: ++seq, id: 4, hud: { state: "listening", id: 4, partial: "approve the", over: null } });
  await tick();
  expect(screen.getByLabelText("Heard so far").textContent).toContain("approve the");
  expect(api.calls.some((c) => c[0] === "voiceShown")).toBe(false);
});

/** S25: after a click on the HUD, Esc cancels the recording; Listening itself never asks for key. */
test("Esc in the HUD cancels the recording by its id", async () => {
  const api = await show({ state: "listening", id: 4, partial: null, over: null });
  await userEvent.keyboard("{Escape}");
  expect(others(api.calls)).toEqual([["voiceCancel", 4]]);
});

test("the HUD's Cancel stops the recording by its id", async () => {
  const api = await show({ state: "listening", id: 4, partial: null, over: null });
  await userEvent.click(screen.getByRole("button", { name: /Cancel/ }));
  expect(others(api.calls)).toEqual([["voiceCancel", 4]]);
});

test("a confirm that waits stays visible and read-only while listening", async () => {
  const over: HudState = { state: "confirm", proposal: proposal(), proposal_id: 9 };
  const api = await show({ state: "listening", id: 10, partial: null, over });
  expect(screen.getByLabelText("Rule to remember").textContent).toBe("Bash(npm test:*)");
  expect(screen.queryByRole("button", { name: /Confirm/ })).toBeNull();
  await userEvent.keyboard("{Enter}");
  await userEvent.click(screen.getByRole("button", { name: /Cancel/ }));
  expect(others(api.calls)).toEqual([["voiceCancel", 10]]);
});

test("a which-one list under listening cannot be clicked", async () => {
  const over: HudState = { state: "which_one", candidates: [target], transcript: "approve", choice_id: 3 };
  const api = await show({ state: "listening", id: 11, partial: null, over });
  const button = screen.getByRole("button", { name: /Fix the build/ }) as HTMLButtonElement;
  expect(button.disabled).toBe(true);
  await userEvent.keyboard("1");
  expect(others(api.calls)).toEqual([]);
});

test("permission errors open their System Settings pane; others offer none", async () => {
  const api = await show({ state: "error", refusal: "microphone_denied", transcript: null });
  expect(screen.getByText("Microphone access is off for Agent Caller.")).toBeTruthy();
  await userEvent.click(screen.getByRole("button", { name: "Open System Settings" }));
  expect(api.calls).toContainEqual(["openVoicePrivacy", "microphone"]);
  cleanup();
  await show({ state: "error", refusal: "recording_too_long", transcript: null });
  expect(screen.getByText("Recording stopped after 60 seconds. Nothing was sent.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Open System Settings" })).toBeNull();
});

test("an unavailable model says why", async () => {
  await show({ state: "error", refusal: "model_off", transcript: null });
  expect(screen.getByText("Apple Intelligence is off. Simple commands still work.")).toBeTruthy();
  cleanup();
  await show({ state: "error", refusal: "model_not_ready", transcript: null });
  expect(screen.getByText("The on-device model is not ready yet. Simple commands still work.")).toBeTruthy();
});
