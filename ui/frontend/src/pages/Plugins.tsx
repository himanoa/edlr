import { useAtom, useAtomValue, useSetAtom } from "jotai";
import { AlertCircle, PackageOpen } from "lucide-react";
import { Component, type ReactNode, Suspense } from "react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Skeleton } from "@/components/ui/skeleton";
import { pluginList$ } from "@/store/pluginList";
import { rpcClient$ } from "@/store/rpcClient";
import { BusSection } from "../components/BusSection";
import CapabilitySection from "../components/CapabilitySection";
import { DashboardSection } from "../components/DashboardSection";
import { DroppedSection } from "../components/DroppedSection";
import FilesystemSection from "../components/FilesystemSection";
import PluginForm from "../components/PluginForm";
import { ScheduleSection } from "../components/ScheduleSection";
import SidecarSection from "../components/SidecarSection";
import type {
  Capabilities,
  FilesystemConfig,
  FilesystemRoots,
  PluginInfo,
  SidecarConfig,
  Sidecars,
} from "../types/plugin";
import { selectedPluginId$ } from "@/store/selectedPluginId";

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

class LoadErrorBoundary extends Component<{ children: ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null };
  static getDerivedStateFromError(error: Error) {
    return { error };
  }
  render() {
    if (this.state.error) {
      return (
        <section>
          <Alert variant="destructive">
            <AlertCircle />
            <AlertTitle>プラグイン一覧の取得に失敗しました</AlertTitle>
            <AlertDescription>{this.state.error.message}</AlertDescription>
          </Alert>
        </section>
      );
    }
    return this.props.children;
  }
}

export default function Plugins() {
  return (
    <LoadErrorBoundary>
      <Suspense
        fallback={
          <section role="status" className="flex h-full gap-4">
            <span className="sr-only">読み込み中…</span>
            <div className="w-64 shrink-0 space-y-2 border-r pr-2">
              <Skeleton className="h-12 w-full" />
              <Skeleton className="h-12 w-full" />
              <Skeleton className="h-12 w-full" />
            </div>
            <div className="max-w-2xl flex-1 space-y-3">
              <Skeleton className="h-7 w-48" />
              <Skeleton className="h-4 w-72" />
              <Skeleton className="h-32 w-full" />
            </div>
          </section>
        }
      >
        <PluginsContent />
      </Suspense>
    </LoadErrorBoundary>
  );
}

function PluginsContent() {
  // 一覧の取得は pluginList$ が担う。rpcClient$ は設定変更・承認トグルなどの
  // ミューテーション用の共有クライアント。
  const client = useAtomValue(rpcClient$);
  const { pluginsDir, plugins } = useAtomValue(pluginList$);
  const setPluginList = useSetAtom(pluginList$);
  const [selectedId, setSelectedId] = useAtom(selectedPluginId$);

  const rpc = () => {
    if (!client) throw new Error("RPC に接続されていません");
    return client;
  };

  const patchPlugin = (pluginId: string, patch: Partial<PluginInfo>) =>
    setPluginList((prev) => ({
      ...prev,
      plugins: prev.plugins.map((p) => (p.id === pluginId ? { ...p, ...patch } : p)),
    }));

  const handleChange = (pluginId: string) => async (key: string, value: unknown) => {
    const updated = await rpc().call<Record<string, unknown>>("plugins/set-settings", {
      plugin: pluginId,
      values: { [key]: value },
    });
    patchPlugin(pluginId, { values: updated });
  };

  const handleCapabilityToggle = (pluginId: string) => async (granted: boolean) => {
    const updated = await rpc().call<Capabilities>("plugins/set-capabilities", {
      plugin: pluginId,
      granted,
    });
    patchPlugin(pluginId, { capabilities: updated });
  };

  const handleSidecarConfig = (pluginId: string) => async (name: string, config: SidecarConfig) => {
    const updated = await rpc().call<Sidecars>("plugins/set-sidecar-config", {
      plugin: pluginId,
      name,
      config,
    });
    patchPlugin(pluginId, { sidecars: updated.sidecars });
  };

  const handleSidecarGrant = (pluginId: string) => async (name: string, granted: boolean) => {
    const updated = await rpc().call<Sidecars>("plugins/set-sidecar-grant", {
      plugin: pluginId,
      name,
      granted,
    });
    patchPlugin(pluginId, { sidecars: updated.sidecars });
  };

  const handleSidecarControl =
    (pluginId: string) => async (name: string, action: "start" | "stop" | "restart") => {
      const updated = await rpc().call<Sidecars>("plugins/sidecar-control", {
        plugin: pluginId,
        name,
        action,
      });
      patchPlugin(pluginId, { sidecars: updated.sidecars });
    };

  const handleFilesystemConfig =
    (pluginId: string) => async (name: string, config: FilesystemConfig) => {
      const updated = await rpc().call<FilesystemRoots>("plugins/set-filesystem-config", {
        plugin: pluginId,
        name,
        config,
      });
      patchPlugin(pluginId, { filesystem: updated.roots });
    };

  const handleFilesystemGrant =
    (pluginId: string) => async (name: string, granted: boolean) => {
      const updated = await rpc().call<FilesystemRoots>("plugins/set-filesystem-grant", {
        plugin: pluginId,
        name,
        granted,
      });
      patchPlugin(pluginId, { filesystem: updated.roots });
    };

  const handleBusGrant = async (pluginId: string, driver: string, granted: boolean) => {
    const updated = await rpc().setBusGrant(pluginId, driver, granted);
    patchPlugin(pluginId, { bus: updated.bus });
  };

  const handleDashboardGrant = async (pluginId: string, widget: string, granted: boolean) => {
    const updated = await rpc().setDashboardGrant(pluginId, widget, granted);
    patchPlugin(pluginId, { dashboard: updated.dashboard });
  };

  const selected = plugins.find((p) => p.id === selectedId) ?? plugins[0];

  if (plugins.length === 0) {
    return (
      <section className="h-full">
        <Empty className="h-full">
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <PackageOpen />
            </EmptyMedia>
            <EmptyTitle>プラグインが見つかりませんでした</EmptyTitle>
            <EmptyDescription>
              {pluginsDir} にプラグインを配置してください。
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
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
