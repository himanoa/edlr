import { useEffect, useState } from "react";
import type { Schedule } from "../types/plugin";

/** 表示を追従させる再描画間隔。秒単位の相対表記なので 1 秒で十分。 */
const TICK_MS = 1000;

/**
 * `next` までの残り時間を相対表記にする(`in 42s` / `in 3m 05s` / `in 2h 07m`)。
 * 既に過ぎている場合は `due`(次の RPC で値が更新されるまでの過渡状態)。
 */
function formatRelative(deltaMs: number): string {
  if (deltaMs <= 0) {
    return "due";
  }
  const totalSeconds = Math.round(deltaMs / 1000);
  if (totalSeconds < 60) {
    return `in ${totalSeconds}s`;
  }
  const minutes = Math.floor(totalSeconds / 60);
  if (minutes < 60) {
    const seconds = totalSeconds % 60;
    return `in ${minutes}m ${seconds.toString().padStart(2, "0")}s`;
  }
  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    return `in ${hours}h ${(minutes % 60).toString().padStart(2, "0")}m`;
  }
  return `in ${Math.floor(hours / 24)}d ${(hours % 24).toString().padStart(2, "0")}h`;
}

/** `2026-07-29 09:00`。日付をまたぐ発火を `HH:MM` と区別するために使う。 */
function formatAbsolute(date: Date): string {
  const yyyy = date.getFullYear();
  const mo = (date.getMonth() + 1).toString().padStart(2, "0");
  const dd = date.getDate().toString().padStart(2, "0");
  const hh = date.getHours().toString().padStart(2, "0");
  const mi = date.getMinutes().toString().padStart(2, "0");
  return `${yyyy}-${mo}-${dd} ${hh}:${mi}`;
}

/**
 * スケジュールの `next`(ISO8601 のローカル時刻文字列)を表示用にする。
 *
 * 素の `HH:MM` だと `cron = "0 9 * * *"` が明日発火するのか5分後なのか
 * 区別できないため、常に相対表記を主にし、当日でない発火には絶対時刻を
 * 併記する。パースできない値が来た場合は元の文字列をそのまま表示する
 * (壊れた表示よりましなフォールバック)。
 */
export function formatNext(next: string, now: Date): string {
  const date = new Date(next);
  if (Number.isNaN(date.getTime())) {
    return next;
  }
  const relative = formatRelative(date.getTime() - now.getTime());
  const sameDay =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();
  return sameDay ? relative : `${relative} (${formatAbsolute(date)})`;
}

/**
 * プラグインカードに表示する読み取り専用のスケジュール一覧
 * (`BusSection`/`DashboardSection` と違い、承認トグルなどの操作は無い --
 * `[[schedule]]` は承認対象の capability ではないため)。
 * 宣言が無いプラグインでは何も描画しない。
 */
export function ScheduleSection({ schedules }: { schedules: Schedule[] }) {
  const [now, setNow] = useState(() => new Date());
  const hasSchedules = schedules.length > 0;

  // 相対表記はレンダー時のスナップショットなので、ティックが無いと即座に
  // 陳腐化する。宣言が無いプラグインではタイマーを回さない。
  useEffect(() => {
    if (!hasSchedules) {
      return;
    }
    const timer = setInterval(() => setNow(new Date()), TICK_MS);
    return () => clearInterval(timer);
  }, [hasSchedules]);

  if (!hasSchedules) {
    return null;
  }

  return (
    <div className="mt-3 border-t border-border pt-3">
      <h3 className="m-0 mb-2 text-base font-semibold">Schedules</h3>
      <ul className="m-0 list-none p-0 font-mono text-[0.85rem] text-muted-foreground">
        {schedules.map((schedule) => (
          <li key={schedule.name}>
            {schedule.name} — {schedule.spec} (next {formatNext(schedule.next, now)})
          </li>
        ))}
      </ul>
    </div>
  );
}

export default ScheduleSection;
