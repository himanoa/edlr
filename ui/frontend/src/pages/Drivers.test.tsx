import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DriverInfo } from "../types/plugin";
import { Drivers } from "./Drivers";

// `Drivers` creates its `RpcClient` lazily (only once a mutation handler
// actually fires), so these tests can mock "../rpc" the same way
// `Plugins.test.tsx` does for the equivalent sidecar/filesystem sections,
// without every render in this file opening a connection.
const calls: Array<{ method: string; params: unknown }> = [];
let setSidecarGrantImpl: (params: unknown) => Promise<{ sidecars: DriverInfo["sidecars"] }> = () =>
  Promise.resolve({ sidecars: [] });
let setFilesystemGrantImpl: (params: unknown) => Promise<{ roots: DriverInfo["filesystem"] }> = () =>
  Promise.resolve({ roots: [] });

vi.mock("../rpc", () => {
  return {
    RpcClient: class {
      close = vi.fn();
      call(method: string, params?: unknown) {
        calls.push({ method, params });
        if (method === "drivers/set-sidecar-grant") return setSidecarGrantImpl(params);
        if (method === "drivers/set-filesystem-grant") return setFilesystemGrantImpl(params);
        return Promise.reject(new Error(`unexpected method: ${method}`));
      }
    },
  };
});

vi.mock("../ws", () => ({
  defaultWsUrl: () => "ws://test/ws",
}));

beforeEach(() => {
  calls.length = 0;
  setSidecarGrantImpl = () => Promise.resolve({ sidecars: [] });
  setFilesystemGrantImpl = () => Promise.resolve({ roots: [] });
});

const driver = {
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
  state: "running" as const,
  layout: null,
};

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

describe("Drivers page", () => {
  it("lists installed drivers with their topics", () => {
    render(<Drivers drivers={[driver]} driversDir="/home/u/.config/edlr/drivers" onReload={vi.fn()} />);
    expect(screen.getByText("ED State")).toBeInTheDocument();
    expect(screen.getByText("current-system")).toBeInTheDocument();
    expect(screen.getByText(/retain/i)).toBeInTheDocument();
  });

  it("shows an empty state with the drivers dir", () => {
    render(<Drivers drivers={[]} driversDir="/home/u/.config/edlr/drivers" onReload={vi.fn()} />);
    expect(screen.getByText(/\/home\/u\/\.config\/edlr\/drivers/)).toBeInTheDocument();
  });

  it("shows the disabled reason", () => {
    render(
      <Drivers
        drivers={[{ ...driver, state: "disabled" as const, reason: "on-message call failed" }]}
        driversDir="/d"
        onReload={vi.fn()}
      />,
    );
    expect(screen.getByText(/on-message call failed/)).toBeInTheDocument();
  });

  it("shows the sidecar section for a driver that declares sidecars", () => {
    const withSidecar = { ...driver, sidecars: [makeSidecar()] };
    render(<Drivers drivers={[withSidecar]} driversDir="/d" onReload={vi.fn()} />);
    expect(screen.getByText(/音声合成エンジン/)).toBeInTheDocument();
  });

  it("toggling sidecar approval calls drivers/set-sidecar-grant with the right params", async () => {
    const withSidecar = { ...driver, sidecars: [makeSidecar()] };
    render(<Drivers drivers={[withSidecar]} driversDir="/d" onReload={vi.fn()} />);

    const toggle = (await screen.findByRole("checkbox", {
      name: /このサイドカーを承認する/,
    })) as HTMLInputElement;
    await userEvent.click(toggle);

    await waitFor(() => {
      const call = calls.find((c) => c.method === "drivers/set-sidecar-grant");
      expect(call?.params).toEqual({ driver: "ed-state", name: "tts", granted: true });
    });
  });

  it("shows the filesystem section for a driver that declares filesystem roots", () => {
    const withRoot = { ...driver, filesystem: [makeFilesystemRoot()] };
    render(<Drivers drivers={[withRoot]} driversDir="/d" onReload={vi.fn()} />);
    expect(screen.getByText(/CSV で書き出すため/)).toBeInTheDocument();
  });

  it("toggling filesystem approval calls drivers/set-filesystem-grant with the right params", async () => {
    const withRoot = { ...driver, filesystem: [makeFilesystemRoot()] };
    render(<Drivers drivers={[withRoot]} driversDir="/d" onReload={vi.fn()} />);

    const toggle = (await screen.findByRole("checkbox", {
      name: /このフォルダへのアクセスを承認する/,
    })) as HTMLInputElement;
    await userEvent.click(toggle);

    await waitFor(() => {
      const call = calls.find((c) => c.method === "drivers/set-filesystem-grant");
      expect(call?.params).toEqual({ driver: "ed-state", name: "exports", granted: true });
    });
  });
});
