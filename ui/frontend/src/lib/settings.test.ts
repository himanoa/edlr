import { mockPlugins } from "../mock/plugins";
import { loadSettings, saveSettings } from "./settings";

beforeEach(() => localStorage.clear());

test("returns defaults when nothing is stored", () => {
  const manifest = mockPlugins[0];
  const values = loadSettings(manifest);
  for (const field of manifest.settings) {
    expect(values[field.key]).toEqual(field.default);
  }
});

test("stored values override defaults and survive reload", () => {
  const manifest = mockPlugins[0];
  saveSettings(manifest.id, { volume: 30 });
  const values = loadSettings(manifest);
  expect(values.volume).toBe(30);
  expect(values.enabled).toBe(true); // 未保存のキーは default
});

test("broken stored JSON falls back to defaults", () => {
  const manifest = mockPlugins[0];
  localStorage.setItem(`edlr.plugin-settings.${manifest.id}`, "{broken");
  expect(loadSettings(manifest).enabled).toBe(true);
});
