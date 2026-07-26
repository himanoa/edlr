import { useEffect, useState } from "react";
import { invoke, isTauri } from "../lib/tauri";
import type { Sidecar, SidecarConfig } from "../types/plugin";

/// 承認すると実際に採番される実ポート列(`port, port+1, …, port+replicas-1`)。
/// `core::plugin::sidecar::assign_ports` と同じ規則(`replicas` は 1 未満なら
/// 1 として扱う)。承認画面はこの一覧をそのまま見せる -- ここに挙がる各ポート
/// の `127.0.0.1` は、承認と同時に(サイドカーを 1 つも起動していなくても)
/// `driver-http` からの通信が暗黙に許可される対象になるため
/// (`core::plugin::sidecar_runtime::implicit_http_hosts` 参照)。
function assignPorts(config: SidecarConfig): number[] {
  const replicas = Math.max(1, config.replicas || 1);
  const ports: number[] = [];
  for (let offset = 0; offset < replicas; offset++) {
    const port = config.port + offset;
    if (port > 65535) break;
    ports.push(port);
  }
  return ports;
}

function SidecarCard({
  sidecar,
  onConfigChange,
  onGrantChange,
  onControl,
}: {
  sidecar: Sidecar;
  onConfigChange: (name: string, config: SidecarConfig) => Promise<void>;
  onGrantChange: (name: string, granted: boolean) => Promise<void>;
  onControl: (name: string, action: "start" | "stop" | "restart") => Promise<void>;
}) {
  const [command, setCommand] = useState(sidecar.config.command);
  const [args, setArgs] = useState(sidecar.config.args.join("\n"));
  const [port, setPort] = useState(String(sidecar.config.port));
  const [replicas, setReplicas] = useState(String(sidecar.config.replicas));
  const [saving, setSaving] = useState(false);

  // フォーム state はマウント時の初期値だけで駆動していた(`useState` の
  // 初期値は最初のレンダーでしか評価されない)ため、別のクライアント(別の
  // 開いた設定画面や、将来の RPC 経路)が同じサイドカーの設定を変えても
  // 画面には古い値が残り続け、そのまま「保存」を押すと相手の変更をこちらの
  // 古い値で黙って上書きしてしまっていた(Minor: 最終レビューで見つかった
  // 取りこぼし)。`sidecar.config` の実際の値が変わるたびにフォーム state を
  // サーバの最新値へ追随させる。依存配列はオブジェクト参照ではなく実際の値
  // (`args` は結合した文字列)にしているので、`instances` の更新など
  // `config` の中身が変わらない再レンダーでは発火せず、ユーザーの未保存の
  // 入力を不必要に巻き戻すことはない。
  useEffect(() => {
    setCommand(sidecar.config.command);
    setArgs(sidecar.config.args.join("\n"));
    setPort(String(sidecar.config.port));
    setReplicas(String(sidecar.config.replicas));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    sidecar.config.command,
    sidecar.config.args.join("\n"),
    sidecar.config.port,
    sidecar.config.replicas,
  ]);
  const [grantSaving, setGrantSaving] = useState(false);
  const [controlling, setControlling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handlePick = async () => {
    setError(null);
    try {
      const picked = await invoke<string | null>("pick_executable");
      if (picked === null) return;
      setCommand(picked);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    try {
      await onConfigChange(sidecar.name, {
        command,
        args: args === "" ? [] : args.split("\n"),
        port: Number(port),
        replicas: Number(replicas),
      });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const handleGrant = async (next: boolean) => {
    setGrantSaving(true);
    setError(null);
    try {
      await onGrantChange(sidecar.name, next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setGrantSaving(false);
    }
  };

  const handleControl = async (action: "start" | "stop" | "restart") => {
    setControlling(true);
    setError(null);
    try {
      await onControl(sidecar.name, action);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setControlling(false);
    }
  };

  return (
    <fieldset className="sidecar-card">
      <legend>{sidecar.name}</legend>
      <p className="sidecar-reason">{sidecar.reason}</p>

      <label className="form-row">
        <span>実行ファイル</span>
        <input
          aria-label="実行ファイル"
          type="text"
          value={command}
          onChange={(e) => setCommand(e.target.value)}
          disabled={saving}
        />
      </label>
      {isTauri() && (
        <button type="button" onClick={handlePick} disabled={saving}>
          選択…
        </button>
      )}

      <label className="form-row">
        <span>引数(1 行 1 引数)</span>
        <textarea
          aria-label="引数"
          value={args}
          onChange={(e) => setArgs(e.target.value)}
          disabled={saving}
        />
      </label>

      <label className="form-row">
        <span>ポート</span>
        <input
          aria-label="ポート"
          type="number"
          value={port}
          onChange={(e) => setPort(e.target.value)}
          disabled={saving}
        />
      </label>

      {sidecar.scalable && (
        <label className="form-row">
          <span>レプリカ数</span>
          <input
            aria-label="レプリカ数"
            type="number"
            value={replicas}
            onChange={(e) => setReplicas(e.target.value)}
            disabled={saving}
          />
        </label>
      )}

      <button type="button" onClick={handleSave} disabled={saving}>
        保存
      </button>

      <label className="form-row sidecar-grant-toggle">
        <span>このサイドカーを承認する</span>
        {/*
          `CapabilitySection` と同じ理由で、`checked` はサーバから返った
          `sidecar.granted` のみで駆動する(楽観的更新をしない)。
          RPC が返らないまま「承認済み」に見えるのを防ぐため。
        */}
        <input
          type="checkbox"
          aria-label="このサイドカーを承認する"
          checked={sidecar.granted}
          // `checked` と同じ理由で、`disabled` もローカルの未保存入力
          // (`command` state)ではなくサーバが確認済みの `sidecar.config.command`
          // で判定する。保存前の入力だけでトグルが有効になると、承認した対象
          // (サーバ側の command)と実際に走るプログラムがずれてしまう。
          disabled={sidecar.config.command === "" || grantSaving}
          onChange={(e) => handleGrant(e.target.checked)}
        />
        {grantSaving && (
          <span className="capability-pending" role="status">
            確認中…
          </span>
        )}
      </label>

      <p className="capability-warning">
        承認するとこのプラグインはあなたが指定したプログラムを実行できます。そのプログラムは
        edlr のサンドボックスの外で動きます。
      </p>
      <p className="capability-warning" data-testid="sidecar-network-warning">
        さらに、承認するとこのプラグインは{" "}
        <code>http://127.0.0.1:{assignPorts(sidecar.config).join(", 127.0.0.1:")}</code>{" "}
        への通信が(サイドカーを 1 つも起動していなくても)許可されます。このポート番号は
        プラグイン作者が指定した既定値で、上の「ポート」欄で変更できます。既にこのポートで
        別のプログラム(別プラグインのサイドカーやローカルの開発サーバなど)が待ち受けている
        場合、承認するとそのプログラムとも通信できてしまう点に注意してください。
      </p>

      {!sidecar.granted && (
        <p className="capability-notice">未承認 — このプラグインはプロセスを起動できません</p>
      )}
      {sidecar.staleGrant && (
        <p className="capability-warning">要求が変わったため再承認が必要です</p>
      )}

      <ul className="sidecar-instances">
        {sidecar.instances.map((inst) => (
          <li key={inst.index}>
            #{inst.index} :{inst.port}{" "}
            {inst.state === "running"
              ? "実行中"
              : `停止${inst.exitCode !== null ? `(終了コード ${inst.exitCode})` : ""}`}
          </li>
        ))}
      </ul>

      <div className="sidecar-controls">
        <button type="button" onClick={() => handleControl("start")} disabled={controlling}>
          起動
        </button>
        <button type="button" onClick={() => handleControl("stop")} disabled={controlling}>
          停止
        </button>
        <button type="button" onClick={() => handleControl("restart")} disabled={controlling}>
          再起動
        </button>
      </div>

      {error && <p className="form-error">{error}</p>}
    </fieldset>
  );
}

export default function SidecarSection({
  sidecars,
  onConfigChange,
  onGrantChange,
  onControl,
}: {
  sidecars: Sidecar[];
  onConfigChange: (name: string, config: SidecarConfig) => Promise<void>;
  onGrantChange: (name: string, granted: boolean) => Promise<void>;
  onControl: (name: string, action: "start" | "stop" | "restart") => Promise<void>;
}) {
  if (sidecars.length === 0) {
    return null;
  }

  return (
    <div className="sidecar-section">
      {sidecars.map((s) => (
        <SidecarCard
          key={s.name}
          sidecar={s}
          onConfigChange={onConfigChange}
          onGrantChange={onGrantChange}
          onControl={onControl}
        />
      ))}
    </div>
  );
}
