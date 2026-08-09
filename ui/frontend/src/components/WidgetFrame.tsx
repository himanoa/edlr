import { useEffect, useRef, useState } from "react";
import { useAtomValue } from "jotai";
import type { DashboardListEntry } from "../types/plugin";
import type { LogEntry } from "../lib/filter";
import { matchesEvent } from "../lib/events";
import { rpcClient$ } from "@/store/rpcClient";
import { daemonHttpUrl } from "../ws";

const MIN_HEIGHT_PX = 120;
const MAX_HEIGHT_PX = 800;
const DEFAULT_HEIGHT_PX = 240;

/**
 * ダッシュボードウィジェット 1 件の iframe と postMessage ブリッジ。
 *
 * - iframe は sandbox="allow-scripts" のみ(opaque origin)。widget からは
 *   親 DOM にも WS にも触れず、通信は postMessage だけ。
 * - widget からの edlr:ready を受けてから edlr:init を送り、蓄積済み +
 *   以後の manifest `events` マッチ分を edlr:event で転送する
 *   (ready 前には何も送らない)。
 * - edlr:height で iframe の高さを合わせる(暴走防止のため上下限で
 *   クランプする)。
 * - message の検証は `e.source === iframe.contentWindow` で行う。opaque
 *   origin の iframe からのメッセージは origin が "null" になるため、
 *   origin ではなく source で相手を識別する。
 */
export function WidgetFrame({
  entry,
  entries,
}: {
  entry: DashboardListEntry;
  entries: LogEntry[];
}) {
  const iframeRef = useRef<HTMLIFrameElement | null>(null);
  const [ready, setReady] = useState(false);
  const [height, setHeight] = useState(DEFAULT_HEIGHT_PX);
  // 転送済み位置。ready 前に届いた分は ready 時にまとめて送る。
  const sentUpTo = useRef(0);
  const rpc = useAtomValue(rpcClient$);

  useEffect(() => {
    const onMessage = (e: MessageEvent) => {
      const win = iframeRef.current?.contentWindow;
      if (!win || e.source !== win) return;
      const msg = e.data as { type?: string; px?: number; name?: string } | null;
      if (msg?.type === "edlr:ready") {
        win.postMessage(
          { type: "edlr:init", plugin: entry.plugin, widget: entry.widget },
          "*",
        );
        setReady(true);
      } else if (msg?.type === "edlr:height" && typeof msg.px === "number") {
        setHeight(Math.min(MAX_HEIGHT_PX, Math.max(MIN_HEIGHT_PX, msg.px)));
      } else if (msg?.type === "edlr:action" && typeof msg.name === "string") {
        // plugin/widget は iframe の自己申告ではなく、この iframe を作った
        // ときの値を使う(他プラグイン宛にはならない)。配送は
        // fire-and-forget で、失敗(未 grant・プラグイン停止・キュー満杯)は
        // console に残すだけ — widget への結果通知経路は v1 スコープ外。
        rpc?.dashboardAction(entry.plugin, entry.widget, msg.name).catch((err) => {
          console.error(
            `dashboard action ${entry.plugin}/${entry.widget}/${msg.name} failed:`,
            err,
          );
        });
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, [entry.plugin, entry.widget, rpc]);

  useEffect(() => {
    if (!ready) return;
    const win = iframeRef.current?.contentWindow;
    if (!win) return;
    for (const log of entries.slice(sentUpTo.current)) {
      if (matchesEvent(entry.events, log)) {
        win.postMessage({ type: "edlr:event", event: log }, "*");
      }
    }
    sentUpTo.current = entries.length;
  }, [ready, entries, entry.events]);

  return (
    <iframe
      ref={iframeRef}
      title={`${entry.plugin}/${entry.widget}`}
      src={daemonHttpUrl(entry.url)}
      sandbox="allow-scripts"
      className="w-full border-none"
      style={{ height }}
    />
  );
}

export default WidgetFrame;
