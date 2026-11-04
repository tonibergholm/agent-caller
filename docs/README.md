# Documentation index

Start with the [roadmap](superpowers/plans/2026-10-07-roadmap.md) for status
and the [known gaps](superpowers/plans/2026-10-07-p1-followups.md) for open
work. The wire contract is
[`crates/acd-protocol/PROTOCOL.md`](../crates/acd-protocol/PROTOCOL.md). Specs
say what was decided. Plans are historical briefs: do not replay their task
lists. Research files are dated evidence.

## Status

- [Roadmap and status](superpowers/plans/2026-10-07-roadmap.md): what merged, what is next, what only the owner can do.
- [Known gaps](superpowers/plans/2026-10-07-p1-followups.md): open items by area, owner decisions, 1.0 triage.
- [Threat model and reporting](../SECURITY.md).

## Specs (`superpowers/specs/`)

- [Agent inbox v1](superpowers/specs/2026-10-07-agent-inbox-design.md): the design and review decisions.
- [Voice control](superpowers/specs/2026-10-11-voice-design.md): spoken commands on macOS.
- [Omarchy inbox](superpowers/specs/2026-10-12-omarchy-inbox-design.md): `acd-inbox` and the shell plugin.
- [External source](superpowers/specs/2026-10-13-external-source-design.md): local programs publish cards.
- [Relay and push](superpowers/specs/2026-11-03-relay-push-design.md): wake-up relay design, reviewed, not built on main.

## Plans (`superpowers/plans/`, historical)

- [P1 protocol and core](superpowers/plans/2026-10-07-p1-protocol-core.md)
- [P2 herdr adapter](superpowers/plans/2026-10-08-p2-herdr-adapter.md)
- [P3 hooks](superpowers/plans/2026-10-09-p3-hooks.md)
- [P4 T3 adapter](superpowers/plans/2026-10-08-p4-t3-adapter.md)
- [P5 Mac app](superpowers/plans/2026-10-08-p5-mac-app.md)
- [P5 answer revisions](superpowers/plans/2026-10-08-p5-answer-revision.md)
- [Cloud session batch](superpowers/plans/2026-10-11-cloud-batch.md)
- [Voice step 1](superpowers/plans/2026-10-11-voice-step1.md)
- [Omarchy inbox](superpowers/plans/2026-10-12-omarchy-inbox.md)
- [Voice step 2](superpowers/plans/2026-10-12-voice-step2.md)
- [Voice steps 3 and 4](superpowers/plans/2026-10-13-voice-step3-4.md)

## Research (`research/`, dated evidence)

Live checks:

- [P0 checks](research/2026-10-07-p0-checks.md)
- [herdr gate 6](research/2026-10-08-herdr-gate6.md)
- [T3 gate 3](research/2026-10-08-t3-gate3.md)
- [Claude Code gates 2, 5, 7](research/2026-10-09-p3-gates-claude.md)
- [Codex gate 4](research/2026-10-09-p3-gate4-codex.md)
- [P3 live checks 1 to 14](research/2026-10-09-p3-live.md)
- [End-to-end run](research/2026-10-11-e2e-run.md)
- [iOS spike](research/2026-10-12-ios-spike.md)
- [Omarchy plugin API and live check](research/2026-10-12-omarchy-plugin-api.md)
- [Voice step 2 spike](research/2026-10-12-voice-step2-spike.md)
- [Voice model contract](research/2026-10-13-voice-model-contract.md): includes the 16-item live check list, not yet run.
- [Voice model quality](research/2026-10-13-voice-model-quality.md)

Reviews and feasibility:

- [Landscape](research/2026-10-07-landscape.md)
- [Voice backend B (Linux, Windows)](research/2026-10-12-voice-backend-b.md)
- [Security review: protocol, core, client](research/2026-11-03-security-review-core.md)
- [Security review: local attack surface](research/2026-11-03-security-review-local.md)
- [Windows and Android feasibility](research/2026-11-03-windows-android-feasibility.md)

## Release and platforms

- [Releases](release.md): cutting a release, secrets, what the binaries mean.
- [Windows](windows.md): client groundwork, type-checked only, with the owner's checklist.
- [Screenshots](images/README.md): how the README images are made.
- [Obsidian Cockpit](design/obsidian-cockpit.md): the visual language.
- [Archive](archive/): the original roadmap and the P1 review ledger.
