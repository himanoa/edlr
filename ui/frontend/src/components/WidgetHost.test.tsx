import { render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import WidgetHost, { type WidgetApi } from "./WidgetHost";
import type { DashboardListEntry } from "../types/plugin";
import type { LogEntry } from "../lib/filter";

const entry: DashboardListEntry = {
  plugin: "widgety",
  pluginName: "W",
  widget: "status",
  title: "Status",
  url: "/plugin-ui/widgety/status/index.js",
  size: "small",
  events: ["FSDJump"],
  resolved: true,
  state: "running",
};
const jump: LogEntry = { id: 1, kind: "journal", timestamp: "t", event: "FSDJump", raw: {} };
const dock: LogEntry = { id: 2, kind: "journal", timestamp: "t", event: "Docked", raw: {} };

/** mount 済み api を捕まえる fake モジュールローダ。 */
function fakeLoader(captured: { api?: WidgetApi; el?: HTMLElement; cleanups: number }) {
  return async (_url: string) => ({
    default: (el: HTMLElement, api: WidgetApi) => {
      captured.el = el;
      captured.api = api;
      el.textContent = "widget body";
      return () => {
        captured.cleanups++;
      };
    },
  });
}

describe("WidgetHost", () => {
  it("loads the module from the daemon url and mounts it", async () => {
    const captured = { cleanups: 0 } as Parameters<typeof fakeLoader>[0];
    const urls: string[] = [];
    const load = (url: string) => (urls.push(url), fakeLoader(captured)(url));
    render(<WidgetHost entry={entry} entries={[]} load={load} />);
    await screen.findByText("widget body");
    // 相対パスのままだと Tauri シェルでは tauri:// origin に解決されるため絶対化される
    expect(urls).toEqual([`http://localhost:3000${entry.url}`]);
    expect(captured.api!.plugin).toBe("widgety");
    expect(captured.api!.widget).toBe("status");
  });

  it("delivers only matching events, including ones from before mount, once each", async () => {
    // 実ウィジェット同様、mount の中で同期的に onEvent を登録する
    const received: LogEntry[] = [];
    const load = async () => ({
      default: (el: HTMLElement, api: WidgetApi) => {
        el.textContent = "widget body";
        api.onEvent((ev) => received.push(ev));
      },
    });
    const { rerender } = render(<WidgetHost entry={entry} entries={[jump, dock]} load={load} />);
    await screen.findByText("widget body");
    // mount 前の蓄積分のうちマッチする FSDJump のみが届く
    await waitFor(() => expect(received).toHaveLength(1));
    expect(received[0].event).toBe("FSDJump");

    const more: LogEntry = { id: 3, kind: "journal", timestamp: "t", event: "FSDJump", raw: {} };
    rerender(<WidgetHost entry={entry} entries={[jump, dock, more, dock]} load={load} />);
    await waitFor(() => expect(received).toHaveLength(2));
  });

  it("delivers bus frames to onBus listeners regardless of the events filter", async () => {
    const received: Array<{ driver: string; topic: string; payload: string }> = [];
    const load = async () => ({
      default: (el: HTMLElement, api: WidgetApi) => {
        el.textContent = "widget body";
        api.onBus((msg) => received.push(msg));
      },
    });
    const busEntry: LogEntry = {
      id: 1,
      kind: "bus",
      driver: "eddn",
      topic: "upload-status",
      payload: '{"ok":true}',
      event: "eddn/upload-status",
      raw: {},
    };
    // entry.events は ["FSDJump"] のままでも bus フレームは届く
    render(<WidgetHost entry={entry} entries={[busEntry, jump]} load={load} />);
    await screen.findByText("widget body");
    await waitFor(() => expect(received).toHaveLength(1));
    expect(received[0]).toEqual({
      driver: "eddn",
      topic: "upload-status",
      payload: '{"ok":true}',
    });
  });

  it("calls cleanup on unmount", async () => {
    const captured = { cleanups: 0 } as Parameters<typeof fakeLoader>[0];
    const { unmount } = render(
      <WidgetHost entry={entry} entries={[]} load={fakeLoader(captured)} />,
    );
    await screen.findByText("widget body");
    unmount();
    expect(captured.cleanups).toBe(1);
  });

  it("shows an error inside the widget frame when the module fails to load", async () => {
    const load = () => Promise.reject(new Error("404"));
    render(<WidgetHost entry={entry} entries={[]} load={load} />);
    await screen.findByText(/読み込みに失敗/);
  });
});
