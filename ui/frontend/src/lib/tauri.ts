import { invoke as tauriInvoke } from "@tauri-apps/api/core";

/** Tauri の webview 内かどうか。ブラウザと vitest/jsdom では false になる。 */
export function isTauri(): boolean {
  return typeof globalThis !== "undefined" && "__TAURI_INTERNALS__" in globalThis;
}

export type AppConfigDto = {
  /**
   * 実効値。EDLR_JOURNAL_DIR が設定されていれば設定ファイルより優先される。
   * 表示・ルーティング判定(未設定なら Settings タブへ)にのみ使う。
   * 編集フォームの初期値には使わない — envOverride 中はこれが env 由来の
   * 値になり、設定ファイルの値と一致しないことがある。
   */
  journalDir: string | null;
  /**
   * 設定ファイルに実際に保存されている生の値。envOverride の有無に関わらず
   * 常に設定ファイルの内容そのもの。編集フォームの初期値・保存対象はこちら。
   */
  configuredJournalDir: string | null;
  /** false なら外部起動のデーモン。設定は保存できるが再起動はされない。 */
  daemonManaged: boolean;
  configError: string | null;
  /** デーモンが起動していない理由(起動時の spawn 失敗)。動いていれば null。 */
  daemonError: string | null;
  /** EDLR_JOURNAL_DIR が journalDir を上書きしているか。 */
  envOverride: boolean;
};

/** Tauri コマンドを呼ぶ。非 Tauri 環境では呼び出し自体が誤りなので明示的に失敗させる。 */
export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) {
    throw new Error("Tauri 環境ではありません");
  }
  return tauriInvoke<T>(cmd, args);
}
