import { render, screen } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import type { DriverInfo, DriversList } from "../types/plugin";
import DriversPage from "./Drivers";

// Mirrors `Plugins.test.tsx`'s container test: `DriversPage` is the
// `drivers/list`-fetching container (loading/ready/error), analogous to the
// default export of `Plugins.tsx`. It is exercised separately from the
// presentational `Drivers` component's tests in `Drivers.test.tsx` (which
// mock only the sidecar/filesystem mutation methods) because this container
// always eagerly creates an `RpcClient` on mount to call `listDrivers()`.
let listImpl: () => Promise<DriversList> = () =>
  Promise.resolve({ driversDir: "/drivers", drivers: [] });
let instances: Array<{ close: ReturnType<typeof vi.fn> }> = [];

vi.mock("../rpc", () => {
  return {
    RpcClient: class {
      close = vi.fn();
      constructor() {
        instances.push(this);
      }
      listDrivers() {
        return listImpl();
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

beforeEach(() => {
  instances = [];
  listImpl = () => Promise.resolve({ driversDir: "/drivers", drivers: [] });
});

test("shows loading then renders the driver list", async () => {
  const driver = makeDriver();
  listImpl = () => Promise.resolve({ driversDir: "/drivers", drivers: [driver] });

  render(<DriversPage />);
  expect(screen.getByText(/読み込み/)).toBeInTheDocument();

  expect(await screen.findByText("ED State")).toBeInTheDocument();
});

test("shows an empty-state message including driversDir when there are no drivers", async () => {
  listImpl = () => Promise.resolve({ driversDir: "/opt/edlr/drivers", drivers: [] });

  render(<DriversPage />);

  expect(await screen.findByText(/\/opt\/edlr\/drivers/)).toBeInTheDocument();
});

test("shows an error message when drivers/list fails", async () => {
  listImpl = () => Promise.reject(new Error("connection refused"));

  render(<DriversPage />);

  expect(await screen.findByText(/connection refused/)).toBeInTheDocument();
});

test("shows the reason for a disabled driver", async () => {
  const driver = makeDriver({ state: "disabled", reason: "wasm load failed" });
  listImpl = () => Promise.resolve({ driversDir: "/drivers", drivers: [driver] });

  render(<DriversPage />);

  expect(await screen.findByText(/wasm load failed/)).toBeInTheDocument();
});

test("closes the RpcClient when the component unmounts", async () => {
  const driver = makeDriver();
  listImpl = () => Promise.resolve({ driversDir: "/drivers", drivers: [driver] });

  const { unmount } = render(<DriversPage />);
  await screen.findByText("ED State");

  unmount();

  expect(instances).toHaveLength(1);
  expect(instances[0].close).toHaveBeenCalledTimes(1);
});
