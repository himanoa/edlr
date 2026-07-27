import { useState } from "react";
import type { BusRequest } from "../types/plugin";

function BusEntryCard({
  pluginId,
  entry,
  onSetGrant,
}: {
  pluginId: string;
  entry: BusRequest;
  onSetGrant: (pluginId: string, driver: string, granted: boolean) => Promise<void>;
}) {
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleGrant = async (next: boolean) => {
    setSaving(true);
    setError(null);
    try {
      await onSetGrant(pluginId, entry.driver, next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <fieldset className="bus-card">
      <legend>{entry.driver}</legend>
      <p className="bus-reason">{entry.reason}</p>

      {!entry.resolved && <span className="badge badge-bus-unresolved">未解決</span>}
      {entry.staleGrant && <span className="badge badge-bus-stale">要再承認</span>}

      {entry.publish.length > 0 && (
        <p className="bus-publish">配信するトピック: {entry.publish.join(", ")}</p>
      )}
      {entry.subscribe.length > 0 && (
        <p className="bus-subscribe">購読するトピック: {entry.subscribe.join(", ")}</p>
      )}

      <label className="form-row bus-grant-toggle">
        <span>このバス接続を承認する</span>
        {/*
          `FilesystemSection` / `SidecarSection` と同じ規律: `checked` は
          サーバから返った `entry.granted` のみで駆動する(楽観的更新をしない)。
          未解決(`resolved === false`)のときは承認しても意味がないため
          トグルを無効化する。
        */}
        <input
          type="checkbox"
          aria-label="このバス接続を承認する"
          checked={entry.granted}
          disabled={!entry.resolved || saving}
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
          未承認 — このプラグインはこのドライバとメッセージをやり取りできません
        </p>
      )}

      {error && <p className="form-error">{error}</p>}
    </fieldset>
  );
}

export function BusSection({
  pluginId,
  bus,
  onSetGrant,
}: {
  pluginId: string;
  bus: BusRequest[];
  onSetGrant: (pluginId: string, driver: string, granted: boolean) => Promise<void>;
}) {
  if (bus.length === 0) {
    return null;
  }

  return (
    <div className="bus-section">
      {bus.map((entry) => (
        <BusEntryCard
          key={entry.driver}
          pluginId={pluginId}
          entry={entry}
          onSetGrant={onSetGrant}
        />
      ))}
    </div>
  );
}

export default BusSection;
