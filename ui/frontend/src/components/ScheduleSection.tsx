import type { Schedule } from "../types/plugin";

/**
 * スケジュールの `next`(ISO8601 のローカル時刻文字列)を `HH:MM` 表示にする。
 * パースできない値が来た場合は元の文字列をそのまま表示する(壊れた表示より
 * ましなフォールバック)。
 */
function formatNext(next: string): string {
  const date = new Date(next);
  if (Number.isNaN(date.getTime())) {
    return next;
  }
  const hh = date.getHours().toString().padStart(2, "0");
  const mm = date.getMinutes().toString().padStart(2, "0");
  return `${hh}:${mm}`;
}

/**
 * プラグインカードに表示する読み取り専用のスケジュール一覧
 * (`BusSection`/`DashboardSection` と違い、承認トグルなどの操作は無い --
 * `[[schedule]]` は承認対象の capability ではないため)。
 * 宣言が無いプラグインでは何も描画しない。
 */
export function ScheduleSection({ schedules }: { schedules: Schedule[] }) {
  if (schedules.length === 0) {
    return null;
  }

  return (
    <div className="schedule-section">
      <h3>Schedules</h3>
      <ul>
        {schedules.map((schedule) => (
          <li key={schedule.name}>
            {schedule.name} — {schedule.spec} (next {formatNext(schedule.next)})
          </li>
        ))}
      </ul>
    </div>
  );
}

export default ScheduleSection;
