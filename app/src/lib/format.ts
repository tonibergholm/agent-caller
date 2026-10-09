import type { MachineStatus } from "./bindings/MachineStatus";

/** Relative time for "offline since": just now, N min ago, N h ago, then a date. */
export function since(ms: number, now: number): string {
  const diff = Math.max(0, now - ms);
  const minutes = Math.floor(diff / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  return new Date(ms).toLocaleString();
}

/** The status kind, e.g. "local unavailable"; no reason. */
export const statusKind = (s: MachineStatus) => s.kind.replace(/_/g, " ");

/** The reason a status carries (message or error), or null. */
export const statusReason = (s: MachineStatus): string | null =>
  "message" in s ? s.message : "error" in s ? s.error : null;

/** Full status text: `kind`, or `kind: reason` when the status carries one. */
export function statusText(s: MachineStatus | undefined): string {
  if (!s) return "";
  const reason = statusReason(s);
  return reason === null ? statusKind(s) : `${statusKind(s)}: ${reason}`;
}
