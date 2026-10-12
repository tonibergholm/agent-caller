import { afterEach, expect, test, vi } from "vitest";
import { isMobile, setPreviewMobile } from "./platform";

afterEach(() => {
  setPreviewMobile(null);
  vi.unstubAllEnvs();
});

test("the default build is macOS, not mobile", () => {
  expect(isMobile()).toBe(false);
});

test("a stray VITE_PLATFORM=ios in the shell does not select the mobile layout", () => {
  vi.stubEnv("VITE_PLATFORM", "ios");
  expect(isMobile()).toBe(false);
});

test("the ios Vite mode selects the mobile layout, read at call time", () => {
  vi.stubEnv("MODE", "ios");
  expect(isMobile()).toBe(true);
  vi.stubEnv("MODE", "production");
  expect(isMobile()).toBe(false);
});

test("the preview override forces the layout and null restores the build choice", () => {
  setPreviewMobile(true);
  expect(isMobile()).toBe(true);
  setPreviewMobile(null);
  expect(isMobile()).toBe(false);
});
