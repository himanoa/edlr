import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import FilesystemSection from "./FilesystemSection";
import type { FilesystemRoot } from "../types/plugin";

function root(overrides: Partial<FilesystemRoot> = {}): FilesystemRoot {
  return {
    name: "exports",
    reason: "巡回した星系の一覧を CSV で書き出すため",
    mode: "read-write",
    granted: false,
    staleGrant: false,
    config: { path: "" },
    ...overrides,
  };
}

const noop = async () => {};

describe("FilesystemSection", () => {
  it("renders nothing when the plugin declares no roots", () => {
    const { container } = render(
      <FilesystemSection roots={[]} onConfigChange={noop} onGrantChange={noop} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("shows the reason and the ungranted notice", () => {
    render(<FilesystemSection roots={[root()]} onConfigChange={noop} onGrantChange={noop} />);
    expect(screen.getByText(/CSV で書き出すため/)).toBeInTheDocument();
    expect(
      screen.getByText(/未承認 — このプラグインはファイルにアクセスできません/),
    ).toBeInTheDocument();
  });

  it("warns about write access for read-write roots", () => {
    render(<FilesystemSection roots={[root()]} onConfigChange={noop} onGrantChange={noop} />);
    expect(screen.getByText(/読み取り・作成・上書き・削除できます/)).toBeInTheDocument();
  });

  it("warns about read-only access for read roots", () => {
    render(
      <FilesystemSection roots={[root({ mode: "read" })]} onConfigChange={noop} onGrantChange={noop} />,
    );
    expect(screen.getByText(/読み取れます/)).toBeInTheDocument();
    expect(screen.queryByText(/上書き・削除/)).not.toBeInTheDocument();
  });

  it("disables the grant toggle until a directory is saved", async () => {
    render(<FilesystemSection roots={[root()]} onConfigChange={noop} onGrantChange={noop} />);
    const toggle = screen.getByRole("checkbox", { name: /このフォルダへのアクセスを承認する/ });
    expect(toggle).toBeDisabled();

    await userEvent.type(screen.getByLabelText("フォルダ"), "/home/u/exports");
    expect(toggle).toBeDisabled();
  });

  it("enables the grant toggle once the daemon has the directory", async () => {
    const onGrantChange = vi.fn(async () => {});
    render(
      <FilesystemSection
        roots={[root({ config: { path: "/home/u/exports" } })]}
        onConfigChange={noop}
        onGrantChange={onGrantChange}
      />,
    );
    const toggle = screen.getByRole("checkbox", { name: /このフォルダへのアクセスを承認する/ });
    expect(toggle).toBeEnabled();
    await userEvent.click(toggle);
    expect(onGrantChange).toHaveBeenCalledWith("exports", true);
  });

  it("shows a stale-grant warning", () => {
    render(
      <FilesystemSection roots={[root({ staleGrant: true })]} onConfigChange={noop} onGrantChange={noop} />,
    );
    expect(screen.getByText(/要求が変わったため再承認が必要/)).toBeInTheDocument();
  });

  it("surfaces an error from a rejected config save", async () => {
    const onConfigChange = vi.fn(async () => {
      throw new Error("protected directory");
    });
    render(
      <FilesystemSection roots={[root()]} onConfigChange={onConfigChange} onGrantChange={noop} />,
    );
    await userEvent.type(screen.getByLabelText("フォルダ"), "/etc");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByText(/protected directory/)).toBeInTheDocument();
  });
});
