import { useEffect, useRef, useState } from "react";
import CapabilitySection from "../components/CapabilitySection";
import FilesystemSection from "../components/FilesystemSection";
import PluginForm from "../components/PluginForm";
import SidecarSection from "../components/SidecarSection";
import { RpcClient } from "../rpc";
import type {
  DriverInfo,
  DriversList,
  FilesystemConfig,
  FilesystemRoots,
  SidecarConfig,
  Sidecars,
} from "../types/plugin";
import { defaultWsUrl } from "../ws";

type Status = "loading" | "ready" | "error";

function StateBadge({ driver }: { driver: DriverInfo }) {
  if (driver.state === "disabled") {
    return (
      <span className="badge badge-plugin-disabled">
        無効{driver.reason ? `: ${driver.reason}` : ""}
      </span>
    );
  }
  return <span className="badge badge-plugin-running">有効</span>;
}

/**
 * 表示専用 + 変更操作をまとめたセクション。`drivers/list` の結果を props
 * で受け取り、設定/capability/sidecar/filesystem/topics を描画する。
 *
 * RPC クライアントは実際に変更操作(設定変更・承認トグルなど)が行われる
 * まで作らない(遅延生成)。一覧表示だけのレンダーや、これを使うテストで
 * 不要な WebSocket 接続を作らないようにするため。
 */
export function Drivers({
  drivers,
  driversDir,
  onReload,
}: {
  drivers: DriverInfo[];
  driversDir: string;
  onReload: () => void;
}) {
  const clientRef = useRef<RpcClient | null>(null);
  const mountedRef = useRef(true);
  const [items, setItems] = useState<DriverInfo[]>(drivers);

  useEffect(() => {
    setItems(drivers);
  }, [drivers]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      clientRef.current?.close();
      clientRef.current = null;
    };
  }, []);

  const getClient = (): RpcClient => {
    if (!clientRef.current) {
      clientRef.current = new RpcClient(defaultWsUrl());
    }
    return clientRef.current;
  };

  const handleChange = (driverId: string) => async (key: string, value: unknown) => {
    const updated = await getClient().setDriverSettings(driverId, { [key]: value });
    if (!mountedRef.current) return;
    setItems((prev) => prev.map((d) => (d.id === driverId ? { ...d, values: updated } : d)));
  };

  const handleCapabilityToggle = (driverId: string) => async (granted: boolean) => {
    const updated = await getClient().setDriverCapabilities(driverId, granted);
    if (!mountedRef.current) return;
    setItems((prev) => prev.map((d) => (d.id === driverId ? { ...d, capabilities: updated } : d)));
  };

  const handleSidecarConfig =
    (driverId: string) => async (name: string, config: SidecarConfig) => {
      const updated = await getClient().call<Sidecars>("drivers/set-sidecar-config", {
        driver: driverId,
        name,
        config,
      });
      if (!mountedRef.current) return;
      setItems((prev) =>
        prev.map((d) => (d.id === driverId ? { ...d, sidecars: updated.sidecars } : d)),
      );
    };

  const handleSidecarGrant =
    (driverId: string) => async (name: string, granted: boolean) => {
      const updated = await getClient().call<Sidecars>("drivers/set-sidecar-grant", {
        driver: driverId,
        name,
        granted,
      });
      if (!mountedRef.current) return;
      setItems((prev) =>
        prev.map((d) => (d.id === driverId ? { ...d, sidecars: updated.sidecars } : d)),
      );
    };

  const handleSidecarControl =
    (driverId: string) => async (name: string, action: "start" | "stop" | "restart") => {
      const updated = await getClient().call<Sidecars>("drivers/sidecar-control", {
        driver: driverId,
        name,
        action,
      });
      if (!mountedRef.current) return;
      setItems((prev) =>
        prev.map((d) => (d.id === driverId ? { ...d, sidecars: updated.sidecars } : d)),
      );
    };

  const handleFilesystemConfig =
    (driverId: string) => async (name: string, config: FilesystemConfig) => {
      const updated = await getClient().call<FilesystemRoots>("drivers/set-filesystem-config", {
        driver: driverId,
        name,
        config,
      });
      if (!mountedRef.current) return;
      setItems((prev) =>
        prev.map((d) => (d.id === driverId ? { ...d, filesystem: updated.roots } : d)),
      );
    };

  const handleFilesystemGrant =
    (driverId: string) => async (name: string, granted: boolean) => {
      const updated = await getClient().call<FilesystemRoots>("drivers/set-filesystem-grant", {
        driver: driverId,
        name,
        granted,
      });
      if (!mountedRef.current) return;
      setItems((prev) =>
        prev.map((d) => (d.id === driverId ? { ...d, filesystem: updated.roots } : d)),
      );
    };

  return (
    <section>
      <h1>Drivers</h1>
      <button type="button" onClick={onReload}>
        更新
      </button>
      {items.length === 0 && (
        <p className="note">
          ドライバが見つかりませんでした。{driversDir} にドライバを配置してください。
        </p>
      )}
      {items.map((d) => (
        <article key={d.id} className="plugin-card">
          <h2>
            {d.name} <StateBadge driver={d} />
          </h2>
          <p>{d.description}</p>

          {d.topics.length > 0 && (
            <ul className="driver-topics">
              {d.topics.map((t) => (
                <li key={t.name}>
                  <span className="driver-topic-name">{t.name}</span>{" "}
                  <span className="badge badge-topic-retain">
                    {t.retain ? "retain" : "no-retain"}
                  </span>{" "}
                  <span className="driver-topic-description">{t.description}</span>
                </li>
              ))}
            </ul>
          )}

          <PluginForm plugin={d} onChange={handleChange(d.id)} />
          <CapabilitySection
            capabilities={d.capabilities}
            onToggle={handleCapabilityToggle(d.id)}
          />
          <SidecarSection
            sidecars={d.sidecars}
            onConfigChange={handleSidecarConfig(d.id)}
            onGrantChange={handleSidecarGrant(d.id)}
            onControl={handleSidecarControl(d.id)}
          />
          <FilesystemSection
            roots={d.filesystem}
            onConfigChange={handleFilesystemConfig(d.id)}
            onGrantChange={handleFilesystemGrant(d.id)}
          />
        </article>
      ))}
    </section>
  );
}

/** `Plugins.tsx` と同じ形の RPC 取得コンテナ。App.tsx から Drivers タブとして使う。 */
export default function DriversPage() {
  const clientRef = useRef<RpcClient | null>(null);
  const mountedRef = useRef(true);
  const [status, setStatus] = useState<Status>("loading");
  const [driversDir, setDriversDir] = useState("");
  const [drivers, setDrivers] = useState<DriverInfo[]>([]);
  const [error, setError] = useState<string | null>(null);

  const load = (client: RpcClient) => {
    client
      .listDrivers()
      .then((res: DriversList) => {
        if (!mountedRef.current) return;
        setDriversDir(res.driversDir);
        setDrivers(res.drivers);
        setStatus("ready");
      })
      .catch((err) => {
        if (!mountedRef.current) return;
        setError(err instanceof Error ? err.message : String(err));
        setStatus("error");
      });
  };

  useEffect(() => {
    mountedRef.current = true;
    const client = new RpcClient(defaultWsUrl());
    clientRef.current = client;
    load(client);

    return () => {
      mountedRef.current = false;
      clientRef.current = null;
      client.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleReload = () => {
    const client = clientRef.current;
    if (!client) return;
    setStatus("loading");
    load(client);
  };

  if (status === "loading") {
    return <p className="note">読み込み中…</p>;
  }
  if (status === "error") {
    return <p className="form-error">ドライバ一覧の取得に失敗しました: {error}</p>;
  }

  return <Drivers drivers={drivers} driversDir={driversDir} onReload={handleReload} />;
}
