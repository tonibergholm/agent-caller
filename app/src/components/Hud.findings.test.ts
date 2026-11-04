// Demonstrations for docs/research/2026-11-04-security-review-apps-voice.md.
// Each test asserts the safe behaviour and is skipped because it fails today.
// Remove `.skip` to reproduce.
import { cleanup, fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import { afterEach, expect, test } from "vitest";
import Hud from "./Hud.svelte";
import { fakeApi, view } from "../lib/fixtures";
import type { ProposalView, TargetView } from "../lib/hud";

afterEach(cleanup);

const target: TargetView = {
  card: "m/a", machine: "devbox", agent: "Claude", title: "Fix the build",
  request: "bash approval", off_screen: false, disambiguator: null,
};
const proposal: ProposalView = {
  action: "answer", target, option: "Yes, always", effect: "allow_always", rule: "Bash(*)",
  question: "npm test", text: null, provenance: "selected", level: "confirm",
};

// Finding V3: a held Enter (auto-repeat) confirms the step the moment the HUD takes key focus.
// The inbox panel refuses a repeated key for a `confirm` option (lib/keys.ts); the HUD does not.
test.skip("an auto-repeating Enter must not confirm a voice step", async () => {
  const api = fakeApi();
  render(Hud, { api, view: view() });
  await tick();
  api.hud!({ seq: 1, id: 3, hud: { state: "confirm", proposal, proposal_id: 3 } });
  await tick();
  await tick();
  await fireEvent.keyDown(window, { key: "Enter", repeat: true });
  expect(api.calls.filter((c) => c[0] === "voiceConfirm")).toEqual([]);
});
