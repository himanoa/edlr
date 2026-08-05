import { useEffect, useRef, useState } from "react";
import { Badge } from "@/components/ui/badge";
import { BusSection } from "../components/BusSection";
import CapabilitySection from "../components/CapabilitySection";
import { DashboardSection } from "../components/DashboardSection";
import { DroppedSection } from "../components/DroppedSection";
import FilesystemSection from "../components/FilesystemSection";
import PluginForm from "../components/PluginForm";
import { ScheduleSection } from "../components/ScheduleSection";
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
      <Badge className="bg-red-950 text-red-400">
        無効{plugin.reason ? `: ${plugin.reason}` : ""}
      </Badge>
    );
  }
  return <Badge className="bg-emerald-950 text-emerald-400">有効</Badge>;
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
  const [selectedId, setSelectedId] = useState<string | null>(null);

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

  const selected = plugins.find((p) => p.id === selectedId) ?? plugins[0];

  if (status !== "ready") {
    return (
      <section>
        {status === "loading" && <p className="text-sm text-muted-foreground">読み込み中…</p>}
        {status === "error" && (
          <p className="mt-1.5 text-sm text-red-400">
            プラグイン一覧の取得に失敗しました: {error}
          </p>
        )}
      </section>
    );
  }

  if (plugins.length === 0) {
    return (
      <section>
        <p className="text-sm text-muted-foreground">
          プラグインが見つかりませんでした。{pluginsDir} にプラグインを配置してください。
        </p>
      </section>
    );
  }

  return (
    <section className="flex h-full gap-4">
      <nav className="w-64 shrink-0 overflow-y-auto border-r pr-2">
        <ul className="m-0 list-none space-y-1 p-0">
          {plugins.map((p) => (
            <li key={p.id}>
              <button
                type="button"
                onClick={() => setSelectedId(p.id)}
                aria-current={p.id === selected?.id}
                className={`w-full rounded-md px-3 py-2 text-left hover:bg-accent/50 ${
                  p.id === selected?.id ? "bg-accent" : ""
                }`}
              >
                <span className="flex items-center gap-2">
                  <span
                    aria-hidden
                    className={`size-2 shrink-0 rounded-full ${
                      p.state === "disabled" ? "bg-red-400" : "bg-emerald-400"
                    }`}
                  />
                  <span className="truncate font-medium">{p.name}</span>
                </span>
                <span className="mt-0.5 block truncate text-xs text-muted-foreground">
                  {p.description}
                </span>
              </button>
            </li>
          ))}
        </ul>
      </nav>
      {selected && (
        <article key={selected.id} className="min-w-0 flex-1 overflow-y-auto pr-1">
          <div className="max-w-2xl">
            <h2 className="flex items-center gap-2 text-lg font-semibold">
              {selected.name} <StateBadge plugin={selected} />
            </h2>
            <p className="my-2">{selected.description}</p>
            <PluginForm plugin={selected} onChange={handleChange(selected.id)} />
            <CapabilitySection
              capabilities={selected.capabilities}
              onToggle={handleCapabilityToggle(selected.id)}
            />
            <SidecarSection
              sidecars={selected.sidecars}
              onConfigChange={handleSidecarConfig(selected.id)}
              onGrantChange={handleSidecarGrant(selected.id)}
              onControl={handleSidecarControl(selected.id)}
            />
            <FilesystemSection
              roots={selected.filesystem}
              onConfigChange={handleFilesystemConfig(selected.id)}
              onGrantChange={handleFilesystemGrant(selected.id)}
            />
            <BusSection pluginId={selected.id} bus={selected.bus} onSetGrant={handleBusGrant} />
            <DashboardSection
              pluginId={selected.id}
              dashboard={selected.dashboard}
              onSetGrant={handleDashboardGrant}
            />
            <ScheduleSection schedules={selected.schedules} />
            <DroppedSection dropped={selected.dropped} />
          </div>
        </article>
      )}
    </section>
  );
}
