import { render, screen, waitFor } from "@testing-library/react";
// 各テストを独立した jotai store で走らせる(async atom のキャッシュが
// テスト間に漏れないように)。store なし Provider はマウントごとに新規 store を作る。
import { Provider } from "jotai";
import { beforeEach, describe, expect, it, vi } from "vitest";
import Dashboard from "./Dashboard";
import type { DashboardListEntry } from "../types/plugin";

let widgets: DashboardListEntry[] = [];
let listDashboardImpl: () => Promise<{ widgets: DashboardListEntry[] }> = () =>
  Promise.resolve({ widgets });

vi.mock("../ws", () => ({
  defaultWsUrl: () => "ws://test/ws",
  daemonHttpUrl: (path: string) => `http://test${path}`,
  useEventStream: () => ({ entries: [], connection: "open" }),
}));
vi.mock("../rpc", () => ({
  RpcClient: class {
    listDashboard() {
      return listDashboardImpl();
    }
    close() {}
  },
}));

const running: DashboardListEntry = {
  plugin: "widgety",
  pluginName: "W",
  widget: "status",
  title: "Status",
  url: "/plugin-ui/widgety/status/index.html",
  size: "medium",
  events: ["*"],
  resolved: true,
  state: "running",
};

beforeEach(() => {
  widgets = [];
  listDashboardImpl = () => Promise.resolve({ widgets });
});

describe("Dashboard", () => {
  it("renders granted widget cards and placeholders for unresolved ones", async () => {
    widgets = [
      running,
      { ...running, widget: "broken", title: "Broken", size: "small", resolved: false },
      { ...running, widget: "stopped", title: "Stopped", state: "disabled" },
    ];
    render(<Provider><Dashboard /></Provider>);
    await waitFor(() => expect(screen.getByText("Status")).toBeInTheDocument());
    // iframe 方式は廃止 — 稼働中 + 解決済みの 1 件だけ WidgetHost がマウントする
    expect(document.querySelectorAll("iframe")).toHaveLength(0);
    expect(document.querySelectorAll(".widget-card")).toHaveLength(3);
    expect(screen.getByText(/entry ファイルが見つかりません/)).toBeInTheDocument();
    expect(screen.getByText(/プラグインが停止しています/)).toBeInTheDocument();
  });

  it("renders widget cards as draggable grid items", async () => {
    widgets = [running];
    render(<Provider><Dashboard /></Provider>);
    await waitFor(() => expect(screen.getByText("Status")).toBeInTheDocument());
    // 配置・サイズは react-grid-layout が受け持つ(初期幅は mergeLayout が
    // manifest size から決める — そちらは widgetLayout.test.ts で担保)
    const card = document.querySelector(".widget-card") as HTMLElement;
    expect(card.classList.contains("react-grid-item")).toBe(true);
    expect(card.querySelector(".widget-drag-handle")).not.toBeNull();
  });

  it("exposes resize handles on every edge and corner", async () => {
    widgets = [running];
    render(<Provider><Dashboard /></Provider>);
    await waitFor(() => expect(screen.getByText("Status")).toBeInTheDocument());
    const card = document.querySelector(".widget-card") as HTMLElement;
    // 右下だけでなく外周のどこからでも掴めること(4辺 + 4隅)
    expect(card.querySelectorAll(".react-resizable-handle")).toHaveLength(8);
  });

  it("shows guidance when no widgets are granted", async () => {
    render(<Provider><Dashboard /></Provider>);
    await waitFor(() =>
      expect(screen.getByText(/承認済みのウィジェットがありません/)).toBeInTheDocument(),
    );
  });

  it("shows an error when dashboard/list fails", async () => {
    listDashboardImpl = () => Promise.reject(new Error("boom"));
    render(<Provider><Dashboard /></Provider>);
    await waitFor(() => expect(screen.getByText(/boom/)).toBeInTheDocument());
  });
});
