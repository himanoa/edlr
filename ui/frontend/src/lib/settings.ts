import type { PluginManifest } from "../mock/plugins";

const KEY_PREFIX = "edlr.plugin-settings.";

export function loadSettings(manifest: PluginManifest): Record<string, unknown> {
  const defaults: Record<string, unknown> = {};
  for (const field of manifest.settings) {
    defaults[field.key] = field.default;
  }
  const stored = localStorage.getItem(KEY_PREFIX + manifest.id);
  if (!stored) return defaults;
  try {
    const parsed = JSON.parse(stored);
    if (typeof parsed !== "object" || parsed === null) return defaults;
    return { ...defaults, ...(parsed as Record<string, unknown>) };
  } catch {
    return defaults;
  }
}

export function saveSettings(pluginId: string, values: Record<string, unknown>): void {
  const key = KEY_PREFIX + pluginId;
  const stored = localStorage.getItem(key);
  let current: Record<string, unknown> = {};
  if (stored) {
    try {
      const parsed = JSON.parse(stored);
      if (typeof parsed === "object" && parsed !== null) {
        current = parsed as Record<string, unknown>;
      }
    } catch {
      // 壊れた保存値は捨てて上書きする
    }
  }
  localStorage.setItem(key, JSON.stringify({ ...current, ...values }));
}
