import { useAtomValue, useSetAtom } from "jotai";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { pluginList$ } from "@/store/pluginList";
import { rpcClient$ } from "@/store/rpcClient";
import type {
  Capabilities,
  FilesystemConfig,
  FilesystemRoots,
  PluginInfo,
  SidecarConfig,
  Sidecars,
} from "../types/plugin";
import { BusSection } from "./BusSection";
import CapabilitySection from "./CapabilitySection";
import { DashboardSection } from "./DashboardSection";
import FilesystemSection from "./FilesystemSection";
import SidecarSection from "./SidecarSection";

/**
 * 権限系の設定(外部通信・サイドカー・ファイルアクセス・バス・ダッシュボード)を
 * ウィザード式のモーダルに閉じ込める。プラグインが宣言している種類だけが
 * ステップになり、「次へ」で順に確認していく。
 */
export function PermissionWizard({ plugin }: { plugin: PluginInfo }) {
  const client = useAtomValue(rpcClient$);
  const setPluginList = useSetAtom(pluginList$);

  const rpc = () => {
    if (!client) throw new Error("RPC に接続されていません");
    return client;
  };

  const patchPlugin = (pluginId: string, patch: Partial<PluginInfo>) =>
    setPluginList((prev) => ({
      ...prev,
      plugins: prev.plugins.map((p) => (p.id === pluginId ? { ...p, ...patch } : p)),
    }));

  const handleCapabilityToggle = async (granted: boolean) => {
    const updated = await rpc().call<Capabilities>("plugins/set-capabilities", {
      plugin: plugin.id,
      granted,
    });
    patchPlugin(plugin.id, { capabilities: updated });
  };

  const handleSidecarConfig = async (name: string, config: SidecarConfig) => {
    const updated = await rpc().call<Sidecars>("plugins/set-sidecar-config", {
      plugin: plugin.id,
      name,
      config,
    });
    patchPlugin(plugin.id, { sidecars: updated.sidecars });
  };

  const handleSidecarGrant = async (name: string, granted: boolean) => {
    const updated = await rpc().call<Sidecars>("plugins/set-sidecar-grant", {
      plugin: plugin.id,
      name,
      granted,
    });
    patchPlugin(plugin.id, { sidecars: updated.sidecars });
  };

  const handleSidecarControl = async (name: string, action: "start" | "stop" | "restart") => {
    const updated = await rpc().call<Sidecars>("plugins/sidecar-control", {
      plugin: plugin.id,
      name,
      action,
    });
    patchPlugin(plugin.id, { sidecars: updated.sidecars });
  };

  const handleFilesystemConfig = async (name: string, config: FilesystemConfig) => {
    const updated = await rpc().call<FilesystemRoots>("plugins/set-filesystem-config", {
      plugin: plugin.id,
      name,
      config,
    });
    patchPlugin(plugin.id, { filesystem: updated.roots });
  };

  const handleFilesystemGrant = async (name: string, granted: boolean) => {
    const updated = await rpc().call<FilesystemRoots>("plugins/set-filesystem-grant", {
      plugin: plugin.id,
      name,
      granted,
    });
    patchPlugin(plugin.id, { filesystem: updated.roots });
  };

  const handleBusGrant = async (pluginId: string, driver: string, granted: boolean) => {
    const updated = await rpc().setBusGrant(pluginId, driver, granted);
    patchPlugin(pluginId, { bus: updated.bus });
  };

  const handleDashboardGrant = async (pluginId: string, widget: string, granted: boolean) => {
    const updated = await rpc().setDashboardGrant(pluginId, widget, granted);
    patchPlugin(pluginId, { dashboard: updated.dashboard });
  };

  const steps = [
    plugin.capabilities.requests.length > 0 && {
      title: "外部通信",
      body: (
        <CapabilitySection capabilities={plugin.capabilities} onToggle={handleCapabilityToggle} />
      ),
    },
    plugin.sidecars.length > 0 && {
      title: "サイドカー",
      body: (
        <SidecarSection
          sidecars={plugin.sidecars}
          onConfigChange={handleSidecarConfig}
          onGrantChange={handleSidecarGrant}
          onControl={handleSidecarControl}
        />
      ),
    },
    plugin.filesystem.length > 0 && {
      title: "ファイルアクセス",
      body: (
        <FilesystemSection
          roots={plugin.filesystem}
          onConfigChange={handleFilesystemConfig}
          onGrantChange={handleFilesystemGrant}
        />
      ),
    },
    plugin.bus.length > 0 && {
      title: "バス接続",
      body: <BusSection pluginId={plugin.id} bus={plugin.bus} onSetGrant={handleBusGrant} />,
    },
    plugin.dashboard.length > 0 && {
      title: "ダッシュボード",
      body: (
        <DashboardSection
          pluginId={plugin.id}
          dashboard={plugin.dashboard}
          onSetGrant={handleDashboardGrant}
        />
      ),
    },
  ].filter((s): s is WizardStep => Boolean(s));

  return <PermissionWizardDialog subjectName={plugin.name} steps={steps} />;
}

export type WizardStep = { title: string; body: JSX.Element };

/** ウィザードのダイアログ殻。ステップの中身は呼び出し側(プラグイン/ドライバ)が組む。 */
export function PermissionWizardDialog({
  subjectName,
  steps,
}: {
  subjectName: string;
  steps: WizardStep[];
}) {
  const [open, setOpen] = useState(false);
  const [step, setStep] = useState(0);

  if (steps.length === 0) {
    return null;
  }

  const current = steps[step];
  const isLast = step === steps.length - 1;

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) setStep(0);
      }}
    >
      <DialogTrigger asChild>
        <Button type="button" variant="secondary" size="sm">
          権限を設定…
        </Button>
      </DialogTrigger>
      <DialogContent className="max-h-[85vh] w-full max-w-2xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            権限設定({step + 1}/{steps.length}): {current.title}
          </DialogTitle>
          <DialogDescription>
            {subjectName} が要求している権限を順に確認して承認します。
          </DialogDescription>
        </DialogHeader>
        {current.body}
        <DialogFooter>
          <Button
            type="button"
            variant="secondary"
            disabled={step === 0}
            onClick={() => setStep(step - 1)}
          >
            戻る
          </Button>
          {isLast ? (
            <Button type="button" onClick={() => setOpen(false)}>
              完了
            </Button>
          ) : (
            <Button type="button" onClick={() => setStep(step + 1)}>
              次へ
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
