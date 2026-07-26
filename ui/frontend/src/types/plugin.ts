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
}

export interface PluginsList {
  pluginsDir: string;
  plugins: PluginInfo[];
}
