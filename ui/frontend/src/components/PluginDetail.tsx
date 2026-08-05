import { useAtomValue, useSetAtom } from "jotai";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { grantsPending } from "@/lib/grants";
import { pluginList$ } from "@/store/pluginList";
import { rpcClient$ } from "@/store/rpcClient";
import type { PluginInfo } from "../types/plugin";
import { DroppedSection } from "./DroppedSection";
import { PermissionWizard } from "./PermissionWizard";
import PluginForm from "./PluginForm";
import { ScheduleSection } from "./ScheduleSection";
import { StateBadge } from "./StateBadge";

export function PluginDetailSkeleton() {
  return (
    <div className="flex-1 space-y-3 p-4">
      <Skeleton className="h-7 w-48" />
      <Skeleton className="h-4 w-72" />
      <Skeleton className="h-32 w-full" />
    </div>
  );
}

export function PluginDetail({ plugin }: { plugin: PluginInfo }) {
  // 設定値の保存は rpcClient$ の共有クライアントで実行し、
  // 応答で pluginList$ 内の該当プラグインを差し替える。
  // 権限系(外部通信・サイドカー・ファイル・バス・ダッシュボード)は
  // PermissionWizard のモーダルに任せる。
  const client = useAtomValue(rpcClient$);
  const setPluginList = useSetAtom(pluginList$);

  const handleChange = async (key: string, value: unknown) => {
    if (!client) throw new Error("RPC に接続されていません");
    const updated = await client.call<Record<string, unknown>>("plugins/set-settings", {
      plugin: plugin.id,
      values: { [key]: value },
    });
    setPluginList((prev) => ({
      ...prev,
      plugins: prev.plugins.map((p) => (p.id === plugin.id ? { ...p, values: updated } : p)),
    }));
  };

  return (
    <article key={plugin.id} className="min-w-0 w-full flex-1 overflow-y-auto p-4">
      <div className="space-y-3">
        <h2 className="flex items-center gap-2 text-lg font-semibold">
          {plugin.name}{" "}
          <StateBadge state={plugin.state} reason={plugin.reason} pending={grantsPending(plugin)} />
          <span className="ml-auto">
            <PermissionWizard plugin={plugin} />
          </span>
        </h2>
        <p className="my-2">{plugin.description}</p>
        <Separator />
        <PluginForm plugin={plugin} onChange={handleChange} />
        <ScheduleSection schedules={plugin.schedules} />
        <DroppedSection dropped={plugin.dropped} />
      </div>
    </article>
  );
}
