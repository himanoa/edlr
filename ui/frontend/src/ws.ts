import { useEffect, useRef, useState } from "react";
import type { LogEntry } from "./lib/filter";

export type WsMessage =
  | { type: "hello"; protocol: number }
  | { type: "event"; kind: "journal"; timestamp: string; event: string; raw: unknown }
  | { type: "event"; kind: "status"; raw: unknown }
  | {
      type: "event";
      kind: "log";
      timestamp: string;
      level: string;
      target?: string;
      message: string;
    }
  | { type: "event"; kind: "bus"; driver: string; topic: string; payload: string };

export type ConnectionState = "connecting" | "open" | "closed";

const CLIENT_BUFFER_LIMIT = 2000;
const RECONNECT_DELAY_MS = 1000;

export function parseWsMessage(data: string): WsMessage | null {
  let value: unknown;
  try {
    value = JSON.parse(data);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null) return null;
  const msg = value as Record<string, unknown>;
  if (msg.type === "hello" && typeof msg.protocol === "number") {
    return { type: "hello", protocol: msg.protocol };
  }
  if (msg.type === "event" && msg.kind === "journal") {
    if (typeof msg.timestamp === "string" && typeof msg.event === "string") {
      return { type: "event", kind: "journal", timestamp: msg.timestamp, event: msg.event, raw: msg.raw };
    }
    return null;
  }
  if (msg.type === "event" && msg.kind === "status") {
    return { type: "event", kind: "status", raw: msg.raw };
  }
  if (msg.type === "event" && msg.kind === "log") {
    if (
      typeof msg.timestamp === "string" &&
      typeof msg.level === "string" &&
      typeof msg.message === "string"
    ) {
      return {
        type: "event",
        kind: "log",
        timestamp: msg.timestamp,
        level: msg.level,
        ...(typeof msg.target === "string" ? { target: msg.target } : {}),
        message: msg.message,
      };
    }
    return null;
  }
  if (msg.type === "event" && msg.kind === "bus") {
    if (
      typeof msg.driver === "string" &&
      typeof msg.topic === "string" &&
      typeof msg.payload === "string"
    ) {
      return { type: "event", kind: "bus", driver: msg.driver, topic: msg.topic, payload: msg.payload };
    }
    return null;
  }
  return null;
}

const DAEMON_DEFAULT_WS_URL = "ws://127.0.0.1:8137/ws";

export type LocationLike = {
  protocol: string;
  host: string;
  hostname: string;
};

function isTauriRuntime(loc: LocationLike): boolean {
  if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) return true;
  if (loc.hostname === "tauri.localhost") return true;
  if (loc.protocol === "tauri:") return true;
  return false;
}

export function defaultWsUrl(loc: LocationLike = window.location): string {
  if (isTauriRuntime(loc)) {
    // Tauri シェル(tauri:// または tauri.localhost)では常にデーモンの既定アドレスへ接続する
    return DAEMON_DEFAULT_WS_URL;
  }
  if (loc.protocol.startsWith("http") && loc.host) {
    const scheme = loc.protocol === "https:" ? "wss" : "ws";
    return `${scheme}://${loc.host}/ws`;
  }
  // それ以外(non-http プロトコルやテスト環境)では既定のデーモンアドレスに接続する
  return DAEMON_DEFAULT_WS_URL;
}

/**
 * デーモン HTTP の絶対 URL。`defaultWsUrl` と同じ判定で origin を決めるため、
 * Tauri シェル(origin が tauri://)でも相対パスのままにならない。
 */
export function daemonHttpUrl(path: string, loc: LocationLike = window.location): string {
  const origin = defaultWsUrl(loc).replace(/^ws/, "http").replace(/\/ws$/, "");
  return origin + path;
}

export function useEventStream(url: string): {
  entries: LogEntry[];
  connection: ConnectionState;
} {
  const [entries, setEntries] = useState<LogEntry[]>([]);
  const [connection, setConnection] = useState<ConnectionState>("connecting");
  const nextId = useRef(1);

  useEffect(() => {
    let ws: WebSocket | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let disposed = false;

    const connect = () => {
      setConnection("connecting");
      ws = new WebSocket(url);
      ws.onopen = () => setConnection("open");
      ws.onmessage = (e) => {
        const msg = parseWsMessage(String(e.data));
        if (!msg || msg.type !== "event") return;
        const entry: LogEntry =
          msg.kind === "journal"
            ? { id: nextId.current++, kind: "journal", timestamp: msg.timestamp, event: msg.event, raw: msg.raw }
            : msg.kind === "log"
              ? {
                  id: nextId.current++,
                  kind: "log",
                  timestamp: msg.timestamp,
                  level: msg.level,
                  target: msg.target,
                  message: msg.message,
                  // raw 展開(クリック時)にも同じ内容を出す
                  raw: { level: msg.level, target: msg.target, message: msg.message },
                }
              : msg.kind === "bus"
                ? {
                    id: nextId.current++,
                    kind: "bus",
                    driver: msg.driver,
                    topic: msg.topic,
                    payload: msg.payload,
                    // Logs の行表示(entry.event)と検索がそのまま効く形にする
                    event: `${msg.driver}/${msg.topic}`,
                    // raw 展開(クリック時)で payload を読めるように
                    raw: { driver: msg.driver, topic: msg.topic, payload: msg.payload },
                  }
                : { id: nextId.current++, kind: "status", raw: msg.raw };
        setEntries((prev) => {
          const next = [...prev, entry];
          return next.length > CLIENT_BUFFER_LIMIT
            ? next.slice(next.length - CLIENT_BUFFER_LIMIT)
            : next;
        });
      };
      ws.onclose = () => {
        setConnection("closed");
        if (!disposed) timer = setTimeout(connect, RECONNECT_DELAY_MS);
      };
      ws.onerror = () => ws?.close();
    };

    connect();
    return () => {
      disposed = true;
      if (timer) clearTimeout(timer);
      ws?.close();
    };
  }, [url]);

  return { entries, connection };
}
