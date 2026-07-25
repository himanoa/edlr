const DEFAULT_TIMEOUT_MS = 5000;

export type RpcResponse =
  | { type: "rpc-result"; id: number; result: unknown }
  | { type: "rpc-error"; id: number; error: string };

/** サーバからの 1 メッセージを RPC 応答としてパースする。RPC 応答でなければ null。 */
export function parseRpcResponse(data: string): RpcResponse | null {
  let value: unknown;
  try {
    value = JSON.parse(data);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null) return null;
  const msg = value as Record<string, unknown>;
  if (msg.type === "rpc-result" && typeof msg.id === "number") {
    return { type: "rpc-result", id: msg.id, result: msg.result };
  }
  if (msg.type === "rpc-error" && typeof msg.id === "number" && typeof msg.error === "string") {
    return { type: "rpc-error", id: msg.id, error: msg.error };
  }
  return null;
}

type PendingCall = {
  resolve: (value: unknown) => void;
  reject: (reason: unknown) => void;
  timer: ReturnType<typeof setTimeout>;
};

type QueuedFrame = {
  frame: string;
};

export class RpcClient {
  private ws: WebSocket;
  private nextId = 1;
  private pending = new Map<number, PendingCall>();
  private queue: QueuedFrame[] = [];
  private open = false;
  private closed = false;
  private readonly timeoutMs: number;

  constructor(url: string, timeoutMs: number = DEFAULT_TIMEOUT_MS) {
    this.timeoutMs = timeoutMs;
    this.ws = new WebSocket(url);
    this.ws.onopen = () => {
      this.open = true;
      for (const { frame } of this.queue) {
        this.ws.send(frame);
      }
      this.queue = [];
    };
    this.ws.onmessage = (event: { data: unknown }) => {
      const response = parseRpcResponse(String(event.data));
      if (!response) return;
      const pending = this.pending.get(response.id);
      if (!pending) return;
      this.pending.delete(response.id);
      clearTimeout(pending.timer);
      if (response.type === "rpc-result") {
        pending.resolve(response.result);
      } else {
        pending.reject(new Error(response.error));
      }
    };
    this.ws.onclose = () => {
      this.rejectAllPending(new Error("WebSocket closed"));
    };
  }

  call<T = unknown>(method: string, params?: unknown): Promise<T> {
    if (this.closed) {
      return Promise.reject(new Error("RpcClient is closed"));
    }
    const id = this.nextId++;
    const frame: Record<string, unknown> = { type: "rpc", id, method };
    if (params !== undefined) frame.params = params;
    const serialized = JSON.stringify(frame);

    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`RPC call "${method}" timed out after ${this.timeoutMs}ms`));
      }, this.timeoutMs);

      this.pending.set(id, {
        resolve: resolve as (value: unknown) => void,
        reject,
        timer,
      });

      if (this.open) {
        this.ws.send(serialized);
      } else {
        this.queue.push({ frame: serialized });
      }
    });
  }

  close(): void {
    this.closed = true;
    this.ws.close();
    this.rejectAllPending(new Error("RpcClient closed"));
  }

  private rejectAllPending(reason: unknown): void {
    for (const [id, pending] of this.pending) {
      clearTimeout(pending.timer);
      pending.reject(reason);
      this.pending.delete(id);
    }
  }
}
