// プラグイン基盤(wasmtime + マニフェスト)実装までのモックデータ。
// この型が将来のマニフェストスキーマの叩き台になる。
export type SettingField =
  | { key: string; label: string; type: "boolean"; default: boolean }
  | { key: string; label: string; type: "string"; default: string }
  | { key: string; label: string; type: "number"; default: number }
  | { key: string; label: string; type: "select"; options: string[]; default: string };

export interface PluginManifest {
  id: string;
  name: string;
  description: string;
  settings: SettingField[];
}

export const mockPlugins: PluginManifest[] = [
  {
    id: "voice-notify",
    name: "Voice Notify",
    description: "ジャンプ・ドッキング等のイベントを音声で通知する(モック)",
    settings: [
      { key: "enabled", label: "有効", type: "boolean", default: true },
      { key: "voice", label: "音声", type: "select", options: ["Amber", "Blue"], default: "Amber" },
      { key: "volume", label: "音量", type: "number", default: 80 },
    ],
  },
  {
    id: "translator",
    name: "Translator",
    description: "受信テキストを翻訳パイプラインへ送る(モック)",
    settings: [
      { key: "enabled", label: "有効", type: "boolean", default: false },
      { key: "endpoint", label: "エンドポイント", type: "string", default: "http://localhost:5000" },
    ],
  },
];
