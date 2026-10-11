---
name: Obsidian Cockpit
colors:
  surface: '#0f131c'
  surface-dim: '#0f131c'
  surface-bright: '#353943'
  surface-container-lowest: '#0a0e17'
  surface-container-low: '#181b25'
  surface-container: '#1c1f29'
  surface-container-high: '#262a34'
  surface-container-highest: '#31353f'
  on-surface: '#dfe2ef'
  on-surface-variant: '#bcc9cd'
  inverse-surface: '#dfe2ef'
  inverse-on-surface: '#2c303a'
  outline: '#869397'
  outline-variant: '#3d494c'
  surface-tint: '#4cd7f6'
  primary: '#4cd7f6'
  on-primary: '#003640'
  primary-container: '#06b6d4'
  on-primary-container: '#00424f'
  inverse-primary: '#00687a'
  secondary: '#b7c8e1'
  on-secondary: '#213145'
  secondary-container: '#3a4a5f'
  on-secondary-container: '#a9bad3'
  tertiary: '#4edea3'
  on-tertiary: '#003824'
  tertiary-container: '#1bbd85'
  on-tertiary-container: '#00452e'
  error: '#ffb4ab'
  on-error: '#690005'
  error-container: '#93000a'
  on-error-container: '#ffdad6'
  primary-fixed: '#acedff'
  primary-fixed-dim: '#4cd7f6'
  on-primary-fixed: '#001f26'
  on-primary-fixed-variant: '#004e5c'
  secondary-fixed: '#d3e4fe'
  secondary-fixed-dim: '#b7c8e1'
  on-secondary-fixed: '#0b1c30'
  on-secondary-fixed-variant: '#38485d'
  tertiary-fixed: '#6ffbbe'
  tertiary-fixed-dim: '#4edea3'
  on-tertiary-fixed: '#002113'
  on-tertiary-fixed-variant: '#005236'
  background: '#0f131c'
  on-background: '#dfe2ef'
  surface-variant: '#31353f'
typography:
  display-lg:
    fontFamily: Geist
    fontSize: 1.75rem
    fontWeight: '600'
    lineHeight: 2.25rem
    letterSpacing: -0.03em
  headline-md:
    fontFamily: Geist
    fontSize: 1.25rem
    fontWeight: '600'
    lineHeight: 1.75rem
    letterSpacing: -0.025em
  headline-sm:
    fontFamily: Geist
    fontSize: 1rem
    fontWeight: '600'
    lineHeight: 1.5rem
    letterSpacing: -0.02em
  body-lg:
    fontFamily: Geist
    fontSize: 0.9375rem
    fontWeight: '400'
    lineHeight: 1.45rem
    letterSpacing: -0.015em
  body-md:
    fontFamily: Geist
    fontSize: 0.8125rem
    fontWeight: '400'
    lineHeight: 1.25rem
    letterSpacing: -0.01em
  body-sm:
    fontFamily: Geist
    fontSize: 0.75rem
    fontWeight: '400'
    lineHeight: 1.1rem
    letterSpacing: -0.005em
  code-md:
    fontFamily: JetBrains Mono
    fontSize: 0.8125rem
    fontWeight: '400'
    lineHeight: 1.3rem
    letterSpacing: 0em
  code-sm:
    fontFamily: JetBrains Mono
    fontSize: 0.6875rem
    fontWeight: '500'
    lineHeight: 1rem
    letterSpacing: 0.02em
  label-md:
    fontFamily: Geist
    fontSize: 0.6875rem
    fontWeight: '600'
    lineHeight: 0.95rem
    letterSpacing: 0.04em
  label-sm:
    fontFamily: Geist
    fontSize: 0.625rem
    fontWeight: '700'
    lineHeight: 0.85rem
    letterSpacing: 0.06em
rounded:
  sm: 0.25rem
  DEFAULT: 0.5rem
  md: 0.75rem
  lg: 1rem
  xl: 1.5rem
  full: 9999px
spacing:
  gutter: 0.75rem
  margin: 1rem
  space-2xs: 0.125rem
  space-xs: 0.25rem
  space-sm: 0.5rem
  space-md: 0.75rem
  space-lg: 1rem
  space-xl: 1.5rem
---

## Brand & Style

The design system establishes a hyper-focused, precision-engineered developer environment built for orchestration across distributed machines. It bridges high-density terminal efficiency with modern operating system vibrancy, evoking instantaneous feedback, total machine visibility, and low-latency control.

The aesthetic fuses deep slate and obsidian surfaces with translucent optical glassmorphism and surgical typography. Surfaces do not feel like passive containers; they behave like calibrated instruments—machined, silent, and luminous. Motion is brief and functional, micro-interactions honor muscle memory, and high-contrast telemetry signals state without visual clutter.

## Colors

The palette operates on an absolute dark plane, grounding the interface in deep obsidian (`#05080F`) and structured slate tones. Color is strictly functional, reserved for runtime telemetry, active focus vectors, and tactical operational alerts.

- **Primary (`#06B6D4` / Cyan):** Signals active orchestration, executing processes, and primary keyboard focus indicators.
- **Secondary (`#64748B` / Cool Slate):** Provides structural boundaries, inactive state markers, metadata chips, and non-blocking telemetry.
- **Tertiary (`#10B981` / Emerald):** Indicates idle runtimes, verified test passes, and clean daemon shutdowns.
- **Neutral (`#090D16` / Obsidian Slate):** Serves as the canvas core, delivering ambient depth without washed-out charcoal tones.
- **Semantic Signals:**
  - *Needs Attention (Amber):* `#F59E0B`
  - *Blocked / Error (Crimson):* `#EF4444`
  - *Tailscale Peer Synced (Indigo):* `#6366F1`

## Typography

The typography uses Geist for shell navigation, agent status inspection, and high-density meta grids, matched with JetBrains Mono for output streams, process IDs, and command lines.

All tracking is tightened slightly at medium and large scales to maintain dense, mechanical balance. Monospaced tokens feature optical alignment for tabular numerals, ensuring dynamic memory values, token consumption counts, and ping latencies update without horizontal jitter.

## Layout & Spacing

Layout geometry follows an adaptable 3-pane split: Machine Tree / Node Rail (collapsible, 220px–280px), Agent Process Canvas (adaptive multi-column or tiled workspace), and Contextual Terminal Telemetry Inspector (360px–480px).

Gaps conform strictly to an 8px base rhythm with 2px and 4px micro-offsets (`space-2xs`, `space-xs`) for command chips, key combos, and status indicators. Internal panel margins contract automatically in dense multi-session tiling mode to preserve maximum terminal buffer width.

## Elevation & Depth

Visual depth is achieved through layered glassmorphism, background blurs, and translucent edge lines rather than diffuse drop shadows.

- **Base Layer (Canvas):** Pure Obsidian (`#05080F`), non-reflective, anchors the window frame.
- **Mid Layer (Panels & Panes):** Slate-tinted translucent fill (`rgba(15, 23, 42, 0.72)`) with a `backdrop-filter: blur(16px)` and a hairline border (`rgba(255, 255, 255, 0.08)`).
- **Floating Overlays (HUD Voice Commander, Command Palette):** High-density dark glass (`rgba(9, 13, 22, 0.88)`), `backdrop-filter: blur(24px)`, coupled with a direct inner hairline bevel (`inset 0 1px 0 rgba(255, 255, 255, 0.12)`) and a focused cyan boundary ring (`0 0 0 1px rgba(6, 182, 212, 0.4)`).
- **Active Focus Elevation:** Inactive panes rest at flat contrast; an active terminal or agent card receives an ambient directional glow using `0 0 20px -4px rgba(6, 182, 212, 0.15)`.

## Shapes

The interface balances sharp industrial hardware profiles with native platform familiarity. Outer application panels, floating sheets, and HUD overlays apply standard curvature (`0.5rem` / `roundedness: 2`), while command palette inputs, terminal viewports, and nested output rows step down to micro-curves (`0.25rem`) for compact spatial discipline.

## Components

### Buttons & Interactive Triggers
- **Primary Action (Execute/Run):** Solid cyan background (`#06B6D4`), dark slate text (`#05080F`), font weight `600`, micro-bevel highlight on top edge.
- **Secondary Action (Kill/Fork/Tail):** Surface container fill (`rgba(255, 255, 255, 0.04)`), crisp 1px stroke (`rgba(255, 255, 255, 0.09)`), hover transition to `rgba(255, 255, 255, 0.08)`.
- **Keyboard Shortcut Affordances:** Nested within button triggers using `JetBrains Mono`, tracking subtle opacity (`rgba(255, 255, 255, 0.5)`), wrapped in 1px micro-borders.

### Status Badges & Agent Health
- **Architecture:** Compact 18px pills with an inline 6px glowing indicator dot.
- **Working (Cyan):** `#06B6D4` dot with subtle pulse animation, dark translucent cyan pill background (`rgba(6, 182, 212, 0.1)`).
- **Needs Attention (Amber):** `#F59E0B` dot, warning fill (`rgba(245, 158, 11, 0.12)`), text rendered in amber-300.
- **Idle / Done (Emerald):** `#10B981` dot, stable fill (`rgba(16, 185, 129, 0.1)`).
- **Blocked / Error (Crimson):** `#EF4444` dot, error fill (`rgba(239, 68, 68, 0.14)`).

### HUD Voice Commander & Command Palette
- **Placement:** Centered floating modal, top 18% offset.
- **Voice Commander HUD:** Displays a dynamic cyan-to-indigo frequency waveform canvas flanked by speech-to-text live stream preview and matched target agents (`Claude Code @ tailscale-workstation`).
- **Undo Progress Timer:** Appears pinned to lower edge after stateful commands. Linear 2000ms countdown progress bar styled in solid `#06B6D4` against an obsidian substrate, cancellable via `Escape` or `Cmd+Z`.

### Terminal Preview Panes
- **Surface:** Obsidian black (`#020408`) with 96% opacity to catch subtle desktop backdrop vibrancy.
- **Header Strip:** Monospace host identity badge (e.g., `remote::100.84.12.3:herdr`), latency gauge, token rate meter, and quick-detach icon buttons.
- **Buffer:** JetBrains Mono font, 13px baseline, syntax highlighting optimized for code diffs and continuous agent reasoning logs.