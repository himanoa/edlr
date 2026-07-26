import { invoke as tauriInvoke } from "@tauri-apps/api/core";

/** Tauri の webview 内かどうか。ブラウザと vitest/jsdom では false になる。 */
export function isTauri(): boolean {
  return typeof globalThis !== "undefined" && "__TAURI_INTERNALS__" in globalThis;
}

export type AppConfigDto = {
  journalDir: string | null;
  /** false なら外部起動のデーモン。設定は保存できるが再起動はされない。 */
  daemonManaged: boolean;
  configError: string | null;
};

/** Tauri コマンドを呼ぶ。非 Tauri 環境では呼び出し自体が誤りなので明示的に失敗させる。 */
export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) {
    throw new Error("Tauri 環境ではありません");
  }
  return tauriInvoke<T>(cmd, args);
}
