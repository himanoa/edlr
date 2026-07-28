import { useEffect, useRef, useState } from "react";
import { BusSection } from "../components/BusSection";
import CapabilitySection from "../components/CapabilitySection";
import { DashboardSection } from "../components/DashboardSection";
import FilesystemSection from "../components/FilesystemSection";
import PluginForm from "../components/PluginForm";
import SidecarSection from "../components/SidecarSection";
import { RpcClient } from "../rpc";
import type {
  Capabilities,
  FilesystemConfig,
  FilesystemRoots,
  PluginInfo,
  PluginsList,
  SidecarConfig,
  Sidecars,
} from "../types/plugin";
import { defaultWsUrl } from "../ws";

type Status = "loading" | "ready" | "error";

function StateBadge({ plugin }: { plugin: PluginInfo }) {
  if (plugin.state === "disabled") {
    return (
      <span className="badge badge-plugin-disabled">
        無効{plugin.reason ? `: ${plugin.reason}` : ""}
      </span>
    );
  }
  return <span className="badge badge-plugin-running">有効</span>;
}

export default function Plugins() {
  const clientRef = useRef<RpcClient | null>(null);
  // Guards every post-await setState in this component (the initial `plugins/list`
  // load and `handleChange`'s `plugins/set-settings` round-trip) against firing
  // after the component has unmounted (e.g. the user switches tabs mid-save).
  const mountedRef = useRef(true);
  const [status, setStatus] = useState<Status>("loading");
  const [pluginsDir, setPluginsDir] = useState("");
  const [plugins, setPlugins] = useState<PluginInfo[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    mountedRef.current = true;
    const client = new RpcClient(defaultWsUrl());
    clientRef.current = client;

    client
      .call<PluginsList>("plugins/list")
      .then((res) => {
        if (!mountedRef.current) return;
        setPluginsDir(res.pluginsDir);
        setPlugins(res.plugins);
        setStatus("ready");
      })
      .catch((err) => {
        if (!mountedRef.current) return;
        setError(err instanceof Error ? err.message : String(err));
        setStatus("error");
      });

    return () => {
      mountedRef.current = false;
      clientRef.current = null;
      client.close();
    };
  }, []);

  const handleChange = (pluginId: string) => async (key: string, value: unknown) => {
    const client = clientRef.current;
    if (!client) throw new Error("RPC に接続されていません");
    const updated = await client.call<Record<string, unknown>>("plugins/set-settings", {
      plugin: pluginId,
      values: { [key]: value },
    });
    if (!mountedRef.current) return;
    setPlugins((prev) =>
      prev.map((p) => (p.id === pluginId ? { ...p, values: updated } : p)),
    );
  };

  const handleCapabilityToggle = (pluginId: string) => async (granted: boolean) => {
    const client = clientRef.current;
    if (!client) throw new Error("RPC に接続されていません");
    const updated = await client.call<Capabilities>("plugins/set-capabilities", {
      plugin: pluginId,
      granted,
    });
    if (!mountedRef.current) return;
    setPlugins((prev) =>
      prev.map((p) => (p.id === pluginId ? { ...p, capabilities: updated } : p)),
    );
  };

  const handleSidecarConfig = (pluginId: string) => async (name: string, config: SidecarConfig) => {
    const client = clientRef.current;
    if (!client) throw new Error("RPC に接続されていません");
    const updated = await client.call<Sidecars>("plugins/set-sidecar-config", {
      plugin: pluginId,
      name,
      config,
    });
    if (!mountedRef.current) return;
    setPlugins((prev) =>
      prev.map((p) => (p.id === pluginId ? { ...p, sidecars: updated.sidecars } : p)),
    );
  };

  const handleSidecarGrant = (pluginId: string) => async (name: string, granted: boolean) => {
    const client = clientRef.current;
    if (!client) throw new Error("RPC に接続されていません");
    const updated = await client.call<Sidecars>("plugins/set-sidecar-grant", {
      plugin: pluginId,
      name,
      granted,
    });
    if (!mountedRef.current) return;
    setPlugins((prev) =>
      prev.map((p) => (p.id === pluginId ? { ...p, sidecars: updated.sidecars } : p)),
    );
  };

  const handleSidecarControl =
    (pluginId: string) => async (name: string, action: "start" | "stop" | "restart") => {
      const client = clientRef.current;
      if (!client) throw new Error("RPC に接続されていません");
      const updated = await client.call<Sidecars>("plugins/sidecar-control", {
        plugin: pluginId,
        name,
        action,
      });
      if (!mountedRef.current) return;
      setPlugins((prev) =>
        prev.map((p) => (p.id === pluginId ? { ...p, sidecars: updated.sidecars } : p)),
      );
    };

  const handleFilesystemConfig =
    (pluginId: string) => async (name: string, config: FilesystemConfig) => {
      const client = clientRef.current;
      if (!client) throw new Error("RPC に接続されていません");
      const updated = await client.call<FilesystemRoots>("plugins/set-filesystem-config", {
        plugin: pluginId,
        name,
        config,
      });
      if (!mountedRef.current) return;
      setPlugins((prev) =>
        prev.map((p) => (p.id === pluginId ? { ...p, filesystem: updated.roots } : p)),
      );
    };

  const handleFilesystemGrant =
    (pluginId: string) => async (name: string, granted: boolean) => {
      const client = clientRef.current;
      if (!client) throw new Error("RPC に接続されていません");
      const updated = await client.call<FilesystemRoots>("plugins/set-filesystem-grant", {
        plugin: pluginId,
        name,
        granted,
      });
      if (!mountedRef.current) return;
      setPlugins((prev) =>
        prev.map((p) => (p.id === pluginId ? { ...p, filesystem: updated.roots } : p)),
      );
    };

  const handleBusGrant = async (pluginId: string, driver: string, granted: boolean) => {
    const client = clientRef.current;
    if (!client) throw new Error("RPC に接続されていません");
    const updated = await client.setBusGrant(pluginId, driver, granted);
    if (!mountedRef.current) return;
    setPlugins((prev) =>
      prev.map((p) => (p.id === pluginId ? { ...p, bus: updated.bus } : p)),
    );
  };

  const handleDashboardGrant = async (pluginId: string, widget: string, granted: boolean) => {
    const client = clientRef.current;
    if (!client) throw new Error("RPC に接続されていません");
    const updated = await client.setDashboardGrant(pluginId, widget, granted);
    if (!mountedRef.current) return;
    setPlugins((prev) =>
      prev.map((p) => (p.id === pluginId ? { ...p, dashboard: updated.dashboard } : p)),
    );
  };

  return (
    <section>
      <h1>Plugins</h1>
      {status === "loading" && <p className="note">読み込み中…</p>}
      {status === "error" && <p className="form-error">プラグイン一覧の取得に失敗しました: {error}</p>}
      {status === "ready" && plugins.length === 0 && (
        <p className="note">
          プラグインが見つかりませんでした。{pluginsDir} にプラグインを配置してください。
        </p>
      )}
      {status === "ready" &&
        plugins.map((p) => (
          <article key={p.id} className="plugin-card">
            <h2>
              {p.name} <StateBadge plugin={p} />
            </h2>
            <p>{p.description}</p>
            <PluginForm plugin={p} onChange={handleChange(p.id)} />
            <CapabilitySection
              capabilities={p.capabilities}
              onToggle={handleCapabilityToggle(p.id)}
            />
            <SidecarSection
              sidecars={p.sidecars}
              onConfigChange={handleSidecarConfig(p.id)}
              onGrantChange={handleSidecarGrant(p.id)}
              onControl={handleSidecarControl(p.id)}
            />
            <FilesystemSection
              roots={p.filesystem}
              onConfigChange={handleFilesystemConfig(p.id)}
              onGrantChange={handleFilesystemGrant(p.id)}
            />
            <BusSection pluginId={p.id} bus={p.bus} onSetGrant={handleBusGrant} />
            <DashboardSection
              pluginId={p.id}
              dashboard={p.dashboard}
              onSetGrant={handleDashboardGrant}
            />
          </article>
        ))}
    </section>
  );
}
