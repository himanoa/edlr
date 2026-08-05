import { useAtomValue, useSetAtom } from "jotai";
import { driverList$ } from "@/store/driverList";
import { rpcClient$ } from "@/store/rpcClient";
import type {
  DriverInfo,
  FilesystemConfig,
  FilesystemRoots,
  SidecarConfig,
  Sidecars,
} from "../types/plugin";
import CapabilitySection from "./CapabilitySection";
import FilesystemSection from "./FilesystemSection";
import { PermissionWizardDialog, type WizardStep } from "./PermissionWizard";
import SidecarSection from "./SidecarSection";

/** `PermissionWizard` のドライバ版。RPC が drivers/* で、バス/ダッシュボードが無い。 */
export function DriverPermissionWizard({ driver }: { driver: DriverInfo }) {
  const client = useAtomValue(rpcClient$);
  const setDriverList = useSetAtom(driverList$);

  const rpc = () => {
    if (!client) throw new Error("RPC に接続されていません");
    return client;
  };

  const patchDriver = (driverId: string, patch: Partial<DriverInfo>) =>
    setDriverList((prev) => ({
      ...prev,
      drivers: prev.drivers.map((d) => (d.id === driverId ? { ...d, ...patch } : d)),
    }));

  const handleCapabilityToggle = async (granted: boolean) => {
    const updated = await rpc().setDriverCapabilities(driver.id, granted);
    patchDriver(driver.id, { capabilities: updated });
  };

  const handleSidecarConfig = async (name: string, config: SidecarConfig) => {
    const updated = await rpc().call<Sidecars>("drivers/set-sidecar-config", {
      driver: driver.id,
      name,
      config,
    });
    patchDriver(driver.id, { sidecars: updated.sidecars });
  };

  const handleSidecarGrant = async (name: string, granted: boolean) => {
    const updated = await rpc().call<Sidecars>("drivers/set-sidecar-grant", {
      driver: driver.id,
      name,
      granted,
    });
    patchDriver(driver.id, { sidecars: updated.sidecars });
  };

  const handleSidecarControl = async (name: string, action: "start" | "stop" | "restart") => {
    const updated = await rpc().call<Sidecars>("drivers/sidecar-control", {
      driver: driver.id,
      name,
      action,
    });
    patchDriver(driver.id, { sidecars: updated.sidecars });
  };

  const handleFilesystemConfig = async (name: string, config: FilesystemConfig) => {
    const updated = await rpc().call<FilesystemRoots>("drivers/set-filesystem-config", {
      driver: driver.id,
      name,
      config,
    });
    patchDriver(driver.id, { filesystem: updated.roots });
  };

  const handleFilesystemGrant = async (name: string, granted: boolean) => {
    const updated = await rpc().call<FilesystemRoots>("drivers/set-filesystem-grant", {
      driver: driver.id,
      name,
      granted,
    });
    patchDriver(driver.id, { filesystem: updated.roots });
  };

  const steps = [
    driver.capabilities.requests.length > 0 && {
      title: "外部通信",
      body: (
        <CapabilitySection capabilities={driver.capabilities} onToggle={handleCapabilityToggle} />
      ),
    },
    driver.sidecars.length > 0 && {
      title: "サイドカー",
      body: (
        <SidecarSection
          sidecars={driver.sidecars}
          onConfigChange={handleSidecarConfig}
          onGrantChange={handleSidecarGrant}
          onControl={handleSidecarControl}
        />
      ),
    },
    driver.filesystem.length > 0 && {
      title: "ファイルアクセス",
      body: (
        <FilesystemSection
          roots={driver.filesystem}
          onConfigChange={handleFilesystemConfig}
          onGrantChange={handleFilesystemGrant}
        />
      ),
    },
  ].filter((s): s is WizardStep => Boolean(s));

  return <PermissionWizardDialog subjectName={driver.name} steps={steps} />;
}
