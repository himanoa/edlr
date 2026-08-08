import { useEffect, useRef } from "react";
import type { SettingField } from "@/types/plugin";

/**
 * `options-from` の候補が未着(`options === null`)の select がある間だけ、
 * refetch を定期実行して一覧を取り直す。サーバは retained topic の到着を
 * push しないため、届くまでポーリングするしかない。候補が揃えば止まる。
 */
export function useOptionsPolling(
  settings: SettingField[],
  refetch: () => Promise<void>,
  intervalMs = 3000,
) {
  const unresolved = settings.some((f) => f.type === "select" && f.options === null);
  const refetchRef = useRef(refetch);
  refetchRef.current = refetch;

  useEffect(() => {
    if (!unresolved) return;
    const id = setInterval(() => void refetchRef.current().catch(() => {}), intervalMs);
    return () => clearInterval(id);
  }, [unresolved, intervalMs]);
}
