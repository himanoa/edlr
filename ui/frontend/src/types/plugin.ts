// プラグインマニフェストのシリアライズ形(サーバの `plugins/list` レスポンス)に一致させる型。
export type SettingField =
  | { type: "boolean"; key: string; label: string; default: boolean }
  | { type: "string"; key: string; label: string; default: string }
  | { type: "number"; key: string; label: string; default: number }
  | { type: "select"; key: string; label: string; default: string; options: string[] };

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

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  description: string;
  state: "running" | "disabled";
  reason?: string;
  settings: SettingField[];
  values: Record<string, unknown>;
  capabilities: Capabilities;
  sidecars: Sidecar[];
  filesystem: FilesystemRoot[];
  bus: BusRequest[];
  dashboard: DashboardWidget[];
  schedules: Schedule[];
  dropped: DroppedCounts;
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
}

export interface DriversList {
  driversDir: string;
  drivers: DriverInfo[];
}
