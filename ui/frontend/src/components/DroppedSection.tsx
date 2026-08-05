import type { DroppedCounts } from "../types/plugin";

/**
 * 作業キュー満杯で捨てられたイベント / バス配信の件数。
 *
 * **何も捨てていないときは何も描画しない**: 常時 "0 件" を出しても情報量が
 * 無く、他のセクションと並ぶとノイズになる。ここが出ている = 実際に取り
 * こぼしが起きている、という警告として読めるようにする。
 *
 * どちらの経路も再送されない(journal の読み取り位置は配送の成否と独立に
 * 進むため replay でも戻らず、バス配信にも再送は無い)ので、この件数は
 * そのまま失われたイベント数を意味する。
 */
export function DroppedSection({ dropped }: { dropped: DroppedCounts }) {
  const items: string[] = [];
  if (dropped.events > 0) {
    items.push(`${dropped.events} journal events`);
  }
  if (dropped.busDeliveries > 0) {
    items.push(`${dropped.busDeliveries} bus deliveries`);
  }
  if (items.length === 0) {
    return null;
  }

  return (
    <div className="mt-3 border-t border-border pt-3">
      <h3 className="m-0 mb-2 text-base font-semibold">Dropped</h3>
      <p className="m-0 text-sm text-yellow-400">
        {items.join(", ")} dropped since the daemon started — the work queue was full.
        These are not replayed.
      </p>
    </div>
  );
}

export default DroppedSection;
