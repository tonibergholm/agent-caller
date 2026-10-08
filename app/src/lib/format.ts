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
