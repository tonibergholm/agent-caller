import { expect, test } from "vitest";
import { privacyPane } from "./hud";

test("only permission refusals point to a System Settings pane", () => {
  expect(privacyPane("microphone_denied")).toBe("microphone");
  expect(privacyPane("speech_denied")).toBe("speech");
  expect(privacyPane("hold_key_off")).toBe("input_monitoring");
  expect(privacyPane("microphone_unavailable")).toBeNull();
  expect(privacyPane("snapshot_changed")).toBeNull();
});
