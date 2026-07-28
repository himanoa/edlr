import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, test, vi } from "vitest";
import type { PluginInfo } from "../types/plugin";
import PluginForm from "./PluginForm";

function makePlugin(overrides: Partial<PluginInfo> = {}): PluginInfo {
  return {
    id: "voice-notify",
    name: "Voice Notify",
    version: "1.0.0",
    description: "test plugin",
    state: "running",
    settings: [
      { type: "boolean", key: "enabled", label: "有効", default: true },
      { type: "string", key: "endpoint", label: "エンドポイント", default: "http://localhost" },
      { type: "number", key: "volume", label: "音量", default: 80 },
      { type: "select", key: "voice", label: "音声", default: "Amber", options: ["Amber", "Blue"] },
    ],
    values: { enabled: true, endpoint: "http://localhost", volume: 80, voice: "Amber" },
    capabilities: { requests: [], granted: false, staleGrant: false },
    sidecars: [],
    filesystem: [],
    bus: [],
    dashboard: [],
    ...overrides,
  };
}

test("renders a control per setting field for all 4 types", () => {
  const plugin = makePlugin();
  render(<PluginForm plugin={plugin} onChange={vi.fn()} />);
  for (const field of plugin.settings) {
    expect(screen.getByLabelText(field.label)).toBeInTheDocument();
  }
});

test("toggling a boolean calls onChange(key, value)", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  const plugin = makePlugin();
  render(<PluginForm plugin={plugin} onChange={onChange} />);
  await userEvent.click(screen.getByLabelText("有効"));
  expect(onChange).toHaveBeenCalledWith("enabled", false);
});

test("shows an error and reverts the value when onChange rejects (boolean)", async () => {
  const onChange = vi.fn().mockRejectedValue(new Error("save failed"));
  const plugin = makePlugin();
  render(<PluginForm plugin={plugin} onChange={onChange} />);

  const checkbox = screen.getByLabelText("有効") as HTMLInputElement;
  expect(checkbox.checked).toBe(true);

  await userEvent.click(checkbox);

  expect(await screen.findByText("save failed")).toBeInTheDocument();
  expect(checkbox.checked).toBe(true);
});

test("a boolean field still commits immediately on change", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  const plugin = makePlugin();
  render(<PluginForm plugin={plugin} onChange={onChange} />);

  await userEvent.click(screen.getByLabelText("有効"));

  expect(onChange).toHaveBeenCalledTimes(1);
  expect(onChange).toHaveBeenCalledWith("enabled", false);
});

test("typing in a string field does not call onChange until blur or Enter", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  const plugin = makePlugin();
  render(<PluginForm plugin={plugin} onChange={onChange} />);

  const input = screen.getByLabelText("エンドポイント") as HTMLInputElement;
  await userEvent.clear(input);
  await userEvent.type(input, "http://localhost:5000");

  expect(input.value).toBe("http://localhost:5000");
  expect(onChange).not.toHaveBeenCalled();
});

test("blurring a string field commits the draft once with the final value", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  const plugin = makePlugin();
  render(<PluginForm plugin={plugin} onChange={onChange} />);

  const input = screen.getByLabelText("エンドポイント") as HTMLInputElement;
  await userEvent.clear(input);
  await userEvent.type(input, "http://localhost:5000");
  await userEvent.tab();

  expect(onChange).toHaveBeenCalledTimes(1);
  expect(onChange).toHaveBeenCalledWith("endpoint", "http://localhost:5000");
});

test("pressing Enter in a string field commits the draft", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  const plugin = makePlugin();
  render(<PluginForm plugin={plugin} onChange={onChange} />);

  const input = screen.getByLabelText("エンドポイント") as HTMLInputElement;
  await userEvent.clear(input);
  await userEvent.type(input, "http://example.com{Enter}");

  expect(onChange).toHaveBeenCalledTimes(1);
  expect(onChange).toHaveBeenCalledWith("endpoint", "http://example.com");
});

test("a failed save on a string field surfaces the error and reverts the draft", async () => {
  const onChange = vi.fn().mockRejectedValue(new Error("save failed"));
  const plugin = makePlugin();
  render(<PluginForm plugin={plugin} onChange={onChange} />);

  const input = screen.getByLabelText("エンドポイント") as HTMLInputElement;
  await userEvent.clear(input);
  await userEvent.type(input, "http://bad{Enter}");

  expect(await screen.findByText("save failed")).toBeInTheDocument();
  expect(input.value).toBe("http://localhost");
});

test("blurring a string field without editing does not call onChange", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  const plugin = makePlugin();
  render(<PluginForm plugin={plugin} onChange={onChange} />);

  const input = screen.getByLabelText("エンドポイント") as HTMLInputElement;
  await userEvent.click(input);
  await userEvent.tab();

  expect(onChange).not.toHaveBeenCalled();
});
