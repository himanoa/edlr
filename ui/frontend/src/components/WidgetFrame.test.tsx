import { act, render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { WidgetFrame } from "./WidgetFrame";
import type { DashboardListEntry } from "../types/plugin";
import type { LogEntry } from "../lib/filter";

const entry: DashboardListEntry = {
  plugin: "widgety",
  pluginName: "W",
  widget: "status",
  title: "Status",
  url: "/plugin-ui/widgety/status/index.html",
  size: "small",
  events: ["FSDJump"],
  resolved: true,
  state: "running",
};
const jump: LogEntry = {
  id: 1,
  kind: "journal",
  timestamp: "t",
  event: "FSDJump",
  raw: { StarSystem: "Sol" },
};
const dock: LogEntry = { id: 2, kind: "journal", timestamp: "t", event: "Docked", raw: {} };

/** iframe の contentWindow.postMessage を差し替えて観測する。 */
function capturePosts(iframe: HTMLIFrameElement, received: unknown[]) {
  const win = iframe.contentWindow as Window;
  (win as unknown as { postMessage: (msg: unknown) => void }).postMessage = (msg: unknown) =>
    received.push(msg);
  return win;
}

/** widget からの edlr:ready を親 window で受けた体にする。 */
function sendReady(win: Window) {
  act(() => {
    window.dispatchEvent(
      new MessageEvent("message", { data: { type: "edlr:ready" }, source: win }),
    );
  });
}

const eventsOf = (received: unknown[]) =>
  received.filter((m) => (m as { type: string }).type === "edlr:event");

describe("WidgetFrame", () => {
  it("renders a sandboxed iframe pointing at the widget url", () => {
    const { container } = render(<WidgetFrame entry={entry} entries={[]} />);
    const iframe = container.querySelector("iframe")!;
    expect(iframe.getAttribute("sandbox")).toBe("allow-scripts");
    // 相対パスのままだと Tauri シェルでは tauri:// origin に解決されて 404 に
    // なるため、デーモンの origin へ絶対化されていること。
    expect(iframe.getAttribute("src")).toBe(`http://localhost:3000${entry.url}`);
  });

  it("sends init and only matching events after ready", () => {
    const received: unknown[] = [];
    const { container, rerender } = render(<WidgetFrame entry={entry} entries={[jump, dock]} />);
    const iframe = container.querySelector("iframe")!;
    const win = capturePosts(iframe, received);

    sendReady(win);
    // ready 後: init + 蓄積分のうちマッチする FSDJump のみ
    expect(received[0]).toMatchObject({
      type: "edlr:init",
      plugin: "widgety",
      widget: "status",
    });
    expect(eventsOf(received)).toHaveLength(1);
    expect(eventsOf(received)[0]).toMatchObject({ event: { event: "FSDJump" } });

    // 以後の新着もマッチ分のみ転送(既送信分は再送しない)
    const more: LogEntry = { id: 3, kind: "journal", timestamp: "t", event: "FSDJump", raw: {} };
    const dock2: LogEntry = { id: 4, kind: "journal", timestamp: "t", event: "Docked", raw: {} };
    rerender(<WidgetFrame entry={entry} entries={[jump, dock, more, dock2]} />);
    expect(eventsOf(received)).toHaveLength(2);
  });

  it("does not send anything before ready", () => {
    const received: unknown[] = [];
    const { container } = render(<WidgetFrame entry={entry} entries={[jump]} />);
    capturePosts(container.querySelector("iframe")!, received);
    expect(received).toHaveLength(0);
  });

  it("ignores ready messages from other windows", () => {
    const received: unknown[] = [];
    const { container } = render(<WidgetFrame entry={entry} entries={[jump]} />);
    capturePosts(container.querySelector("iframe")!, received);
    act(() => {
      window.dispatchEvent(
        new MessageEvent("message", { data: { type: "edlr:ready" }, source: window }),
      );
    });
    expect(received).toHaveLength(0);
  });

  it("adjusts iframe height from edlr:height messages with clamping", () => {
    const { container } = render(<WidgetFrame entry={entry} entries={[]} />);
    const iframe = container.querySelector("iframe")!;
    const win = iframe.contentWindow as Window;
    act(() => {
      window.dispatchEvent(
        new MessageEvent("message", { data: { type: "edlr:height", px: 300 }, source: win }),
      );
    });
    expect(iframe.style.height).toBe("300px");
    act(() => {
      window.dispatchEvent(
        new MessageEvent("message", { data: { type: "edlr:height", px: 99999 }, source: win }),
      );
    });
    expect(iframe.style.height).toBe("800px");
  });
});
