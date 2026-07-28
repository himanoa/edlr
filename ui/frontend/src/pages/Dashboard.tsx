import { useEffect, useRef, useState } from "react";
import WidgetFrame from "../components/WidgetFrame";
import { RpcClient } from "../rpc";
import { defaultWsUrl, useEventStream } from "../ws";
import type { DashboardListEntry, WidgetSize } from "../types/plugin";

const SPAN: Record<WidgetSize, number> = { small: 1, medium: 2, large: 3 };

/**
 * プラグイン拡張可能なダッシュボード。
 *
 * `dashboard/list` で grant 済みウィジェットを取得し、CSS Grid に自動配置
 * する(small=1 / medium=2 / large=3 カラムスパン、並びは登録順)。
 * イベントは親がここで一本の WS(`useEventStream`)から受け、各
 * `WidgetFrame` がプラグインの events フィルタに従って iframe へ転送する。
 */
export default function Dashboard() {
  const [widgets, setWidgets] = useState<DashboardListEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { entries } = useEventStream(defaultWsUrl());
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    const client = new RpcClient(defaultWsUrl());
    client
      .listDashboard()
      .then((res) => {
        if (mountedRef.current) setWidgets(res.widgets);
      })
      .catch((err) => {
        if (mountedRef.current) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      mountedRef.current = false;
      client.close();
    };
  }, []);

  return (
    <section>
      <h1>Dashboard</h1>
      {error && <p className="form-error">ウィジェット一覧の取得に失敗しました: {error}</p>}
      {widgets && widgets.length === 0 && (
        <p className="note">
          承認済みのウィジェットがありません。Plugins 画面でウィジェットを承認すると、ここに表示されます。
        </p>
      )}
      <div className="widget-grid">
        {widgets?.map((w) => (
          <article
            key={`${w.plugin}/${w.widget}`}
            className="widget-card"
            style={{ gridColumn: `span ${SPAN[w.size]}` }}
          >
            <h2>{w.title}</h2>
            {w.state !== "running" ? (
              <p className="widget-placeholder">プラグインが停止しています</p>
            ) : !w.resolved ? (
              <p className="widget-placeholder">entry ファイルが見つかりません</p>
            ) : (
              <WidgetFrame entry={w} entries={entries} />
            )}
          </article>
        ))}
      </div>
    </section>
  );
}
