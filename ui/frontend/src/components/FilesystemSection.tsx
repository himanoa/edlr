import { useEffect, useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { invoke, isTauri } from "../lib/tauri";
import type { FilesystemConfig, FilesystemRoot } from "../types/plugin";
import { Checkbox } from "@/components/ui/checkbox";

const WARNING = "mt-1.5 text-sm font-semibold text-yellow-400";

function FilesystemRootCard({
  root,
  onConfigChange,
  onGrantChange,
}: {
  root: FilesystemRoot;
  onConfigChange: (name: string, config: FilesystemConfig) => Promise<void>;
  onGrantChange: (name: string, granted: boolean) => Promise<void>;
}) {
  const [path, setPath] = useState(root.config.path);
  const [saving, setSaving] = useState(false);
  const [grantSaving, setGrantSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // `SidecarSection` と同じ理由: サーバの `root.config` が(別クライアント
  // 経由などで)変わったらフォーム state を追随させ、ユーザーの未保存入力を
  // 不必要には巻き戻さない。
  useEffect(() => {
    setPath(root.config.path);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [root.config.path]);

  const handlePick = async () => {
    setError(null);
    try {
      const picked = await invoke<string | null>("pick_directory");
      if (picked === null) return;
      setPath(picked);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    try {
      await onConfigChange(root.name, { path });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleGrant = async (next: boolean) => {
    setGrantSaving(true);
    setError(null);
    try {
      await onGrantChange(root.name, next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setGrantSaving(false);
    }
  };

  return (
    <fieldset className="mb-3 rounded-md border border-border px-4 py-3">
      <legend className="px-1.5 font-semibold text-sky-400">{root.name}</legend>
      <p className="mb-2 text-sm text-muted-foreground">{root.reason}</p>
      <Badge className="bg-accent text-sky-400">
        {root.mode === "read-write" ? "読み書き" : "読み取りのみ"}
      </Badge>

      <label className="flex items-center justify-between gap-4 py-1.5">
        <span>フォルダ</span>
        <input
          aria-label="フォルダ"
          type="text"
          className="w-72 rounded border border-border bg-background px-3 py-1.5 text-foreground disabled:opacity-50"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          disabled={saving}
        />
      </label>
      {isTauri() && (
        <Button type="button" variant="secondary" size="sm" onClick={handlePick} disabled={saving}>
          選択…
        </Button>
      )}

      <Button type="button" variant="secondary" size="sm" onClick={handleSave} disabled={saving}>
        保存
      </Button>

      <label className="mt-2 flex items-center justify-between gap-4 py-1.5">
        <span>このフォルダへのアクセスを承認する</span>
        {/*
          `SidecarSection` と同じ規律: `checked` はサーバから返った
          `root.granted` のみで駆動する(楽観的更新をしない)。RPC が返らない
          まま「承認済み」に見えるのを防ぐため。
        */}
        <Checkbox
          aria-label="このフォルダへのアクセスを承認する"
          checked={root.granted}
          // `checked` と同じ理由で、`disabled` もローカルの未保存入力
          // (`path` state)ではなくサーバが確認済みの `root.config.path` で
          // 判定する。保存前の入力だけでトグルが有効になると、承認した対象
          // (サーバ側のパス)と実際にアクセスされる場所がずれてしまう。
          disabled={root.config.path === "" || grantSaving}
          onCheckedChange={(v) => handleGrant(v === true)}
        />
        {grantSaving && (
          <span className="ml-1.5 text-xs text-muted-foreground" role="status">
            確認中…
          </span>
        )}
      </label>

      {root.mode === "read-write" ? (
        <p className={WARNING}>
          承認すると、このプラグインは選んだフォルダ内のファイルを読み取り・作成・上書き・削除できます
        </p>
      ) : (
        <p className={WARNING}>
          承認すると、このプラグインは選んだフォルダ内のファイルを読み取れます
        </p>
      )}

      {!root.granted && (
        <p className="mt-1.5 text-sm text-yellow-400">
          未承認 — このプラグインはファイルにアクセスできません
        </p>
      )}
      {root.staleGrant && <p className={WARNING}>要求が変わったため再承認が必要です</p>}

      {error && <p className="mt-1.5 text-sm text-red-400">{error}</p>}
    </fieldset>
  );
}

export default function FilesystemSection({
  roots,
  onConfigChange,
  onGrantChange,
}: {
  roots: FilesystemRoot[];
  onConfigChange: (name: string, config: FilesystemConfig) => Promise<void>;
  onGrantChange: (name: string, granted: boolean) => Promise<void>;
}) {
  if (roots.length === 0) {
    return null;
  }

  return (
    <div className="mt-3 border-t border-border pt-3">
      {roots.map((r) => (
        <FilesystemRootCard
          key={r.name}
          root={r}
          onConfigChange={onConfigChange}
          onGrantChange={onGrantChange}
        />
      ))}
    </div>
  );
}
