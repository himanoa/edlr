import { useEffect, useRef, useState } from "react";
import { invoke, isTauri, type AppConfigDto } from "../lib/tauri";

type Status = "loading" | "ready" | "unavailable";

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
    <section>
      <h1>Settings</h1>

      {status === "loading" && <p className="note">読み込み中…</p>}

      {status === "unavailable" && (
        <p className="note">
          設定はデスクトップアプリから変更してください。ブラウザからは変更できません。
        </p>
      )}

      {status === "ready" && (
        <>
          {config?.configError && (
            <p className="form-error">
              設定ファイルを読み込めませんでした: {config.configError}
              <br />
              保存すると新しい内容で上書きされます。
            </p>
          )}

          {config?.envOverride && (
            <p className="note">
              環境変数 EDLR_JOURNAL_DIR が設定されているため、デーモンは現在
              {" "}
              {config.journalDir} を使用しています。ここで編集・保存できるのは設定ファイルの値
              (下の入力欄の初期値)で、環境変数が解除されるまでは保存しても実際の反映先は
              変わりません。
            </p>
          )}

          <label htmlFor="journal-dir">Journal ディレクトリ</label>
          <input
            id="journal-dir"
            type="text"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            disabled={saving}
          />
          <button type="button" onClick={handlePick} disabled={saving}>
            選択…
          </button>
          <button type="button" onClick={handleSave} disabled={saving || draft === ""}>
            保存
          </button>

          <p className="note">
            未設定の場合は Proton の既定パスを自動検出します。自動検出が当たらない環境
            (セカンダリ Steam ライブラリなど)では、ここで明示的に指定してください。
          </p>

          {error && <p className="form-error">{error}</p>}
          {notice && <p className="note">{notice}</p>}
        </>
      )}
    </section>
  );
}
