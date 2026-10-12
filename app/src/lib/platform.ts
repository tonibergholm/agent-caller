/**
 * The touch-first layout for the iOS shell. It is chosen at build time: the
 * `ios` Vite mode (`npm run build:ios`, `npm run dev:ios`) sets
 * `VITE_PLATFORM=ios` through `app/.env.ios`. The default build is macOS.
 *
 * Read on each call, not once at import, so tests can stub the variable.
 * The dev preview can force the layout with `?mobile` (see `preview.ts`).
 */
let previewOverride: boolean | null = null;

export function isMobile(): boolean {
  if (previewOverride !== null) return previewOverride;
  return import.meta.env.VITE_PLATFORM === "ios";
}

/** Dev preview only: `null` restores the build-time choice. */
export function setPreviewMobile(on: boolean | null): void {
  previewOverride = on;
}
