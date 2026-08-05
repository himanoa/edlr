import { useEffect, useRef, useState } from "react";
import type { LayoutSection, PluginInfo, SettingField } from "../types/plugin";

// `PluginInfo` の一部だけを要求する形にしてある。`DriverInfo`(bus を持たない)
// のようにフィールドの一部が異なる形でも、この 3 つさえ揃っていれば
// 同じフォームを再利用できるようにするため。
export type FormPlugin = Pick<PluginInfo, "id" | "settings" | "values"> &
  Partial<Pick<PluginInfo, "secretsSet" | "layout">>;

function mergedValues(plugin: FormPlugin): Record<string, unknown> {
  const defaults: Record<string, unknown> = {};
  for (const field of plugin.settings) {
    // `secret` は `default` を持たず、値もサーバから返ってこない。
    // `map` も `default` を持たない(常に空オブジェクトから始まる)。
    if (field.type === "secret") {
      defaults[field.key] = "";
    } else if (field.type === "map") {
      defaults[field.key] = {};
    } else {
      defaults[field.key] = field.default;
    }
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

/** 編集中の 1 行。`id` は行を消したりキーを書き換えたりしても安定した React key。 */
interface MapRow {
  id: number;
  key: string;
  value: string;
}

/** 行の一覧を保存形(`string -> string`)に畳む。キーが空の行は含めない。 */
function toEntries(rows: MapRow[]): Record<string, string> {
  const entries: Record<string, string> = {};
  for (const row of rows) {
    if (row.key !== "") {
      entries[row.key] = row.value;
    }
  }
  return entries;
}

/**
 * 「同じマップか」の比較に使う正規形。キーの並び順は無視する — サーバが返す
 * オブジェクトのキー順は保存時の並びと一致するとは限らないので、順序の違いだけで
 * 「変わった」と誤判定して保存や行の組み直しが走らないようにする。
 */
function canonical(value: unknown): string {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return "";
  }
  const entries = Object.entries(value as Record<string, unknown>).map(
    ([k, v]) => [k, String(v ?? "")] as const,
  );
  entries.sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  return JSON.stringify(entries);
}

function toRows(value: unknown, nextId: () => number): MapRow[] {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return [];
  }
  return Object.entries(value as Record<string, unknown>).map(([key, v]) => ({
    id: nextId(),
    key,
    value: String(v ?? ""),
  }));
}

/**
 * `string -> string` のマップの編集欄。
 *
 * 他のフィールドと違って値がひとつではないので、行の追加・削除と各セルの
 * 編集を全部まとめて「そのキーの値(オブジェクト)ひとつ」として保存する。
 * 保存のタイミングは他のテキスト入力と同じく blur / Enter(行の削除だけは
 * その場で確定する — 押した時点で意図が確定しているため)。
 *
 * - **キーが空の行は保存対象に含めない**。行を足した直後の空行で保存が走ったり、
 *   サーバ側の「空キーは弾く」に引っ掛かったりしないようにするため。
 *   行そのものは消さない(入力途中で消えたら編集できない)
 * - **キーが重複したら保存せずその場で報せる**。片方を黙って捨てると、
 *   書いたはずの行が消える理由がユーザーからは分からない
 */
function MapField({
  field,
  value,
  disabled,
  onCommit,
}: {
  field: Extract<SettingField, { type: "map" }>;
  value: unknown;
  disabled: boolean;
  onCommit: (v: unknown) => void;
}) {
  const nextId = useRef(0);
  const makeId = () => nextId.current++;
  const [rows, setRows] = useState<MapRow[]>(() => toRows(value, makeId));
  const [duplicate, setDuplicate] = useState<string | null>(null);

  const rowsRef = useRef(rows);
  rowsRef.current = rows;

  // 外から値が変わったとき(保存失敗で巻き戻された・plugin prop が差し替わった)
  // に追従する。`DraftField` と同じ理由で、毎レンダーではなく値の変化時だけ。
  //
  // ただし、いま並んでいる行を畳んだ結果と一致する値なら組み直さない — 自分が
  // 保存した値が返ってきただけであり、ここで行を作り直すと React が行ごと
  // 差し替えて、編集途中の入力欄がフォーカスごと消えてしまう(キーを入れた
  // 直後に値を打ち始めると、その打鍵が丸ごと消える)。
  useEffect(() => {
    if (canonical(toEntries(rowsRef.current)) === canonical(value ?? {})) {
      return;
    }
    setRows(toRows(value, makeId));
    setDuplicate(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [canonical(value)]);

  const commit = (next: MapRow[]) => {
    // キーが空の行は「まだ書いていない」であって「空のキー」ではないので、
    // 保存対象からは外すが行としては残す。
    const filled = next.filter((row) => row.key !== "");
    const entries = toEntries(next);
    if (Object.keys(entries).length !== filled.length) {
      // 後勝ちで黙って潰すと、書いたはずの行が消えた理由が分からない。
      const dup = filled.find((row, i) => filled.findIndex((r) => r.key === row.key) !== i);
      setDuplicate(dup?.key ?? "");
      return;
    }
    setDuplicate(null);
    if (canonical(entries) === canonical(value ?? {})) {
      // 変わっていないなら保存しない(編集せずに blur しただけ、など)。
      return;
    }
    onCommit(entries);
  };

  const editRow = (id: number, patch: Partial<MapRow>) => {
    setRows((current) => current.map((row) => (row.id === id ? { ...row, ...patch } : row)));
  };

  return (
    // 保存はフィールド単位の blur ではなく、fieldset 全体からフォーカスが
    // 外れたときだけ。map は「複数の入力欄で 1 つの値」なので、キー欄→値欄の
    // 移動で {"<key>": ""} のような書きかけの行を保存しない(issue btvh)。
    // blur は React では focusout としてバブルするので fieldset で拾える。
    <fieldset
      className="map-field"
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget)) {
          commit(rows);
        }
      }}
    >
      <legend>{field.label}</legend>
      {rows.map((row) => (
        <div key={row.id} className="map-row">
          <input
            type="text"
            aria-label={`${field.label} のキー`}
            value={row.key}
            disabled={disabled}
            onChange={(e) => editRow(row.id, { key: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                commit(rows);
              }
            }}
          />
          <input
            type="text"
            aria-label={`${field.label} の値`}
            value={row.value}
            disabled={disabled}
            onChange={(e) => editRow(row.id, { value: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                commit(rows);
              }
            }}
          />
          <button
            type="button"
            disabled={disabled}
            onClick={() => {
              const next = rows.filter((r) => r.id !== row.id);
              setRows(next);
              commit(next);
            }}
          >
            削除
          </button>
        </div>
      ))}
      <button
        type="button"
        disabled={disabled}
        onClick={() => setRows((current) => [...current, { id: makeId(), key: "", value: "" }])}
      >
        行を追加
      </button>
      {duplicate !== null && (
        <p className="form-error">キーが重複しています: {duplicate}</p>
      )}
    </fieldset>
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
    case "map":
      return <MapField field={field} value={value} disabled={disabled} onCommit={onChange} />;
    case "select":
      return <SelectField field={field} value={value} disabled={disabled} onChange={onChange} />;
  }
}

/**
 * `select` の描画。候補の取得状況で 3 通りに分かれる:
 *
 * - **候補あり** — 通常のドロップダウン
 * - **`options === null`** — `options-from` が指すドライバから候補を取れて
 *   いない(未インストール・無効化・まだ一度も emit していない)。現在値だけを
 *   出して編集不可にする。設定値そのものは触らない — ドライバが戻れば
 *   そのまま使えるはずの値を、こちらの都合で消してはいけない
 * - **現在値が候補に無い** — 候補は取れたが、保存済みの値がその中にない
 *   (ドライバ側の一覧が変わった)。現在値を先頭に足したうえで警告する。
 *   選択肢から落とすと、開いた瞬間に別の値へ化けたように見えてしまう
 */
function SelectField({
  field,
  value,
  disabled,
  onChange,
}: {
  field: Extract<SettingField, { type: "select" }>;
  value: unknown;
  disabled: boolean;
  onChange: (v: unknown) => void;
}) {
  const id = `field-${field.key}`;
  const current = String(value ?? "");
  const options = field.options;

  if (options === null) {
    const source = field.optionsFrom;
    return (
      <label htmlFor={id} className="form-row">
        <span>{field.label}</span>
        <select id={id} value={current} disabled onChange={() => {}}>
          <option value={current}>{current}</option>
        </select>
        <p className="form-error">
          {source
            ? `候補を取得できません(ドライバ ${source.driver} のトピック ${source.topic} が未着です)`
            : "候補を取得できません"}
        </p>
      </label>
    );
  }

  const missing = current !== "" && !options.some((o) => o.value === current);
  const shown = missing ? [{ value: current, label: current }, ...options] : options;

  return (
    <label htmlFor={id} className="form-row">
      <span>{field.label}</span>
      <select
        id={id}
        value={current}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
      >
        {shown.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      {missing && <p className="form-error">保存済みの値が現在の候補にありません</p>}
    </label>
  );
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

  const fieldByKey = new Map(plugin.settings.map((f) => [f.key, f]));

  const renderField = (key: string) => {
    const field = fieldByKey.get(key);
    if (!field) return null; // サーバ側 resolve 済みなので通常来ないが防御
    return (
      <Field
        key={field.key}
        field={field}
        value={values[field.key]}
        isSecretSet={(plugin.secretsSet ?? []).includes(field.key)}
        disabled={savingKey === field.key}
        onChange={(v) => update(field.key, v)}
      />
    );
  };

  const renderSection = (section: LayoutSection, depth: number, key: number) => (
    <section key={key} className="form-section">
      {depth === 0 ? <h3>{section.title}</h3> : <h4>{section.title}</h4>}
      {section.description && <p className="note">{section.description}</p>}
      {section.children.map((node, i) =>
        "field" in node ? renderField(node.field) : renderSection(node, depth + 1, i),
      )}
    </section>
  );

  return (
    <form className="plugin-form" onSubmit={(e) => e.preventDefault()}>
      {plugin.layout
        ? plugin.layout.sections.map((s, i) => renderSection(s, 0, i))
        : plugin.settings.map((field) => renderField(field.key))}
      {error && <p className="form-error">{error}</p>}
    </form>
  );
}
