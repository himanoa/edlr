import { useEffect, useRef, useState } from "react";
import { useAtomValue } from "jotai";
import type { DashboardListEntry } from "../types/plugin";
import type { LogEntry } from "../lib/filter";
import { matchesEvent } from "../lib/events";
import { rpcClient$ } from "@/store/rpcClient";
import { daemonHttpUrl } from "../ws";

/** プラグインウィジェットの mount に渡す API。 */
export interface WidgetApi {
  plugin: string;
  widget: string;
  /**
   * プラグインへのアクション要求。plugins/dashboard-action RPC に変換され、
   * on-message(driver="dashboard", topic=name) へ届く。fire-and-forget で、
   * 失敗(未 grant・プラグイン停止・キュー満杯)は console に残すだけ。
   */
  action(name: string): void;
  /**
   * manifest `events` にマッチしたイベントの購読。mount 中に同期的に登録する
   * こと(mount 完了時点の登録リスナーへ、mount 前の蓄積分から配り始める)。
   */
  onEvent(cb: (ev: LogEntry) => void): void;
  /**
   * bus フレームの購読。全 driver/topic が届くのでウィジェット側でフィルタ
   * する(manifest 宣言はしない -- 設計書の承認モデル参照)。mount 中に
   * 同期的に登録すること(onEvent と同じ)。
   */
  onBus(cb: (msg: { driver: string; topic: string; payload: string }) => void): void;
  /** retained 値の取得。未保持なら null、RPC 失敗は reject。 */
  retained(driver: string, topic: string): Promise<string | null>;
}

export interface WidgetModule {
  default: (el: HTMLElement, api: WidgetApi) => (() => void) | void;
}

const defaultLoad = (url: string): Promise<WidgetModule> =>
  import(/* @vite-ignore */ url);

/**
 * ダッシュボードウィジェット 1 件のホスト。
 *
 * デーモンが配信する ESM モジュールを dynamic import し、`mount(el, api)` で
 * この DOM に直接マウントする。プラグインは信頼済みインストール物なので
 * サンドボックスは張らない(旧 iframe + postMessage 方式は廃止)。
 * ホストと同一 DOM のため、CSS 変数などホストのスタイルがそのまま当たる。
 */
export function WidgetHost({
  entry,
  entries,
  load = defaultLoad,
}: {
  entry: DashboardListEntry;
  entries: LogEntry[];
  load?: (url: string) => Promise<WidgetModule>;
}) {
  const elRef = useRef<HTMLDivElement | null>(null);
  const listeners = useRef<Array<(ev: LogEntry) => void>>([]);
  const busListeners = useRef<
    Array<(msg: { driver: string; topic: string; payload: string }) => void>
  >([]);
  // 配送済み位置。mount 前は進めず、mount 時に蓄積分をまとめて配る。
  const sentUpTo = useRef(0);
  const [mounted, setMounted] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const rpc = useAtomValue(rpcClient$);
  const rpcRef = useRef(rpc);
  rpcRef.current = rpc;

  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    let cleanup: (() => void) | void;
    let cancelled = false;
    const api: WidgetApi = {
      plugin: entry.plugin,
      widget: entry.widget,
      action(name) {
        rpcRef.current
          ?.dashboardAction(entry.plugin, entry.widget, String(name))
          .catch((err) => {
            console.error(
              `dashboard action ${entry.plugin}/${entry.widget}/${name} failed:`,
              err,
            );
          });
      },
      onEvent(cb) {
        listeners.current.push(cb);
      },
      onBus(cb) {
        busListeners.current.push(cb);
      },
      retained(driver, topic) {
        const rpc = rpcRef.current;
        if (!rpc) return Promise.reject(new Error("rpc unavailable"));
        return rpc.busRetained(driver, topic);
      },
    };
    load(daemonHttpUrl(entry.url))
      .then((mod) => {
        if (cancelled) return;
        cleanup = mod.default(el, api);
        setMounted(true);
      })
      .catch((err) => {
        if (cancelled) return;
        console.error(`widget ${entry.plugin}/${entry.widget} mount failed:`, err);
        setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
      cleanup?.();
      listeners.current = [];
      busListeners.current = [];
      setMounted(false);
      sentUpTo.current = 0;
      el.replaceChildren();
    };
    // load はテスト DI 用で、レンダー間で安定な前提のため依存に含めない
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [entry.plugin, entry.widget, entry.url]);

  useEffect(() => {
    if (!mounted) return;
    for (const log of entries.slice(sentUpTo.current)) {
      if (log.kind === "bus") {
        const msg = { driver: log.driver ?? "", topic: log.topic ?? "", payload: log.payload ?? "" };
        for (const cb of busListeners.current) {
          try {
            cb(msg);
          } catch (err) {
            // リスナーの例外で他のリスナーを止めない(onEvent と同じ)
            console.error(`widget ${entry.plugin}/${entry.widget} bus listener failed:`, err);
          }
        }
        continue;
      }
      if (!matchesEvent(entry.events, log)) continue;
      for (const cb of listeners.current) {
        try {
          cb(log);
        } catch (err) {
          // リスナーの例外で他のリスナーを止めない(旧 SDK と同じ)
          console.error(`widget ${entry.plugin}/${entry.widget} listener failed:`, err);
        }
      }
    }
    sentUpTo.current = entries.length;
  }, [mounted, entries, entry.events, entry.plugin, entry.widget]);

  if (error) {
    return (
      <p className="text-sm text-destructive">
        ウィジェットの読み込みに失敗しました: {error}
      </p>
    );
  }
  return <div ref={elRef} className="h-full min-h-0 overflow-auto" />;
}

export default WidgetHost;
