import { cleanup, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test } from "vitest";
import Panel from "./Panel.svelte";
import { fakeApi, view } from "../lib/fixtures";

afterEach(cleanup);

test("macOS: the gear still asks the app to open its Settings window", async () => {
  const api = fakeApi();
  render(Panel, { view: view(), api });
  await userEvent.click(screen.getByRole("button", { name: "Settings" }));
  expect(api.calls.map((c) => c[0])).toContain("openSettings");
  expect(screen.queryByRole("heading", { name: "Settings" })).toBeNull();
});
