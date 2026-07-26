import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import App from "./App";

// userEvent は @testing-library/user-event。devDependencies に "^14.5.2" で追加すること。
test("shows dashboard placeholder by default and switches tabs", async () => {
  render(<App />);
  expect(screen.getByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Logs" }));
  expect(screen.getByPlaceholderText("フィルタ(イベント名・内容)")).toBeInTheDocument();
});

describe("初期タブ", () => {
  it("journalDir が未設定なら Settings から始まる", async () => {
    vi.resetModules();
    vi.doMock("./lib/tauri", () => ({
      isTauri: () => true,
      invoke: vi.fn().mockResolvedValue({
        journalDir: null,
        daemonManaged: true,
        configError: null,
      }),
    }));
    const { default: FreshApp } = await import("./App");

    render(<FreshApp />);

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Settings" })).toHaveClass("active");
    });
    vi.doUnmock("./lib/tauri");
  });
});
