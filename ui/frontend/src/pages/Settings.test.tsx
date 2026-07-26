import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import Settings from "./Settings";

vi.mock("../lib/tauri", () => ({
  isTauri: vi.fn(),
  invoke: vi.fn(),
}));

import { invoke, isTauri } from "../lib/tauri";

const mockIsTauri = vi.mocked(isTauri);
const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  vi.resetAllMocks();
});

describe("非 Tauri 環境", () => {
  it("読み取り専用の案内を出し、IPC を呼ばない", async () => {
    mockIsTauri.mockReturnValue(false);

    render(<Settings />);

    expect(
      await screen.findByText(/デスクトップアプリから変更してください/),
    ).toBeInTheDocument();
    expect(mockInvoke).not.toHaveBeenCalled();
  });
});

describe("Tauri 環境", () => {
  it("現在の journalDir を表示する", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockResolvedValue({
      journalDir: "/mnt/game/ED",
      daemonManaged: true,
      configError: null,
    });

    render(<Settings />);

    expect(await screen.findByDisplayValue("/mnt/game/ED")).toBeInTheDocument();
  });

  it("保存に成功したら成功メッセージを出す", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return { journalDir: null, daemonManaged: true, configError: null };
      }
      return { journalDir: "/mnt/game/ED", daemonManaged: true, configError: null };
    });

    render(<Settings />);
    const input = await screen.findByLabelText("Journal ディレクトリ");
    await userEvent.type(input, "/mnt/game/ED");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));

    expect(await screen.findByText(/保存しました/)).toBeInTheDocument();
  });

  it("パスが不正ならエラーを出す", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return { journalDir: null, daemonManaged: true, configError: null };
      }
      throw new Error("ディレクトリが存在しません: /nope");
    });

    render(<Settings />);
    const input = await screen.findByLabelText("Journal ディレクトリ");
    await userEvent.type(input, "/nope");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));

    expect(await screen.findByText(/ディレクトリが存在しません/)).toBeInTheDocument();
  });

  it("外部起動デーモンなら再起動されない旨を出す", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return { journalDir: null, daemonManaged: false, configError: null };
      }
      return { journalDir: "/mnt/game/ED", daemonManaged: false, configError: null };
    });

    render(<Settings />);
    const input = await screen.findByLabelText("Journal ディレクトリ");
    await userEvent.type(input, "/mnt/game/ED");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => {
      expect(screen.getByText(/外部で起動中のデーモン/)).toBeInTheDocument();
    });
  });

  it("設定ファイルが壊れている場合は警告を出す", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockResolvedValue({
      journalDir: null,
      daemonManaged: true,
      configError: "config file is not valid JSON: expected value at line 1 column 1",
    });

    render(<Settings />);

    expect(await screen.findByText(/設定ファイルを読み込めませんでした/)).toBeInTheDocument();
  });
});
