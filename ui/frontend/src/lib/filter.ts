export interface LogEntry {
  id: number;
  kind: "journal" | "status";
  timestamp?: string;
  event?: string;
  raw: unknown;
}

export function filterEntries(entries: LogEntry[], query: string): LogEntry[] {
  const q = query.trim().toLowerCase();
  if (!q) return entries;
  return entries.filter((e) => {
    const name = (e.event ?? e.kind).toLowerCase();
    if (name.includes(q)) return true;
    return JSON.stringify(e.raw).toLowerCase().includes(q);
  });
}
