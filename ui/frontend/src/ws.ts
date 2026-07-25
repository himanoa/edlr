import { useEffect, useRef, useState } from "react";
import type { LogEntry } from "./lib/filter";

export type WsMessage =
  | { type: "hello"; protocol: number }
  | { type: "event"; kind: "journal"; timestamp: string; event: string; raw: unknown }
  | { type: "event"; kind: "status"; raw: unknown };

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
  return null;
}

export function defaultWsUrl(): string {
  if (window.location.protocol.startsWith("http") && window.location.host) {
    const scheme = window.location.protocol === "https:" ? "wss" : "ws";
    return `${scheme}://${window.location.host}/ws`;
  }
  // Tauri(tauri://)やテスト環境では既定のデーモンアドレスに接続する
  return "ws://127.0.0.1:8137/ws";
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
