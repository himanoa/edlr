import { useState } from "react";
import type { DashboardWidget } from "../types/plugin";

function DashboardEntryCard({
  pluginId,
  entry,
  onSetGrant,
}: {
  pluginId: string;
  entry: DashboardWidget;
  onSetGrant: (pluginId: string, widget: string, granted: boolean) => Promise<void>;
}) {
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleGrant = async (next: boolean) => {
    setSaving(true);
    setError(null);
    try {
      await onSetGrant(pluginId, entry.id, next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <fieldset className="bus-card">
      <legend>{entry.title}</legend>
      <p className="bus-reason">
        ダッシュボードウィジェット({entry.size}) — {entry.entry}
      </p>

      {!entry.resolved && <span className="badge badge-bus-unresolved">未解決</span>}
      {entry.staleGrant && <span className="badge badge-bus-stale">要再承認</span>}

      <label className="form-row bus-grant-toggle">
        <span>このウィジェットの表示を承認する</span>
        {/*
          `BusSection` と同じ規律: `checked` はサーバから返った
          `entry.granted` のみで駆動する(楽観的更新をしない)。未解決
          (entry ファイル不在)のときは承認 ON にしても表示できないため
          無効化する -- ただし無効化するのは ON にする方向だけで、取消は
          常に可能にする(未解決でも承認済みなら取り消せる)。
        */}
        <input
          type="checkbox"
          aria-label="このウィジェットの表示を承認する"
          checked={entry.granted}
          disabled={saving || (!entry.granted && !entry.resolved)}
          onChange={(e) => handleGrant(e.target.checked)}
        />
        {saving && (
          <span className="capability-pending" role="status">
            確認中…
          </span>
        )}
      </label>

      {!entry.granted && (
        <p className="capability-notice">
          未承認 — このウィジェットは Dashboard に表示されません
        </p>
      )}

      {error && <p className="form-error">{error}</p>}
    </fieldset>
  );
}

export function DashboardSection({
  pluginId,
  dashboard,
  onSetGrant,
}: {
  pluginId: string;
  dashboard: DashboardWidget[];
  onSetGrant: (pluginId: string, widget: string, granted: boolean) => Promise<void>;
}) {
  if (dashboard.length === 0) {
    return null;
  }

  return (
    <div className="dashboard-section">
      {dashboard.map((entry) => (
        <DashboardEntryCard
          key={entry.id}
          pluginId={pluginId}
          entry={entry}
          onSetGrant={onSetGrant}
        />
      ))}
    </div>
  );
}

export default DashboardSection;
