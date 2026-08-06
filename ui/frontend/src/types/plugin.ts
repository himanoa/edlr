// `select` の候補 1 件。表示は `label`、保存されるのは `value`
// (話者一覧のように「アメノちゃん/ギャル」を見せて `uuid:styleId` を保存する
// ケースがあるため、サーバ側で常に 2 つに開いてから返される)。
export interface SelectOption {
  value: string;
  label: string;
}

// プラグインマニフェストのシリアライズ形(サーバの `plugins/list` レスポンス)に一致させる型。
export type SettingField =
  | { type: "boolean"; key: string; label: string; default: boolean }
  | { type: "string"; key: string; label: string; default: string }
  | { type: "number"; key: string; label: string; default: number }
  // 有界な数値(音量など)。UI はスライダーで描画する。min/max は必須、
  // step はマニフェスト省略時にサーバが 1 を埋めて返す。
  | {
      type: "slider";
      key: string;
      label: string;
      default: number;
      min: number;
      max: number;
      step: number;
    }
  // 候補は静的(マニフェストの `options`)でも動的(`options-from` でドライバの
  // retain トピックを指す)でもよいが、サーバは常に解決済みの `{value,label}` の
  // 配列にして返す。**`options: null` は「動的な候補をまだ取得できていない」**
  // (ドライバが未インストール・無効化・まだ一度も emit していない)を表す。
  | {
      type: "select";
      key: string;
      label: string;
      default: string;
      options: SelectOption[] | null;
      optionsFrom?: { driver: string; topic: string };
    }
  // API キーなどの秘密情報。`default` を持たない(マニフェストに秘密情報を
  // 書けてしまう余地を作らないため)。値はサーバから返ってこない
  // (write-only)ので、設定済みかどうかは `PluginInfo.secretsSet` で判断する。
  | { type: "secret"; key: string; label: string }
  // ユーザーが UI で行を追加・削除する `string -> string` のマップ。`default` を
  // 持たない(常に空オブジェクトから始まる)。
  | { type: "map"; key: string; label: string };

export interface CapabilityRequest {
  kind: "http";
  hosts: string[];
  reason: string;
}

export interface Capabilities {
  requests: CapabilityRequest[];
  granted: boolean;
  staleGrant: boolean;
}

export interface SidecarConfig {
  command: string;
  args: string[];
  port: number;
  replicas: number;
}

export interface SidecarInstance {
  index: number;
  port: number;
  state: "running" | "exited";
  exitCode: number | null;
}

export interface Sidecar {
  name: string;
  reason: string;
  args: string[];
  port: number;
  scalable: boolean;
  granted: boolean;
  staleGrant: boolean;
  config: SidecarConfig;
  instances: SidecarInstance[];
}

export interface Sidecars {
  sidecars: Sidecar[];
}

export interface FilesystemConfig {
  path: string;
}

export interface FilesystemRoot {
  name: string;
  reason: string;
  mode: "read" | "read-write";
  /** 欠落(旧デーモン)時は directory 扱い */
  target?: "directory" | "file";
  granted: boolean;
  staleGrant: boolean;
  config: FilesystemConfig;
}

export interface FilesystemRoots {
  roots: FilesystemRoot[];
}

export interface BusRequest {
  driver: string;
  publish: string[];
  subscribe: string[];
  reason: string;
  granted: boolean;
  staleGrant: boolean;
  resolved: boolean;
}

export type WidgetSize = "small" | "medium" | "large";

/** `plugins/list` / `plugins/set-dashboard-grant` が返すウィジェット宣言 1 件。 */
export interface DashboardWidget {
  id: string;
  title: string;
  entry: string;
  size: WidgetSize;
  granted: boolean;
  staleGrant: boolean;
  resolved: boolean;
}

/** `dashboard/list` が返す grant 済みウィジェット 1 件(Dashboard 画面用)。 */
export interface DashboardListEntry {
  plugin: string;
  pluginName: string;
  widget: string;
  title: string;
  url: string;
  size: WidgetSize;
  events: string[];
  resolved: boolean;
  state: "running" | "disabled";
}

/**
 * `plugins/list` が返すスケジュール宣言 1 件(`[[schedule]]`)。
 *
 * `spec` は表示用に整形済みの文字列("every 60s" / "cron: 0 9 * * *")。
 * `next` は ISO8601(ローカル時刻)の次回発火時刻。プラグインスレッドが
 * 実際に予定している時刻で、起動途中や Disabled のときだけサーバ側の推定値へ
 * フォールバックする(詳細は `core/src/plugin/registry.rs` の `ScheduleInfo`
 * のドキュメントコメント参照)。
 */
export interface Schedule {
  name: string;
  spec: string;
  next: string;
}

/**
 * プラグインの作業キューが満杯だったために捨てられた件数
 * (デーモン起動時からの累計)。
 *
 * journal イベントは読み取り位置が配送と独立に進むため replay でも戻らず、
 * バス配信も再送されない。つまりこの数はそのまま**失われたイベント数**。
 */
export interface DroppedCounts {
  events: number;
  busDeliveries: number;
}

/**
 * 設定フォームのセクション 1 件(`plugins/list` / `drivers/list` の `layout` に含まれる)。
 * `children` は入れ子の `LayoutSection` を許すので、任意の深さでグルーピングできる。
 */
export interface LayoutSection {
  title: string;
  description?: string;
  children: LayoutNode[];
}

/** レイアウト木の葉(単一フィールド参照)か、入れ子の `LayoutSection`。 */
export type LayoutNode = { field: string } | LayoutSection;

/**
 * `plugins/list` / `drivers/list` が返すフォームレイアウト。サーバ側で参照解決済み
 * (不正キーは除去、未参照キーは末尾セクションに補完済み)なので、UI 側は
 * そのまま描画すればよい。`layout` 自体が `null` なら従来通りの平坦描画にする。
 */
export interface Layout {
  sections: LayoutSection[];
}

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  state: "running" | "disabled";
  reason?: string;
  settings: SettingField[];
  /** `secret` 型のキーは含まれない(サーバが読み出し応答から除外する)。 */
  values: Record<string, unknown>;
  /** 空でない値が保存済みの `secret` 型設定のキー。 */
  secretsSet: string[];
  capabilities: Capabilities;
  sidecars: Sidecar[];
  filesystem: FilesystemRoot[];
  bus: BusRequest[];
  dashboard: DashboardWidget[];
  schedules: Schedule[];
  dropped: DroppedCounts;
  layout: Layout | null;
}

export interface PluginsList {
  pluginsDir: string;
  plugins: PluginInfo[];
}

export interface TopicSpec {
  name: string;
  retain: boolean;
  description: string;
}

export interface DriverInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  topics: TopicSpec[];
  settings: SettingField[];
  values: Record<string, unknown>;
  capabilities: Capabilities;
  sidecars: Sidecar[];
  filesystem: FilesystemRoot[];
  state: "running" | "disabled";
  reason?: string;
  layout: Layout | null;
}

export interface DriversList {
  driversDir: string;
  drivers: DriverInfo[];
}
