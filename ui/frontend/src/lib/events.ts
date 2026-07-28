import type { LogEntry } from "./filter";

/**
 * manifest の events フィルタが entry にマッチするか。
 * core/src/plugin/manifest.rs の matches_event と同一規則:
 * - "*" は全 journal イベント(status には false)
 * - "status" は status イベントのみ
 * - それ以外は journal イベント名の完全一致
 * - 空リストは常に false
 */
export function matchesEvent(events: string[], entry: LogEntry): boolean {
  if (entry.kind === "journal") {
    return events.some((e) => e === "*" || e === entry.event);
  }
  return events.includes("status");
}
