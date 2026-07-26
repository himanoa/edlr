import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { PluginInfo, PluginsList } from "../types/plugin";
import Plugins from "./Plugins";

const calls: Array<{ method: string; params: unknown }> = [];
let listImpl: () => Promise<PluginsList> = () =>
  Promise.resolve({ pluginsDir: "/plugins", plugins: [] });
let setSettingsImpl: (params: unknown) => Promise<Record<string, unknown>> = () =>
  Promise.resolve({});
let setCapabilitiesImpl: (params: unknown) => Promise<PluginInfo["capabilities"]> = () =>
  Promise.resolve({ requests: [], granted: false, staleGrant: false });
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
        if (method === "plugins/list") return listImpl();
        if (method === "plugins/set-settings") return setSettingsImpl(params);
        if (method === "plugins/set-capabilities") return setCapabilitiesImpl(params);
        return Promise.reject(new Error(`unexpected method: ${method}`));
      }
    },
  };
});

vi.mock("../ws", () => ({
  defaultWsUrl: () => "ws://test/ws",
}));

function makePlugin(overrides: Partial<PluginInfo> = {}): PluginInfo {
  return {
    id: "voice-notify",
    name: "Voice Notify",
    version: "1.0.0",
    description: "音声で通知する",
    state: "running",
    settings: [{ type: "boolean", key: "enabled", label: "有効", default: true }],
    values: { enabled: true },
    capabilities: { requests: [], granted: false, staleGrant: false },
    ...overrides,
  };
}

beforeEach(() => {
  calls.length = 0;
  instances = [];
  listImpl = () => Promise.resolve({ pluginsDir: "/plugins", plugins: [] });
  setSettingsImpl = () => Promise.resolve({});
  setCapabilitiesImpl = () => Promise.resolve({ requests: [], granted: false, staleGrant: false });
});

afterEach(() => {
  vi.restoreAllMocks();
});

test("shows loading then renders the plugin list", async () => {
  const plugin = makePlugin();
  listImpl = () => Promise.resolve({ pluginsDir: "/plugins", plugins: [plugin] });

  render(<Plugins />);
  expect(screen.getByText(/読み込み/)).toBeInTheDocument();

  expect(await screen.findByText("Voice Notify")).toBeInTheDocument();
});

test("shows an empty-state message including pluginsDir when there are no plugins", async () => {
  listImpl = () => Promise.resolve({ pluginsDir: "/opt/edlr/plugins", plugins: [] });

  render(<Plugins />);

  expect(await screen.findByText(/\/opt\/edlr\/plugins/)).toBeInTheDocument();
});

test("shows an error message when plugins/list fails", async () => {
  listImpl = () => Promise.reject(new Error("connection refused"));

  render(<Plugins />);

  expect(await screen.findByText(/connection refused/)).toBeInTheDocument();
});

test("shows the reason for a disabled plugin", async () => {
  const plugin = makePlugin({ state: "disabled", reason: "wasm load failed" });
  listImpl = () => Promise.resolve({ pluginsDir: "/plugins", plugins: [plugin] });

  render(<Plugins />);

  expect(await screen.findByText(/wasm load failed/)).toBeInTheDocument();
});

test("closes the RpcClient when the component unmounts", async () => {
  const plugin = makePlugin();
  listImpl = () => Promise.resolve({ pluginsDir: "/plugins", plugins: [plugin] });

  const { unmount } = render(<Plugins />);
  await screen.findByText("Voice Notify");

  unmount();

  expect(instances).toHaveLength(1);
  expect(instances[0].close).toHaveBeenCalledTimes(1);
});

test("changing a setting calls plugins/set-settings with the right args and updates displayed values", async () => {
  const plugin = makePlugin();
  listImpl = () => Promise.resolve({ pluginsDir: "/plugins", plugins: [plugin] });
  setSettingsImpl = () => Promise.resolve({ enabled: false });

  render(<Plugins />);
  const checkbox = (await screen.findByLabelText("有効")) as HTMLInputElement;
  await userEvent.click(checkbox);

  await waitFor(() => {
    const setSettingsCall = calls.find((c) => c.method === "plugins/set-settings");
    expect(setSettingsCall?.params).toEqual({
      plugin: "voice-notify",
      values: { enabled: false },
    });
  });

  await waitFor(() => expect(checkbox.checked).toBe(false));
});

test("shows the capability section for a plugin that has capability requests", async () => {
  const plugin = makePlugin({
    capabilities: {
      requests: [{ kind: "http", hosts: ["api.example.com"], reason: "天気を取得するため" }],
      granted: false,
      staleGrant: false,
    },
  });
  listImpl = () => Promise.resolve({ pluginsDir: "/plugins", plugins: [plugin] });

  render(<Plugins />);

  expect(await screen.findByText(/api\.example\.com/)).toBeInTheDocument();
});

test("toggling capability approval calls plugins/set-capabilities and updates the display", async () => {
  const plugin = makePlugin({
    capabilities: {
      requests: [{ kind: "http", hosts: ["api.example.com"], reason: "天気を取得するため" }],
      granted: false,
      staleGrant: false,
    },
  });
  listImpl = () => Promise.resolve({ pluginsDir: "/plugins", plugins: [plugin] });
  setCapabilitiesImpl = () =>
    Promise.resolve({
      requests: [{ kind: "http", hosts: ["api.example.com"], reason: "天気を取得するため" }],
      granted: true,
      staleGrant: false,
    });

  render(<Plugins />);
  const toggle = (await screen.findByRole("checkbox", { name: /承認/ })) as HTMLInputElement;
  await userEvent.click(toggle);

  await waitFor(() => {
    const call = calls.find((c) => c.method === "plugins/set-capabilities");
    expect(call?.params).toEqual({ plugin: "voice-notify", granted: true });
  });

  await waitFor(() => expect(toggle.checked).toBe(true));
  expect(screen.queryByText(/未承認/)).not.toBeInTheDocument();
});
