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
