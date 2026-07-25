import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockPlugins } from "../mock/plugins";
import { loadSettings } from "../lib/settings";
import PluginForm from "./PluginForm";

beforeEach(() => localStorage.clear());

test("renders a control per setting field", () => {
  const manifest = mockPlugins[0];
  render(<PluginForm manifest={manifest} />);
  for (const field of manifest.settings) {
    expect(screen.getByLabelText(field.label)).toBeInTheDocument();
  }
});

test("changing a boolean persists to localStorage", async () => {
  const manifest = mockPlugins[0]; // enabled: default true
  render(<PluginForm manifest={manifest} />);
  await userEvent.click(screen.getByLabelText("有効"));
  expect(loadSettings(manifest).enabled).toBe(false);
});

test("changing a number persists to localStorage", async () => {
  const manifest = mockPlugins[0];
  render(<PluginForm manifest={manifest} />);
  const volume = screen.getByLabelText("音量");
  await userEvent.clear(volume);
  await userEvent.type(volume, "42");
  expect(loadSettings(manifest).volume).toBe(42);
});
