import { useState } from "react";
import { Badge } from "@/components/ui/badge";
import type { DashboardWidget } from "../types/plugin";
import { Checkbox } from "@/components/ui/checkbox";

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
    <fieldset className="mb-3 rounded-md border border-border px-4 py-3">
      <legend className="px-1.5 font-semibold text-sky-400">{entry.title}</legend>
      <p className="mb-2 text-sm text-muted-foreground">
        ダッシュボードウィジェット({entry.size}) — {entry.entry}
      </p>

      {!entry.resolved && <Badge className="bg-red-950 text-red-400">未解決</Badge>}
      {entry.staleGrant && <Badge className="bg-yellow-950 text-yellow-400">要再承認</Badge>}

      <label className="mt-2 flex items-center justify-between gap-4 py-1.5">
        <span>このウィジェットの表示を承認する</span>
        {/*
          `BusSection` と同じ規律: `checked` はサーバから返った
          `entry.granted` のみで駆動する(楽観的更新をしない)。未解決
          (entry ファイル不在)のときは承認 ON にしても表示できないため
          無効化する -- ただし無効化するのは ON にする方向だけで、取消は
          常に可能にする(未解決でも承認済みなら取り消せる)。
        */}
        <Checkbox
          aria-label="このウィジェットの表示を承認する"
          checked={entry.granted}
          disabled={saving || (!entry.granted && !entry.resolved)}
          onCheckedChange={(v) => handleGrant(v === true)}
        />
        {saving && (
          <span className="ml-1.5 text-xs text-muted-foreground" role="status">
            確認中…
          </span>
        )}
      </label>

      {!entry.granted && (
        <p className="mt-1.5 text-sm text-yellow-400">
          未承認 — このウィジェットは Dashboard に表示されません
        </p>
      )}

      {error && <p className="mt-1.5 text-sm text-red-400">{error}</p>}
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
    <div className="mt-3 border-t border-border pt-3">
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
