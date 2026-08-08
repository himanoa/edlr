import { useAtomValue, useSetAtom } from "jotai";
import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import { grantsPending } from "@/lib/grants";
import { useOptionsPolling } from "@/lib/useOptionsPolling";
import { driverList$ } from "@/store/driverList";
import { rpcClient$ } from "@/store/rpcClient";
import type { DriverInfo, DriversList } from "../types/plugin";
import { DriverPermissionWizard } from "./DriverPermissionWizard";
import PluginForm from "./PluginForm";
import { StateBadge } from "./StateBadge";

export function DriverDetail({ driver }: { driver: DriverInfo }) {
  // `PluginDetail` と同じ構成。設定値の保存だけをここで持ち、
  // 権限系は DriverPermissionWizard のモーダルに任せる。
  const client = useAtomValue(rpcClient$);
  const setDriverList = useSetAtom(driverList$);

  useOptionsPolling(driver.settings, async () => {
    if (!client) return;
    setDriverList(await client.call<DriversList>("drivers/list"));
  });

  const handleChange = async (key: string, value: unknown) => {
    if (!client) throw new Error("RPC に接続されていません");
    const updated = await client.setDriverSettings(driver.id, { [key]: value });
    setDriverList((prev) => ({
      ...prev,
      drivers: prev.drivers.map((d) => (d.id === driver.id ? { ...d, values: updated } : d)),
    }));
  };

  return (
    <article key={driver.id} className="min-w-0 w-full flex-1 overflow-y-auto p-4">
      <div className="space-y-3">
        <h2 className="flex items-center gap-2 text-lg font-semibold">
          {driver.name}{" "}
          <StateBadge state={driver.state} reason={driver.reason} pending={grantsPending(driver)} />
          <span className="ml-auto">
            <DriverPermissionWizard driver={driver} />
          </span>
        </h2>
        <p className="my-2">{driver.description}</p>

        {driver.topics.length > 0 && (
          <ul className="my-2 list-none space-y-1 p-0 text-sm">
            {driver.topics.map((t) => (
              <li key={t.name}>
                <span className="font-mono text-sky-400">{t.name}</span>{" "}
                <Badge variant="outline">{t.retain ? "retain" : "no-retain"}</Badge>{" "}
                <span className="text-muted-foreground">{t.description}</span>
              </li>
            ))}
          </ul>
        )}

        <Separator />
        <PluginForm plugin={driver} onChange={handleChange} />
      </div>
    </article>
  );
}
