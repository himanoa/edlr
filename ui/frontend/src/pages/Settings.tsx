import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { invoke, isTauri, type AppConfigDto } from "../lib/tauri";

type Status = "loading" | "ready" | "unavailable";

const NOTE = "text-sm text-muted-foreground";
const FORM_ERROR = "mt-1.5 text-sm text-red-400";

export default function Settings() {
  // Plugins.tsx と同じく、await をまたぐ setState をアンマウント後に撃たないよう守る
  const mountedRef = useRef(true);
  const [status, setStatus] = useState<Status>("loading");
  const [config, setConfig] = useState<AppConfigDto | null>(null);
  const [draft, setDraft] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    mountedRef.current = true;

    if (!isTauri()) {
      setStatus("unavailable");
      return () => {
        mountedRef.current = false;
      };
    }

    invoke<AppConfigDto>("get_config")
      .then((res) => {
        if (!mountedRef.current) return;
        setConfig(res);
        // 編集フォームの初期値は設定ファイルの生の値から取る(実効値
        // journalDir ではない)。envOverride 中に journalDir を使うと、
        // 編集せずに保存しただけで env 由来の値を設定ファイルへ
        // 書き戻してしまい、保存済みの値を消してしまう。
        setDraft(res.configuredJournalDir ?? "");
        setStatus("ready");
      })
      .catch((err) => {
        if (!mountedRef.current) return;
        setError(err instanceof Error ? err.message : String(err));
        setStatus("ready");
      });

    return () => {
      mountedRef.current = false;
    };
  }, []);

  const handlePick = async () => {
    setError(null);
    setNotice(null);
    try {
      const picked = await invoke<string | null>("pick_journal_dir");
      if (!mountedRef.current || picked === null) return;
      setDraft(picked);
    } catch (err) {
      if (!mountedRef.current) return;
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleClear = async () => {
    setSaving(true);
    setError(null);
    setNotice(null);
    try {
      const updated = await invoke<AppConfigDto>("clear_journal_dir");
      if (!mountedRef.current) return;
      setConfig(updated);
      setDraft("");
      setNotice(
        updated.envOverride
          ? `設定ファイルの値を消しました。ただし EDLR_JOURNAL_DIR が優先されるため、デーモンは引き続き環境変数の値(${updated.journalDir})を使用します。`
          : "自動検出に戻しました。",
      );
    } catch (err) {
      if (!mountedRef.current) return;
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (mountedRef.current) setSaving(false);
    }
  };

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    setNotice(null);
    try {
      const updated = await invoke<AppConfigDto>("set_journal_dir", { path: draft });
      if (!mountedRef.current) return;
      setConfig(updated);
      setNotice(
        updated.daemonManaged
          ? "保存しました。デーモンを再起動しました。"
          : "保存しました。外部で起動中のデーモンには反映されていません。手動で再起動してください。",
      );
    } catch (err) {
      if (!mountedRef.current) return;
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (mountedRef.current) setSaving(false);
    }
  };

  return (
    <section className="max-w-2xl space-y-3">

      {status === "loading" && <p className={NOTE}>読み込み中…</p>}

      {status === "unavailable" && (
        <p className={NOTE}>
          設定はデスクトップアプリから変更してください。ブラウザからは変更できません。
        </p>
      )}

      {status === "ready" && (
        <>
          {config?.configError && (
            <p className={FORM_ERROR}>
              設定ファイルを読み込めませんでした: {config.configError}
              <br />
              保存すると新しい内容で上書きされます。
            </p>
          )}

          {config?.daemonError && (
            <p className={FORM_ERROR}>
              デーモンを起動できませんでした: {config.daemonError}
              <br />
              保存すると再度起動を試みます。
            </p>
          )}

          {config?.envOverride && (
            <p className={NOTE}>
              環境変数 EDLR_JOURNAL_DIR が設定されているため、デーモンは現在
              {" "}
              {config.journalDir} を使用しています。ここで編集・保存できるのは設定ファイルの値
              (下の入力欄の初期値)で、環境変数が解除されるまでは保存しても実際の反映先は
              変わりません。
            </p>
          )}

          <div className="space-y-1.5">
            <label htmlFor="journal-dir" className="block text-sm font-medium">
              Journal ディレクトリ
            </label>
            <Input
              id="journal-dir"
              type="text"
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              disabled={saving}
            />
          </div>
          <div className="flex flex-wrap gap-2">
            <Button type="button" variant="secondary" onClick={handlePick} disabled={saving}>
              選択…
            </Button>
            <Button type="button" onClick={handleSave} disabled={saving || draft === ""}>
              保存
            </Button>
            {/* config が null(get_config 失敗)のときは保存済みの値が
                あるか分からないので出さない。`!== null` だと undefined を
                拾ってしまうため `!= null` で両方を弾く。 */}
            {config?.configuredJournalDir != null && (
              <Button type="button" variant="secondary" onClick={handleClear} disabled={saving}>
                自動検出に戻す
              </Button>
            )}
            {/* 起動に失敗している間は、保存(draft 必須)もクリア(設定値必須)も
                押せない状況があり得る。責任は持ち続けている以上、再試行の経路を
                必ず一つ残す。実体は「現在の実効値で spawn し直す」なので
                clear_journal_dir と同じ操作。 */}
            {config?.daemonError && config.configuredJournalDir == null && (
              <Button type="button" variant="secondary" onClick={handleClear} disabled={saving}>
                デーモンの起動を再試行
              </Button>
            )}
          </div>

          <p className={NOTE}>
            未設定の場合は Proton の既定パスを自動検出します。自動検出が当たらない環境
            (セカンダリ Steam ライブラリなど)では、ここで明示的に指定してください。
          </p>

          {error && <p className={FORM_ERROR}>{error}</p>}
          {notice && <p className={NOTE}>{notice}</p>}
        </>
      )}
    </section>
  );
}
