import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
// Plugins.test.tsx と同じ理由: async atom のキャッシュがテスト間に漏れないよう、
// store なし Provider でテストごとに新規 store を作る。
import { Provider } from "jotai";
import { beforeEach, expect, test, vi } from "vitest";
import type { DriverInfo, DriversList } from "../types/plugin";
import Drivers from "./Drivers";

const calls: Array<{ method: string; params: unknown }> = [];
let listImpl: () => Promise<DriversList> = () =>
  Promise.resolve({ driversDir: "/drivers", drivers: [] });
let setSidecarGrantImpl: (params: unknown) => Promise<{ sidecars: DriverInfo["sidecars"] }> = () =>
  Promise.resolve({ sidecars: [] });
let setFilesystemGrantImpl: (params: unknown) => Promise<{ roots: DriverInfo["filesystem"] }> = () =>
  Promise.resolve({ roots: [] });
let instances: Array<{ close: ReturnType<typeof vi.fn> }> = [];

vi.mock("../rpc", () => {
  return {
    RpcClient: class {
      close = vi.fn();
      constructor() {
        instances.push(this);
      }
      call(method: string, params?: unknown) {
        calls.push({ method, params });
        if (method === "drivers/list") return listImpl();
        if (method === "drivers/set-sidecar-grant") return setSidecarGrantImpl(params);
        if (method === "drivers/set-filesystem-grant") return setFilesystemGrantImpl(params);
        return Promise.reject(new Error(`unexpected method: ${method}`));
      }
      setDriverSettings(driverId: string, values: Record<string, unknown>) {
        calls.push({ method: "drivers/set-settings", params: { driver: driverId, values } });
        return Promise.resolve(values);
      }
    },
  };
});

vi.mock("../ws", () => ({
  defaultWsUrl: () => "ws://test/ws",
}));

function makeDriver(overrides: Partial<DriverInfo> = {}): DriverInfo {
  return {
    id: "ed-state",
    name: "ED State",
    version: "0.1.0",
    description: "状態を配るドライバ",
    topics: [{ name: "current-system", retain: true, description: "現在のシステム" }],
    settings: [],
    values: {},
    capabilities: { requests: [], granted: false, staleGrant: false },
    sidecars: [],
    filesystem: [],
    state: "running",
    layout: null,
    ...overrides,
  };
}

function makeSidecar(
  overrides: Partial<DriverInfo["sidecars"][number]> = {},
): DriverInfo["sidecars"][number] {
  return {
    name: "tts",
    reason: "音声合成エンジンをローカルで動かすため",
    args: ["--port", "{port}"],
    port: 50021,
    scalable: true,
    granted: false,
    staleGrant: false,
    config: { command: "/usr/bin/piper", args: ["--port", "{port}"], port: 50021, replicas: 1 },
    instances: [],
    ...overrides,
  };
}

function makeFilesystemRoot(
  overrides: Partial<DriverInfo["filesystem"][number]> = {},
): DriverInfo["filesystem"][number] {
  return {
    name: "exports",
    reason: "取得したデータを CSV で書き出すため",
    mode: "read-write",
    granted: false,
    staleGrant: false,
    config: { path: "/home/u/exports" },
    ...overrides,
  };
}

beforeEach(() => {
  calls.length = 0;
  instances = [];
  listImpl = () => Promise.resolve({ driversDir: "/drivers", drivers: [] });
  setSidecarGrantImpl = () => Promise.resolve({ sidecars: [] });
  setFilesystemGrantImpl = () => Promise.resolve({ roots: [] });
});

async function openPermissionWizard() {
  await userEvent.click(await screen.findByRole("button", { name: /権限を設定/ }));
}

test("shows loading then renders the driver list with topics", async () => {
  const driver = makeDriver();
  listImpl = () => Promise.resolve({ driversDir: "/drivers", drivers: [driver] });

  render(<Provider><Drivers /></Provider>);
  expect(screen.getByText(/読み込み/)).toBeInTheDocument();

  expect(await screen.findByRole("heading", { name: /ED State/ })).toBeInTheDocument();
  expect(screen.getByText("current-system")).toBeInTheDocument();
  expect(screen.getByText(/retain/i)).toBeInTheDocument();
});

test("shows an empty-state message including driversDir when there are no drivers", async () => {
  listImpl = () => Promise.resolve({ driversDir: "/opt/edlr/drivers", drivers: [] });

  render(<Provider><Drivers /></Provider>);

  expect(await screen.findByText(/\/opt\/edlr\/drivers/)).toBeInTheDocument();
});

test("shows an error message when drivers/list fails", async () => {
  listImpl = () => Promise.reject(new Error("connection refused"));

  render(<Provider><Drivers /></Provider>);

  expect(await screen.findByText(/connection refused/)).toBeInTheDocument();
});

test("shows the reason for a disabled driver", async () => {
  const driver = makeDriver({ state: "disabled", reason: "wasm load failed" });
  listImpl = () => Promise.resolve({ driversDir: "/drivers", drivers: [driver] });

  render(<Provider><Drivers /></Provider>);

  expect(await screen.findByText(/wasm load failed/)).toBeInTheDocument();
});

test("closes every RpcClient when the component unmounts", async () => {
  const driver = makeDriver();
  listImpl = () => Promise.resolve({ driversDir: "/drivers", drivers: [driver] });

  const { unmount } = render(<Provider><Drivers /></Provider>);
  await screen.findByRole("heading", { name: /ED State/ });

  unmount();

  // driverList$ の一覧取得用(fetch 完了時に close)と、ミューテーション用の 2 本
  expect(instances).toHaveLength(2);
  for (const instance of instances) {
    expect(instance.close).toHaveBeenCalledTimes(1);
  }
});

test("shows the sidecar section in the wizard for a driver that declares sidecars", async () => {
  const driver = makeDriver({ sidecars: [makeSidecar()] });
  listImpl = () => Promise.resolve({ driversDir: "/drivers", drivers: [driver] });

  render(<Provider><Drivers /></Provider>);
  await openPermissionWizard();

  expect(await screen.findByText(/音声合成エンジン/)).toBeInTheDocument();
});

test("toggling sidecar approval calls drivers/set-sidecar-grant with the right params", async () => {
  const driver = makeDriver({ sidecars: [makeSidecar()] });
  listImpl = () => Promise.resolve({ driversDir: "/drivers", drivers: [driver] });
  setSidecarGrantImpl = () => Promise.resolve({ sidecars: [makeSidecar({ granted: true })] });

  render(<Provider><Drivers /></Provider>);
  await openPermissionWizard();
  const toggle = await screen.findByRole("checkbox", { name: /このサイドカーを承認する/ });
  await userEvent.click(toggle);

  await waitFor(() => {
    const call = calls.find((c) => c.method === "drivers/set-sidecar-grant");
    expect(call?.params).toEqual({ driver: "ed-state", name: "tts", granted: true });
  });
  await waitFor(() => expect(toggle).toBeChecked());
});

test("shows the filesystem section in the wizard for a driver that declares filesystem roots", async () => {
  const driver = makeDriver({ filesystem: [makeFilesystemRoot()] });
  listImpl = () => Promise.resolve({ driversDir: "/drivers", drivers: [driver] });

  render(<Provider><Drivers /></Provider>);
  await openPermissionWizard();

  expect(await screen.findByText(/CSV で書き出すため/)).toBeInTheDocument();
});

test("toggling filesystem approval calls drivers/set-filesystem-grant with the right params", async () => {
  const driver = makeDriver({ filesystem: [makeFilesystemRoot()] });
  listImpl = () => Promise.resolve({ driversDir: "/drivers", drivers: [driver] });
  setFilesystemGrantImpl = () =>
    Promise.resolve({ roots: [makeFilesystemRoot({ granted: true })] });

  render(<Provider><Drivers /></Provider>);
  await openPermissionWizard();
  const toggle = await screen.findByRole("checkbox", {
    name: /このフォルダへのアクセスを承認する/,
  });
  await userEvent.click(toggle);

  await waitFor(() => {
    const call = calls.find((c) => c.method === "drivers/set-filesystem-grant");
    expect(call?.params).toEqual({ driver: "ed-state", name: "exports", granted: true });
  });
  await waitFor(() => expect(toggle).toBeChecked());
});
