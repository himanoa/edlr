import { useEffect, useState } from "react";
import type { PluginInfo, SettingField } from "../types/plugin";

function mergedValues(plugin: PluginInfo): Record<string, unknown> {
  const defaults: Record<string, unknown> = {};
  for (const field of plugin.settings) {
    defaults[field.key] = field.default;
  }
  return { ...defaults, ...plugin.values };
}

function Field({
  field,
  value,
  disabled,
  onChange,
}: {
  field: SettingField;
  value: unknown;
  disabled: boolean;
  onChange: (v: unknown) => void;
}) {
  const id = `field-${field.key}`;
  switch (field.type) {
    case "boolean":
      return (
        <label htmlFor={id} className="form-row">
          <span>{field.label}</span>
          <input
            id={id}
            type="checkbox"
            checked={Boolean(value)}
            disabled={disabled}
            onChange={(e) => onChange(e.target.checked)}
          />
        </label>
      );
    case "string":
      return (
        <label htmlFor={id} className="form-row">
          <span>{field.label}</span>
          <input
            id={id}
            type="text"
            value={String(value ?? "")}
            disabled={disabled}
            onChange={(e) => onChange(e.target.value)}
          />
        </label>
      );
    case "number":
      return (
        <label htmlFor={id} className="form-row">
          <span>{field.label}</span>
          <input
            id={id}
            type="number"
            value={Number(value ?? 0)}
            disabled={disabled}
            onChange={(e) => onChange(Number(e.target.value))}
          />
        </label>
      );
    case "select":
      return (
        <label htmlFor={id} className="form-row">
          <span>{field.label}</span>
          <select
            id={id}
            value={String(value)}
            disabled={disabled}
            onChange={(e) => onChange(e.target.value)}
          >
            {field.options.map((o) => (
              <option key={o} value={o}>
                {o}
              </option>
            ))}
          </select>
        </label>
      );
  }
}

export default function PluginForm({
  plugin,
  onChange,
}: {
  plugin: PluginInfo;
  onChange: (key: string, value: unknown) => Promise<void>;
}) {
  const [values, setValues] = useState<Record<string, unknown>>(() => mergedValues(plugin));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setValues(mergedValues(plugin));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [plugin.id, plugin.values]);

  const update = async (key: string, value: unknown) => {
    const previous = values[key];
    setValues((v) => ({ ...v, [key]: value }));
    setSaving(true);
    setError(null);
    try {
      await onChange(key, value);
    } catch (err) {
      setValues((v) => ({ ...v, [key]: previous }));
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <form className="plugin-form" onSubmit={(e) => e.preventDefault()}>
      {plugin.settings.map((field) => (
        <Field
          key={field.key}
          field={field}
          value={values[field.key]}
          disabled={saving}
          onChange={(v) => update(field.key, v)}
        />
      ))}
      {error && <p className="form-error">{error}</p>}
    </form>
  );
}
