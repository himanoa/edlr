import { useState } from "react";
import { Badge } from "@/components/ui/badge";
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
    <fieldset className="mb-3 rounded-md border border-border px-4 py-3">
      <legend className="px-1.5 font-semibold text-sky-400">{entry.driver}</legend>
      <p className="mb-2 text-sm text-muted-foreground">{entry.reason}</p>

      {!entry.resolved && <Badge className="bg-red-950 text-red-400">未解決</Badge>}
      {entry.staleGrant && <Badge className="bg-yellow-950 text-yellow-400">要再承認</Badge>}

      {entry.publish.length > 0 && (
        <p className="text-sm text-muted-foreground">配信するトピック: {entry.publish.join(", ")}</p>
      )}
      {entry.subscribe.length > 0 && (
        <p className="text-sm text-muted-foreground">購読するトピック: {entry.subscribe.join(", ")}</p>
      )}

      <label className="mt-2 flex items-center justify-between gap-4 py-1.5">
        <span>このバス接続を承認する</span>
        {/*
          `FilesystemSection` / `SidecarSection` と同じ規律: `checked` は
          サーバから返った `entry.granted` のみで駆動する(楽観的更新をしない)。
          未解決(`resolved === false`)のときは承認 ON にしても意味がないため
          トグルを無効化する -- ただし無効化するのは ON にする方向だけ。
          `resolved` はドライバ全体・トピック全体が揃っているかの all-or-
          nothing 判定だが、実際の enforcement はトピック単位で行われるため、
          既に承認済みの接続はドライバ更新で 1 トピックだけ欠けても他の
          トピックへの publish/subscribe が生き続ける。ここで取消(OFF)まで
          無効化すると、そのユーザーには「未解決」バッジは見えるのに取消の
          手段が無くなってしまう(Important: 最終レビューで見つかった
          取りこぼし)。取消は常に可能にする。
        */}
        <input
          type="checkbox"
          aria-label="このバス接続を承認する"
          checked={entry.granted}
          disabled={saving || (!entry.granted && !entry.resolved)}
          onChange={(e) => handleGrant(e.target.checked)}
        />
        {saving && (
          <span className="ml-1.5 text-xs text-muted-foreground" role="status">
            確認中…
          </span>
        )}
      </label>

      {!entry.granted && (
        <p className="mt-1.5 text-sm text-yellow-400">
          未承認 — このプラグインはこのドライバとメッセージをやり取りできません
        </p>
      )}

      {error && <p className="mt-1.5 text-sm text-red-400">{error}</p>}
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
    <div className="mt-3 border-t border-border pt-3">
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
