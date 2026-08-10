import type { WidgetSize } from "../types/plugin";

/** react-grid-layout の LayoutItem と構造互換(必要なキーだけ)。 */
export interface LayoutItem {
  i: string;
  x: number;
  y: number;
  w: number;
  h: number;
}

const STORAGE_KEY = "edlr.dashboardLayout";
/** manifest size → 初期カラム幅(グリッドは 6 カラム)。 */
export const SIZE_COLS: Record<WidgetSize, number> = { small: 2, medium: 4, large: 6 };
export const GRID_COLS = 6;
/** rowHeight 80px × 3 + マージンで旧 iframe 既定 240px 相当。 */
export const DEFAULT_H = 3;

/**
 * 保存済みレイアウトと現在のウィジェット一覧の突き合わせ。
 * 既知は保存位置を維持、新顔は最下段に manifest size 幅で追加、
 * 消えたウィジェットの保存分は捨てる。
 */
export function mergeLayout(
  saved: LayoutItem[],
  widgets: { key: string; size: WidgetSize }[],
): LayoutItem[] {
  const byKey = new Map(saved.map((it) => [it.i, it]));
  const bottom = saved
    .filter((it) => widgets.some((w) => w.key === it.i))
    .reduce((max, it) => Math.max(max, it.y + it.h), 0);
  return widgets
    .map(
      (w, idx) =>
        byKey.get(w.key) ?? {
          i: w.key,
          x: 0,
          // 新顔同士の正確な詰めは react-grid-layout の compact に任せる
          y: bottom + idx,
          w: SIZE_COLS[w.size],
          h: DEFAULT_H,
        },
    )
    .map(({ i, x, y, w, h }) => ({ i, x, y, w, h }));
}

export function loadLayout(storage: Pick<Storage, "getItem"> = localStorage): LayoutItem[] {
  try {
    const parsed = JSON.parse(storage.getItem(STORAGE_KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

export function saveLayout(
  items: readonly LayoutItem[],
  storage: Pick<Storage, "setItem"> = localStorage,
): void {
  storage.setItem(
    STORAGE_KEY,
    JSON.stringify(items.map(({ i, x, y, w, h }) => ({ i, x, y, w, h }))),
  );
}
