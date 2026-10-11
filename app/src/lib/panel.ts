import type { CardView } from "./bindings/CardView";
import type { MachineStatus } from "./bindings/MachineStatus";
import type { MachineView } from "./bindings/MachineView";

/** Short relative age: "just now", "12s ago", "3m ago", "2h ago", "4d ago". Null when unparsable. */
export function age(iso: string | number | null | undefined, now: number): string | null {
  if (iso === null || iso === undefined || iso === "") return null;
  const ms = typeof iso === "number" ? iso : Date.parse(iso);
  if (Number.isNaN(ms)) return null;
  const s = Math.max(0, Math.floor((now - ms) / 1000));
  if (s < 5) return "just now";
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

// A Claude/Codex tool name ("Bash", "Edit", "mcp__srv__tool"), as opposed to a
// sentence such as "Waiting for input in the terminal".
const TOOL_NAME = /^[A-Za-z][A-Za-z0-9_.:-]{0,39}$/;

/** The tool name a permission request carries as its title, if it is one. */
export function toolName(card: CardView): string | null {
  const p = card.agent.pending;
  if (!p || p.kind !== "permission") return null;
  return TOOL_NAME.test(p.title) ? p.title : null;
}

/** The type tag of a needs-you card, e.g. "BASH APPROVAL" or "QUESTION". */
export function typeTag(card: CardView): string {
  const p = card.agent.pending;
  if (!p) return "NEEDS YOU";
  switch (p.kind) {
    case "permission": {
      const tool = toolName(card);
      return tool ? `${tool.toUpperCase()} APPROVAL` : "APPROVAL";
    }
    case "question":
      return "QUESTION";
    case "blocked_unparsed":
      return "BLOCKED";
    default:
      return "REQUEST";
  }
}

export type Sections = { needs: CardView[]; working: CardView[]; done: CardView[] };

/**
 * The three panel sections, in on-screen order. Idle and done agents share
 * "Done". A card with a countdown stays in view whatever the machine filter
 * says: filtering is a view change, it must not hide a pending send.
 */
export function sections(cards: CardView[], machine: string | null): Sections {
  const shown =
    machine === null ? cards : cards.filter((c) => c.machine_key === machine || c.action !== null);
  const needs = shown.filter((c) => c.group === "request");
  const working = shown.filter((c) => c.group === "working" || c.group === "unknown");
  const done = shown.filter((c) => c.group === "done" || c.group === "idle");
  return { needs, working, done };
}

export type DotKind = "online" | "syncing" | "offline" | "error";

/** Machine status as one of the four dots the top bar can show. */
export function dotKind(s: MachineStatus): DotKind {
  switch (s.kind) {
    case "online":
      return "online";
    case "connecting":
    case "syncing":
      return "syncing";
    case "offline":
      return "offline";
    default:
      return "error";
  }
}

/** Machines to list: duplicates of another entry are suppressed. */
export const listedMachines = (machines: MachineView[]): MachineView[] =>
  machines.filter((m) => m.duplicate_of === null);
