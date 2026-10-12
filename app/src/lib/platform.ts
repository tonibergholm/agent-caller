/**
 * The touch-first layout for the iOS shell. It is chosen by the Vite mode
 * only: `npm run build:ios` and `npm run dev:ios` run `--mode ios`. The
 * default mode is macOS. A `VITE_PLATFORM` variable in the shell is not read,
 * so a stray environment setting cannot flip the macOS build.
 *
 * Read on each call, not once at import, so tests can stub the mode.
 * The dev preview can force the layout with `?mobile` (see `preview.ts`).
 */
let previewOverride: boolean | null = null;

export function isMobile(): boolean {
  if (previewOverride !== null) return previewOverride;
  return import.meta.env.MODE === "ios";
}

/** Dev preview only: `null` restores the build-time choice. */
export function setPreviewMobile(on: boolean | null): void {
  previewOverride = on;
}
