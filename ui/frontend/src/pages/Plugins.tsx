import { useEffect, useRef, useState } from "react";
import PluginForm from "../components/PluginForm";
import { RpcClient } from "../rpc";
import type { PluginInfo, PluginsList } from "../types/plugin";
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
          </article>
        ))}
    </section>
  );
}
