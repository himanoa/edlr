import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, test, vi } from "vitest";
import type { PluginInfo, SettingField } from "../types/plugin";
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
      {
        type: "select",
        key: "voice",
        label: "音声",
        default: "Amber",
        options: [
          { value: "Amber", label: "Amber" },
          { value: "Blue", label: "Blue" },
        ],
      },
    ],
    values: { enabled: true, endpoint: "http://localhost", volume: 80, voice: "Amber" },
    capabilities: { requests: [], granted: false, staleGrant: false },
    sidecars: [],
    filesystem: [],
    bus: [],
    dashboard: [],
    schedules: [],
    secretsSet: [],
    dropped: { events: 0, busDeliveries: 0 },
    layout: null,
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

function makeSecretPlugin(secretsSet: string[] = []): PluginInfo {
  return makePlugin({
    settings: [{ type: "secret", key: "api-key", label: "API Key" }],
    values: {},
    secretsSet,
  });
}

test("a secret renders as a masked input that is never prefilled", async () => {
  // サーバは保存済みの値を返さない(write-only)ので、埋めようがない。
  render(<PluginForm plugin={makeSecretPlugin(["api-key"])} onChange={vi.fn()} />);

  const input = screen.getByLabelText("API Key") as HTMLInputElement;
  expect(input.type).toBe("password");
  expect(input.value).toBe("");
});

test("a secret's placeholder distinguishes configured from unset", async () => {
  const configured = render(<PluginForm plugin={makeSecretPlugin(["api-key"])} onChange={vi.fn()} />);
  expect((configured.getByLabelText("API Key") as HTMLInputElement).placeholder).toMatch(/設定済み/);
  configured.unmount();

  const unset = render(<PluginForm plugin={makeSecretPlugin([])} onChange={vi.fn()} />);
  expect((unset.getByLabelText("API Key") as HTMLInputElement).placeholder).toMatch(/未設定/);
});

test("typing a secret and blurring saves it, then clears the input", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  render(<PluginForm plugin={makeSecretPlugin([])} onChange={onChange} />);

  const input = screen.getByLabelText("API Key") as HTMLInputElement;
  await userEvent.type(input, "sk-live-123");
  await userEvent.tab();

  expect(onChange).toHaveBeenCalledWith("api-key", "sk-live-123");
  // 入力欄に秘密情報を残さない。
  expect(input.value).toBe("");
});

test("leaving a secret field empty does not overwrite the stored value", async () => {
  // これが無いと、フォームを開いて何もせず閉じるだけで保存済みの
  // 秘密情報が空文字列で潰れてしまう。
  const onChange = vi.fn().mockResolvedValue(undefined);
  render(<PluginForm plugin={makeSecretPlugin(["api-key"])} onChange={onChange} />);

  const input = screen.getByLabelText("API Key");
  await userEvent.click(input);
  await userEvent.tab();

  expect(onChange).not.toHaveBeenCalled();
});

function makeMapPlugin(values: Record<string, string> = {}): PluginInfo {
  return makePlugin({
    settings: [{ type: "map", key: "aliases", label: "表示名の置き換え" }],
    values: { aliases: values },
  });
}

test("a map renders one key/value row per stored entry", () => {
  render(<PluginForm plugin={makeMapPlugin({ Sol: "太陽系" })} onChange={vi.fn()} />);

  const keys = screen.getAllByLabelText("表示名の置き換え のキー") as HTMLInputElement[];
  const vals = screen.getAllByLabelText("表示名の置き換え の値") as HTMLInputElement[];
  expect(keys.map((i) => i.value)).toEqual(["Sol"]);
  expect(vals.map((i) => i.value)).toEqual(["太陽系"]);
});

test("a map with no entries renders no rows", () => {
  render(<PluginForm plugin={makeMapPlugin()} onChange={vi.fn()} />);
  expect(screen.queryByLabelText("表示名の置き換え のキー")).not.toBeInTheDocument();
});

test("adding a row does not save until the key is filled in", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  render(<PluginForm plugin={makeMapPlugin()} onChange={onChange} />);

  await userEvent.click(screen.getByRole("button", { name: "行を追加" }));
  expect(screen.getAllByLabelText("表示名の置き換え のキー")).toHaveLength(1);
  expect(onChange).not.toHaveBeenCalled();

  // 値だけ入れて離れても、キーが空の行は保存対象にならない。
  await userEvent.type(screen.getByLabelText("表示名の置き換え の値"), "太陽系");
  await userEvent.click(document.body);
  expect(onChange).not.toHaveBeenCalled();
});

test("moving from the key input to the value input does not save a half-filled row", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  render(<PluginForm plugin={makeMapPlugin()} onChange={onChange} />);

  await userEvent.click(screen.getByRole("button", { name: "行を追加" }));
  await userEvent.type(screen.getByLabelText("表示名の置き換え のキー"), "Deciat");
  // キー欄 → 値欄への移動(fieldset 内の blur)では保存しない(issue btvh)。
  await userEvent.click(screen.getByLabelText("表示名の置き換え の値"));
  expect(onChange).not.toHaveBeenCalled();

  await userEvent.type(screen.getByLabelText("表示名の置き換え の値"), "デシアト");
  await userEvent.click(document.body);
  expect(onChange).toHaveBeenCalledTimes(1);
  expect(onChange).toHaveBeenCalledWith("aliases", { Deciat: "デシアト" });
});

test("filling in a new row saves the whole map object", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  render(<PluginForm plugin={makeMapPlugin({ Sol: "太陽系" })} onChange={onChange} />);

  await userEvent.click(screen.getByRole("button", { name: "行を追加" }));
  const keys = screen.getAllByLabelText("表示名の置き換え のキー");
  const vals = screen.getAllByLabelText("表示名の置き換え の値");
  await userEvent.type(keys[1], "Deciat");
  await userEvent.type(vals[1], "デシアト");
  await userEvent.click(document.body);

  expect(onChange).toHaveBeenCalledTimes(1);
  expect(onChange).toHaveBeenCalledWith("aliases", { Sol: "太陽系", Deciat: "デシアト" });
});

test("editing a value commits on Enter", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  render(<PluginForm plugin={makeMapPlugin({ Sol: "太陽系" })} onChange={onChange} />);

  const value = screen.getByLabelText("表示名の置き換え の値");
  await userEvent.clear(value);
  await userEvent.type(value, "ソル{Enter}");

  expect(onChange).toHaveBeenCalledTimes(1);
  expect(onChange).toHaveBeenCalledWith("aliases", { Sol: "ソル" });
});

test("removing a row saves the map without that entry", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  render(
    <PluginForm plugin={makeMapPlugin({ Sol: "太陽系", Deciat: "デシアト" })} onChange={onChange} />,
  );

  const remove = screen.getAllByRole("button", { name: "削除" });
  await userEvent.click(remove[0]);

  expect(onChange).toHaveBeenCalledWith("aliases", { Deciat: "デシアト" });
});

test("duplicate keys are reported instead of silently keeping the last one", async () => {
  const onChange = vi.fn().mockResolvedValue(undefined);
  render(<PluginForm plugin={makeMapPlugin({ Sol: "太陽系" })} onChange={onChange} />);

  await userEvent.click(screen.getByRole("button", { name: "行を追加" }));
  const keys = screen.getAllByLabelText("表示名の置き換え のキー");
  const vals = screen.getAllByLabelText("表示名の置き換え の値");
  await userEvent.type(keys[1], "Sol");
  await userEvent.type(vals[1], "別の名前");
  await userEvent.click(document.body);

  expect(await screen.findByText(/キーが重複/)).toBeInTheDocument();
  expect(onChange).not.toHaveBeenCalled();
});

test("a failed save on a map surfaces the error", async () => {
  const onChange = vi.fn().mockRejectedValue(new Error("save failed"));
  render(<PluginForm plugin={makeMapPlugin({ Sol: "太陽系" })} onChange={onChange} />);

  const remove = screen.getByRole("button", { name: "削除" });
  await userEvent.click(remove);

  expect(await screen.findByText("save failed")).toBeInTheDocument();
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

describe("layout", () => {
  const settings: SettingField[] = [
    { type: "string", key: "endpoint", label: "Endpoint", default: "" },
    { type: "string", key: "voice", label: "Voice", default: "" },
  ];

  it("layout があればセクション見出しと説明を描画する", () => {
    render(
      <PluginForm
        plugin={{
          id: "p1",
          settings,
          values: {},
          layout: {
            sections: [
              {
                title: "接続",
                description: "サーバへの接続設定",
                children: [{ field: "endpoint" }],
              },
              { title: "読み上げ", children: [{ field: "voice" }] },
            ],
          },
        }}
        onChange={async () => {}}
      />,
    );
    expect(screen.getByRole("heading", { name: "接続" })).toBeInTheDocument();
    expect(screen.getByText("サーバへの接続設定")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "読み上げ" })).toBeInTheDocument();
    expect(screen.getByLabelText("Endpoint")).toBeInTheDocument();
    expect(screen.getByLabelText("Voice")).toBeInTheDocument();
  });

  it("入れ子セクションも描画する", () => {
    render(
      <PluginForm
        plugin={{
          id: "p1",
          settings,
          values: {},
          layout: {
            sections: [
              {
                title: "外",
                children: [
                  { field: "endpoint" },
                  { title: "内", children: [{ field: "voice" }] },
                ],
              },
            ],
          },
        }}
        onChange={async () => {}}
      />,
    );
    expect(screen.getByRole("heading", { name: "内" })).toBeInTheDocument();
    expect(screen.getByLabelText("Voice")).toBeInTheDocument();
  });

  it("layout が null なら従来どおり平坦に描画する", () => {
    render(
      <PluginForm
        plugin={{ id: "p1", settings, values: {}, layout: null }}
        onChange={async () => {}}
      />,
    );
    expect(screen.queryByRole("heading")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Endpoint")).toBeInTheDocument();
  });
});
