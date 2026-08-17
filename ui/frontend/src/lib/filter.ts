export interface LogEntry {
  id: number;
  kind: "journal" | "status" | "log" | "bus";
  timestamp?: string;
  event?: string;
  /** kind === "log" のみ: info | warn | error */
  level?: string;
  /** kind === "log" のみ: 整形済みログ本文(key=value フィールド込み) */
  message?: string;
  /** kind === "log" のみ: 発生元モジュールパス */
  target?: string;
  /** kind === "bus" のみ: 送信元ドライバ id / トピック名 / UTF-8 payload */
  driver?: string;
  topic?: string;
  payload?: string;
  raw: unknown;
}

export function filterEntries(entries: LogEntry[], query: string): LogEntry[] {
  const q = query.trim().toLowerCase();
  if (!q) return entries;
  return entries.filter((e) => {
    const name = (e.event ?? e.kind).toLowerCase();
    if (name.includes(q)) return true;
    if (e.kind === "log" && e.message?.toLowerCase().includes(q)) return true;
    return JSON.stringify(e.raw).toLowerCase().includes(q);
  });
}
