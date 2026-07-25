import { useState } from "react";
import { loadSettings, saveSettings } from "../lib/settings";
import type { PluginManifest, SettingField } from "../mock/plugins";

function Field({
  field,
  value,
  onChange,
}: {
  field: SettingField;
  value: unknown;
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

export default function PluginForm({ manifest }: { manifest: PluginManifest }) {
  const [values, setValues] = useState<Record<string, unknown>>(() =>
    loadSettings(manifest),
  );
  const update = (key: string, value: unknown) => {
    const next = { ...values, [key]: value };
    setValues(next);
    saveSettings(manifest.id, next);
  };
  return (
    <form className="plugin-form" onSubmit={(e) => e.preventDefault()}>
      {manifest.settings.map((field) => (
        <Field
          key={field.key}
          field={field}
          value={values[field.key]}
          onChange={(v) => update(field.key, v)}
        />
      ))}
    </form>
  );
}
