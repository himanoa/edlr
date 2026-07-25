// プラグインマニフェストのシリアライズ形(サーバの `plugins/list` レスポンス)に一致させる型。
export type SettingField =
  | { type: "boolean"; key: string; label: string; default: boolean }
  | { type: "string"; key: string; label: string; default: string }
  | { type: "number"; key: string; label: string; default: number }
  | { type: "select"; key: string; label: string; default: string; options: string[] };

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  state: "running" | "disabled";
  reason?: string;
  settings: SettingField[];
  values: Record<string, unknown>;
}

export interface PluginsList {
  pluginsDir: string;
  plugins: PluginInfo[];
}
