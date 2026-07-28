import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import Dashboard from "./Dashboard";
import type { DashboardListEntry } from "../types/plugin";

let widgets: DashboardListEntry[] = [];
let listDashboardImpl: () => Promise<{ widgets: DashboardListEntry[] }> = () =>
  Promise.resolve({ widgets });

vi.mock("../ws", () => ({
  defaultWsUrl: () => "ws://test/ws",
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
  it("renders granted widgets as iframe cards and placeholders for unresolved ones", async () => {
    widgets = [
      running,
      { ...running, widget: "broken", title: "Broken", size: "small", resolved: false },
      { ...running, widget: "stopped", title: "Stopped", state: "disabled" },
    ];
    render(<Dashboard />);
    await waitFor(() => expect(screen.getByText("Status")).toBeInTheDocument());
    // 稼働中 + 解決済みの 1 件だけ iframe になる
    expect(document.querySelectorAll("iframe")).toHaveLength(1);
    expect(screen.getByText(/entry ファイルが見つかりません/)).toBeInTheDocument();
    expect(screen.getByText(/プラグインが停止しています/)).toBeInTheDocument();
  });

  it("applies grid column span by size", async () => {
    widgets = [running];
    render(<Dashboard />);
    await waitFor(() => expect(screen.getByText("Status")).toBeInTheDocument());
    const card = document.querySelector(".widget-card") as HTMLElement;
    expect(card.style.gridColumn).toBe("span 2");
  });

  it("shows guidance when no widgets are granted", async () => {
    render(<Dashboard />);
    await waitFor(() =>
      expect(screen.getByText(/承認済みのウィジェットがありません/)).toBeInTheDocument(),
    );
  });

  it("shows an error when dashboard/list fails", async () => {
    listDashboardImpl = () => Promise.reject(new Error("boom"));
    render(<Dashboard />);
    await waitFor(() => expect(screen.getByText(/boom/)).toBeInTheDocument());
  });
});
