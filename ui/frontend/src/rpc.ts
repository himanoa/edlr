import type {
  BusRequest,
  Capabilities,
  DashboardListEntry,
  DashboardWidget,
  DriversList,
} from "./types/plugin";

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

export class RpcClient {
  private ws: WebSocket;
  private nextId = 1;
  private pending = new Map<number, PendingCall>();
  private queue: string[] = [];
  private open = false;
  private closed = false;
  private readonly timeoutMs: number;

  constructor(url: string, timeoutMs: number = DEFAULT_TIMEOUT_MS) {
    this.timeoutMs = timeoutMs;
    this.ws = new WebSocket(url);
    this.ws.onopen = () => {
      this.open = true;
      for (const frame of this.queue) {
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
      // caller-initiated close() already rejected pending calls with "RpcClient closed"
      // before calling ws.close(); this handler only fires for remote/unexpected closes.
      if (this.closed) return;
      this.closed = true;
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
        this.queue.push(serialized);
      }
    });
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    this.rejectAllPending(new Error("RpcClient closed"));
    this.ws.close();
  }

  /** インストール済みドライバの一覧(`drivers/list`)を取得する。 */
  listDrivers(): Promise<DriversList> {
    return this.call<DriversList>("drivers/list");
  }

  /** ドライバの設定値を更新する(`drivers/set-settings`)。サーバが返した確定値を返す。 */
  setDriverSettings(
    driverId: string,
    values: Record<string, unknown>,
  ): Promise<Record<string, unknown>> {
    return this.call<Record<string, unknown>>("drivers/set-settings", {
      driver: driverId,
      values,
    });
  }

  /** ドライバの外部通信承認を更新する(`drivers/set-capabilities`)。 */
  setDriverCapabilities(driverId: string, granted: boolean): Promise<Capabilities> {
    return this.call<Capabilities>("drivers/set-capabilities", { driver: driverId, granted });
  }

  /**
   * プラグインとドライバ間の bus 接続承認を更新する(`plugins/set-bus-grant`)。
   * サーバはそのプラグインの `bus` 配列全体を返すので、呼び出し側は自分の
   * 更新レスポンスからそのまま一覧を差し替えられる(再度 `plugins/list` を
   * 呼ぶ必要がない)。
   */
  setBusGrant(pluginId: string, driver: string, granted: boolean): Promise<{ bus: BusRequest[] }> {
    return this.call<{ bus: BusRequest[] }>("plugins/set-bus-grant", {
      plugin: pluginId,
      driver,
      granted,
    });
  }

  /**
   * ダッシュボードウィジェットの表示承認を更新する
   * (`plugins/set-dashboard-grant`)。`setBusGrant` と同じ流儀で、サーバは
   * そのプラグインの `dashboard` 配列全体を返す。
   */
  setDashboardGrant(
    pluginId: string,
    widget: string,
    granted: boolean,
  ): Promise<{ dashboard: DashboardWidget[] }> {
    return this.call<{ dashboard: DashboardWidget[] }>("plugins/set-dashboard-grant", {
      plugin: pluginId,
      widget,
      granted,
    });
  }

  /** grant 済みダッシュボードウィジェットの一覧(`dashboard/list`)。 */
  listDashboard(): Promise<{ widgets: DashboardListEntry[] }> {
    return this.call<{ widgets: DashboardListEntry[] }>("dashboard/list");
  }

  private rejectAllPending(reason: unknown): void {
    for (const [id, pending] of this.pending) {
      clearTimeout(pending.timer);
      pending.reject(reason);
      this.pending.delete(id);
    }
  }
}
