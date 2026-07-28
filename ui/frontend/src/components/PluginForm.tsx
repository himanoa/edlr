import { useEffect, useState } from "react";
import type { PluginInfo, SettingField } from "../types/plugin";

// `PluginInfo` の一部だけを要求する形にしてある。`DriverInfo`(bus を持たない)
// のようにフィールドの一部が異なる形でも、この 3 つさえ揃っていれば
// 同じフォームを再利用できるようにするため。
export type FormPlugin = Pick<PluginInfo, "id" | "settings" | "values"> &
  Partial<Pick<PluginInfo, "secretsSet">>;

function mergedValues(plugin: FormPlugin): Record<string, unknown> {
  const defaults: Record<string, unknown> = {};
  for (const field of plugin.settings) {
    // `secret` は `default` を持たず、値もサーバから返ってこない。
    defaults[field.key] = field.type === "secret" ? "" : field.default;
  }
  return { ...defaults, ...plugin.values };
}

/**
 * 秘密情報の入力欄。
 *
 * 他のフィールドと違い、**常に空から始まる**: サーバは保存済みの値を返さない
 * (write-only)ので、埋めようがないし、埋めるべきでもない。空のまま離れても
 * 保存はしない -- そうしないと、フォームを開いて何もせず閉じるだけで保存済みの
 * 秘密情報が消えてしまう。意図的に消したい場合の導線は今のところ無い
 * (プラグインの設定ファイルを直接消す)。
 */
function SecretField({
  field,
  isSet,
  disabled,
  onCommit,
}: {
  field: Extract<SettingField, { type: "secret" }>;
  isSet: boolean;
  disabled: boolean;
  onCommit: (v: unknown) => void;
}) {
  const id = `field-${field.key}`;
  const [draft, setDraft] = useState("");

  const commit = () => {
    if (draft === "") {
      // 未入力 = 変更なし。保存済みの値を空で上書きしない。
      return;
    }
    onCommit(draft);
    setDraft("");
  };

  return (
    <label htmlFor={id} className="form-row">
      <span>{field.label}</span>
      <input
        id={id}
        type="password"
        value={draft}
        disabled={disabled}
        placeholder={isSet ? "設定済み(変更する場合のみ入力)" : "未設定"}
        autoComplete="off"
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commit();
          }
        }}
      />
    </label>
  );
}

function DraftField({
  field,
  value,
  disabled,
  onCommit,
}: {
  field: Extract<SettingField, { type: "string" | "number" }>;
  value: unknown;
  disabled: boolean;
  onCommit: (v: unknown) => void;
}) {
  const id = `field-${field.key}`;
  const [draft, setDraft] = useState(() => String(value ?? ""));

  // Keep the draft in sync with the committed value whenever it changes from
  // outside this input (e.g. reverted after a failed save, or the plugin
  // prop changed) — but not on every render, so typing isn't clobbered.
  useEffect(() => {
    setDraft(String(value ?? ""));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value]);

  const commit = () => {
    if (draft === String(value ?? "")) {
      // Nothing changed since the last committed value (e.g. blur without
      // editing, or Enter pressed twice) — avoid an unnecessary save.
      return;
    }
    const next = field.type === "number" ? Number(draft) : draft;
    onCommit(next);
  };

  return (
    <label htmlFor={id} className="form-row">
      <span>{field.label}</span>
      <input
        id={id}
        type={field.type === "number" ? "number" : "text"}
        value={draft}
        disabled={disabled}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commit();
          }
        }}
      />
    </label>
  );
}

function Field({
  field,
  value,
  isSecretSet,
  disabled,
  onChange,
}: {
  field: SettingField;
  value: unknown;
  isSecretSet: boolean;
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
    case "number":
      return <DraftField field={field} value={value} disabled={disabled} onCommit={onChange} />;
    case "secret":
      return (
        <SecretField
          field={field}
          isSet={isSecretSet}
          disabled={disabled}
          onCommit={onChange}
        />
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
  plugin: FormPlugin;
  onChange: (key: string, value: unknown) => Promise<void>;
}) {
  const [values, setValues] = useState<Record<string, unknown>>(() => mergedValues(plugin));
  const [savingKey, setSavingKey] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setValues(mergedValues(plugin));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [plugin.id, plugin.values]);

  const update = async (key: string, value: unknown) => {
    const previous = values[key];
    setValues((v) => ({ ...v, [key]: value }));
    setSavingKey(key);
    setError(null);
    try {
      await onChange(key, value);
    } catch (err) {
      setValues((v) => ({ ...v, [key]: previous }));
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingKey((k) => (k === key ? null : k));
    }
  };

  return (
    <form className="plugin-form" onSubmit={(e) => e.preventDefault()}>
      {plugin.settings.map((field) => (
        <Field
          key={field.key}
          field={field}
          value={values[field.key]}
          isSecretSet={(plugin.secretsSet ?? []).includes(field.key)}
          disabled={savingKey === field.key}
          onChange={(v) => update(field.key, v)}
        />
      ))}
      {error && <p className="form-error">{error}</p>}
    </form>
  );
}
