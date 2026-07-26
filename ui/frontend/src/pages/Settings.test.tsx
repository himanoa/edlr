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
      configuredJournalDir: "/mnt/game/ED",
      daemonManaged: true,
      configError: null,
    });

    render(<Settings />);

    expect(await screen.findByDisplayValue("/mnt/game/ED")).toBeInTheDocument();
  });

  it("envOverride 中は実効値ではなく設定ファイルの値を入力欄に表示する", async () => {
    // envOverride が true で、実効値(journalDir)と設定ファイルの生の値
    // (configuredJournalDir)が食い違うケース。編集フォームは設定ファイルの
    // 値を起点にしないと、無編集で保存しただけで env 由来の値を設定ファイルへ
    // 書き戻し、保存済みの値を消してしまう(Finding 1)。
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockResolvedValue({
      journalDir: "/env/ED",
      configuredJournalDir: "/mnt/game/ED",
      daemonManaged: true,
      configError: null,
      envOverride: true,
    });

    render(<Settings />);

    expect(await screen.findByDisplayValue("/mnt/game/ED")).toBeInTheDocument();
    expect(screen.queryByDisplayValue("/env/ED")).not.toBeInTheDocument();
  });

  it("envOverride 中に無編集のまま保存すると、実効値ではなく設定ファイルの値を送る", async () => {
    // 上のテストの続き: 実際に保存ボタンを押したとき set_journal_dir へ渡る
    // path が env 値(/env/ED)ではなく設定ファイルの値(/mnt/game/ED)である
    // ことを確認する。ここが env 値のままだと、保存のたびに設定ファイルが
    // env 値で上書きされ、env を外したときに意図しないディレクトリで
    // デーモンが起動してしまう。
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return {
          journalDir: "/env/ED",
          configuredJournalDir: "/mnt/game/ED",
          daemonManaged: true,
          configError: null,
          envOverride: true,
        };
      }
      return {
        journalDir: "/env/ED",
        configuredJournalDir: "/mnt/game/ED",
        daemonManaged: true,
        configError: null,
        envOverride: true,
      };
    });

    render(<Settings />);
    await screen.findByDisplayValue("/mnt/game/ED");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));

    expect(mockInvoke).toHaveBeenCalledWith("set_journal_dir", { path: "/mnt/game/ED" });
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

    expect(await screen.findByText(/デーモンを再起動しました/)).toBeInTheDocument();
    expect(mockInvoke).toHaveBeenCalledWith("set_journal_dir", { path: "/mnt/game/ED" });
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

  it("再起動に失敗しても保存済みであることが分かるメッセージを出す", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return { journalDir: null, daemonManaged: true, configError: null };
      }
      throw new Error(
        "設定は保存されました。ただしデーモンの再起動に失敗しました: failed to spawn edlr daemon: No such file or directory",
      );
    });

    render(<Settings />);
    const input = await screen.findByLabelText("Journal ディレクトリ");
    await userEvent.type(input, "/mnt/game/ED");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));

    expect(await screen.findByText(/設定は保存されました/)).toBeInTheDocument();
    expect(screen.getByText(/デーモンの再起動に失敗しました/)).toBeInTheDocument();
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

  it("ディレクトリ選択に成功したら draft に反映する", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return { journalDir: null, daemonManaged: true, configError: null };
      }
      if (cmd === "pick_journal_dir") {
        return "/mnt/game/Picked";
      }
      throw new Error(`unexpected command: ${cmd}`);
    });

    render(<Settings />);
    await screen.findByLabelText("Journal ディレクトリ");
    await userEvent.click(screen.getByRole("button", { name: "選択…" }));

    expect(await screen.findByDisplayValue("/mnt/game/Picked")).toBeInTheDocument();
  });

  it("ディレクトリ選択がキャンセルされたら draft は変わらない", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return {
          journalDir: "/mnt/game/ED",
          configuredJournalDir: "/mnt/game/ED",
          daemonManaged: true,
          configError: null,
        };
      }
      if (cmd === "pick_journal_dir") {
        return null;
      }
      throw new Error(`unexpected command: ${cmd}`);
    });

    render(<Settings />);
    await screen.findByDisplayValue("/mnt/game/ED");
    await userEvent.click(screen.getByRole("button", { name: "選択…" }));

    expect(await screen.findByDisplayValue("/mnt/game/ED")).toBeInTheDocument();
  });

  it("ディレクトリ選択に失敗したらエラーを出す", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return { journalDir: null, daemonManaged: true, configError: null };
      }
      if (cmd === "pick_journal_dir") {
        throw new Error("no desktop portal available");
      }
      throw new Error(`unexpected command: ${cmd}`);
    });

    render(<Settings />);
    await screen.findByLabelText("Journal ディレクトリ");
    await userEvent.click(screen.getByRole("button", { name: "選択…" }));

    expect(await screen.findByText(/no desktop portal available/)).toBeInTheDocument();
  });
});

describe("自動検出へ戻す", () => {
  it("設定値があるときだけボタンを出す", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockResolvedValue({
      journalDir: "/mnt/game/ED",
      configuredJournalDir: "/mnt/game/ED",
      daemonManaged: true,
      configError: null,
      daemonError: null,
      envOverride: false,
    });

    render(<Settings />);

    expect(
      await screen.findByRole("button", { name: "自動検出に戻す" }),
    ).toBeInTheDocument();
  });

  it("設定値が無いときはボタンを出さない", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockResolvedValue({
      journalDir: null,
      configuredJournalDir: null,
      daemonManaged: true,
      configError: null,
      daemonError: null,
      envOverride: false,
    });

    render(<Settings />);
    await screen.findByLabelText("Journal ディレクトリ");

    expect(
      screen.queryByRole("button", { name: "自動検出に戻す" }),
    ).not.toBeInTheDocument();
  });

  it("押すと clear_journal_dir を呼び、入力欄が空になる", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return {
          journalDir: "/mnt/game/ED",
          configuredJournalDir: "/mnt/game/ED",
          daemonManaged: true,
          configError: null,
          daemonError: null,
          envOverride: false,
        };
      }
      if (cmd === "clear_journal_dir") {
        return {
          journalDir: null,
          configuredJournalDir: null,
          daemonManaged: true,
          configError: null,
          daemonError: null,
          envOverride: false,
        };
      }
      throw new Error(`unexpected command: ${cmd}`);
    });

    render(<Settings />);
    await screen.findByDisplayValue("/mnt/game/ED");
    await userEvent.click(screen.getByRole("button", { name: "自動検出に戻す" }));

    expect(mockInvoke).toHaveBeenCalledWith("clear_journal_dir");
    expect(await screen.findByText(/自動検出に戻しました/)).toBeInTheDocument();
    expect(screen.getByLabelText("Journal ディレクトリ")).toHaveValue("");
  });
});

describe("デーモン起動失敗", () => {
  it("daemonError があれば理由を表示する", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockResolvedValue({
      journalDir: null,
      configuredJournalDir: null,
      daemonManaged: true,
      configError: null,
      daemonError: "edlr binary not found (set EDLR_BIN or put edlr on PATH)",
      envOverride: false,
    });

    render(<Settings />);

    expect(await screen.findByText(/edlr binary not found/)).toBeInTheDocument();
  });
});

describe("get_config が失敗したとき", () => {
  it("自動検出に戻すボタンを出さない", async () => {
    // config が null のままなので、保存済みの値があるかどうか分からない。
    // `config?.configuredJournalDir !== null` は undefined !== null で true に
    // なってしまうため、この状態でボタンが出ないことを固定する。
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockRejectedValue(new Error("ipc unavailable"));

    render(<Settings />);
    await screen.findByText(/ipc unavailable/);

    expect(
      screen.queryByRole("button", { name: "自動検出に戻す" }),
    ).not.toBeInTheDocument();
  });
});

describe("デーモン起動失敗からの再試行", () => {
  it("daemonError があれば設定値が無くても再試行できる", async () => {
    // Part 1 の狙いは「起動に失敗しても責任を持ち続け、再試行できる」こと。
    // 保存ボタンは draft === "" で無効、クリアボタンは設定値が無いと非表示、
    // では再試行の経路が無い。
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockResolvedValue({
      journalDir: null,
      configuredJournalDir: null,
      daemonManaged: true,
      configError: null,
      daemonError: "edlr binary not found",
      envOverride: false,
    });

    render(<Settings />);
    await screen.findByText(/edlr binary not found/);

    expect(
      screen.getByRole("button", { name: "デーモンの起動を再試行" }),
    ).toBeEnabled();
  });
});

describe("env override 中のクリア", () => {
  it("自動検出ではなく環境変数が使われ続けると伝える", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return {
          journalDir: "/from/env",
          configuredJournalDir: "/mnt/game/ED",
          daemonManaged: true,
          configError: null,
          daemonError: null,
          envOverride: true,
        };
      }
      return {
        journalDir: "/from/env",
        configuredJournalDir: null,
        daemonManaged: true,
        configError: null,
        daemonError: null,
        envOverride: true,
      };
    });

    render(<Settings />);
    await screen.findByDisplayValue("/mnt/game/ED");
    await userEvent.click(screen.getByRole("button", { name: "自動検出に戻す" }));

    // 「自動検出に戻しました」だけだと嘘になる。env が生きている限り
    // デーモンは env の値を使い続ける。
    expect(await screen.findByText(/引き続き環境変数の値/)).toBeInTheDocument();
  });

  it("env override が無ければ素直に自動検出に戻したと言う", async () => {
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "get_config") {
        return {
          journalDir: "/mnt/game/ED",
          configuredJournalDir: "/mnt/game/ED",
          daemonManaged: true,
          configError: null,
          daemonError: null,
          envOverride: false,
        };
      }
      return {
        journalDir: null,
        configuredJournalDir: null,
        daemonManaged: true,
        configError: null,
        daemonError: null,
        envOverride: false,
      };
    });

    render(<Settings />);
    await screen.findByDisplayValue("/mnt/game/ED");
    await userEvent.click(screen.getByRole("button", { name: "自動検出に戻す" }));

    expect(await screen.findByText(/自動検出に戻しました/)).toBeInTheDocument();
    expect(screen.queryByText(/引き続き環境変数の値/)).not.toBeInTheDocument();
  });
});
