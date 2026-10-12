# README screenshots

`inbox.png` and `settings.png` show the current Svelte components with
fixture data from `app/src/preview.ts`. They do not show live agent data,
the native menu bar or macOS window chrome.

To refresh them:

1. Run `npm ci` and `npm run dev` in `app/`. Use the URL printed by Vite
   (the configured port is 1420).
2. Open `/preview.html?scene=main` at 680 × 1120 CSS pixels for the inbox,
   and `/preview.html?scene=settings` at 680 × 760 for settings.
3. Keep sample approval rules consistent with the sample command. The inbox
   capture uses an exact `Bash(cargo test --workspace --locked && cargo
   clippy --workspace -- -D warnings)` rule in place of the generic fixture
   rule; only sample data was adjusted for capture.
4. Wait for bundled fonts to load, capture the viewport, and save the PNGs
   here. Check that text, controls and footer are legible before committing.

Other preview scenes include `subagent`, `undo`, `empty` and `offline`.
The preview is a development entry point, excluded from production.
