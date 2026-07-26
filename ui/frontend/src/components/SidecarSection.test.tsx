import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import SidecarSection from "./SidecarSection";
import type { Sidecar } from "../types/plugin";

function sidecar(overrides: Partial<Sidecar> = {}): Sidecar {
  return {
    name: "tts",
    reason: "音声合成エンジンをローカルで動かすため",
    args: ["--port", "{port}"],
    port: 50021,
    scalable: true,
    granted: false,
    staleGrant: false,
    config: { command: "", args: ["--port", "{port}"], port: 50021, replicas: 1 },
    instances: [],
    ...overrides,
  };
}

const noop = async () => {};

describe("SidecarSection", () => {
  it("renders nothing when the plugin declares no sidecars", () => {
    const { container } = render(
      <SidecarSection sidecars={[]} onConfigChange={noop} onGrantChange={noop} onControl={noop} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("shows the reason and warns while ungranted", () => {
    render(
      <SidecarSection
        sidecars={[sidecar()]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    expect(screen.getByText(/音声合成エンジン/)).toBeInTheDocument();
    expect(screen.getByText(/未承認 — このプラグインはプロセスを起動できません/)).toBeInTheDocument();
  });

  it("disables the grant toggle until an executable path is set", () => {
    render(
      <SidecarSection
        sidecars={[sidecar()]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    expect(screen.getByRole("checkbox", { name: /このサイドカーを承認する/ })).toBeDisabled();
  });

  it("enables the grant toggle once a command is configured", async () => {
    const onGrantChange = vi.fn(async () => {});
    render(
      <SidecarSection
        sidecars={[sidecar({ config: { command: "/usr/bin/piper", args: [], port: 50021, replicas: 1 } })]}
        onConfigChange={noop}
        onGrantChange={onGrantChange}
        onControl={noop}
      />,
    );
    const toggle = screen.getByRole("checkbox", { name: /このサイドカーを承認する/ });
    expect(toggle).toBeEnabled();
    await userEvent.click(toggle);
    expect(onGrantChange).toHaveBeenCalledWith("tts", true);
  });

  it("keeps the grant toggle disabled while an executable path is only typed but not saved", async () => {
    render(
      <SidecarSection
        sidecars={[sidecar()]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    await userEvent.type(screen.getByLabelText(/実行ファイル/), "/usr/bin/piper");
    expect(screen.getByRole("checkbox", { name: /このサイドカーを承認する/ })).toBeDisabled();
  });

  it("warns that the sidecar runs outside the sandbox", () => {
    render(
      <SidecarSection
        sidecars={[sidecar()]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    expect(screen.getByText(/edlr のサンドボックスの外で動きます/)).toBeInTheDocument();
  });

  it("discloses the network consequence and lists the ports granting would allow", () => {
    render(
      <SidecarSection
        sidecars={[sidecar()]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    const warning = screen.getByTestId("sidecar-network-warning");
    expect(warning).toHaveTextContent("127.0.0.1:50021");
    expect(warning.textContent).toMatch(/通信が.*許可/);
  });

  it("lists every port a multi-replica sidecar would be granted", () => {
    render(
      <SidecarSection
        sidecars={[
          sidecar({
            config: { command: "", args: [], port: 50021, replicas: 3 },
          }),
        ]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    const warning = screen.getByTestId("sidecar-network-warning");
    expect(warning).toHaveTextContent("127.0.0.1:50021");
    expect(warning).toHaveTextContent("127.0.0.1:50022");
    expect(warning).toHaveTextContent("127.0.0.1:50023");
  });

  it("shows a stale-grant warning", () => {
    render(
      <SidecarSection
        sidecars={[sidecar({ staleGrant: true })]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    expect(screen.getByText(/要求が変わったため再承認が必要/)).toBeInTheDocument();
  });

  it("hides the replicas field for non-scalable sidecars", () => {
    render(
      <SidecarSection
        sidecars={[sidecar({ scalable: false })]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    expect(screen.queryByLabelText(/レプリカ数/)).not.toBeInTheDocument();
  });

  it("lists instances with port, state and exit code", () => {
    render(
      <SidecarSection
        sidecars={[
          sidecar({
            granted: true,
            instances: [
              { index: 0, port: 50021, state: "running", exitCode: null },
              { index: 1, port: 50022, state: "exited", exitCode: 1 },
            ],
          }),
        ]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    const instanceList = screen.getByRole("list");
    expect(within(instanceList).getByText(/50021/)).toBeInTheDocument();
    expect(within(instanceList).getByText(/終了コード 1/)).toBeInTheDocument();
  });

  it("sends start/stop/restart control actions", async () => {
    const onControl = vi.fn(async () => {});
    render(
      <SidecarSection
        sidecars={[sidecar({ granted: true, config: { command: "/usr/bin/piper", args: [], port: 50021, replicas: 1 } })]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={onControl}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "起動" }));
    expect(onControl).toHaveBeenCalledWith("tts", "start");
    await userEvent.click(screen.getByRole("button", { name: "停止" }));
    expect(onControl).toHaveBeenCalledWith("tts", "stop");
    await userEvent.click(screen.getByRole("button", { name: "再起動" }));
    expect(onControl).toHaveBeenCalledWith("tts", "restart");
  });

  it("follows a config update coming from the server instead of showing stale form values", () => {
    const { rerender } = render(
      <SidecarSection
        sidecars={[sidecar({ config: { command: "/usr/bin/piper", args: [], port: 50021, replicas: 1 } })]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    expect(screen.getByLabelText(/実行ファイル/)).toHaveValue("/usr/bin/piper");

    // Another client (or a stale-grant refetch) changed the saved config
    // underneath us; the parent re-renders SidecarSection with the new
    // `sidecar.config` prop.
    rerender(
      <SidecarSection
        sidecars={[
          sidecar({
            config: { command: "/usr/bin/other-tts", args: ["--flag"], port: 50099, replicas: 2 },
          }),
        ]}
        onConfigChange={noop}
        onGrantChange={noop}
        onControl={noop}
      />,
    );

    expect(screen.getByLabelText(/実行ファイル/)).toHaveValue("/usr/bin/other-tts");
    expect(screen.getByLabelText(/ポート/)).toHaveValue(50099);
  });

  it("surfaces an error from a rejected config save", async () => {
    const onConfigChange = vi.fn(async () => {
      throw new Error("sidecar tts does not allow replicas > 1");
    });
    render(
      <SidecarSection
        sidecars={[sidecar()]}
        onConfigChange={onConfigChange}
        onGrantChange={noop}
        onControl={noop}
      />,
    );
    await userEvent.type(screen.getByLabelText(/実行ファイル/), "/usr/bin/piper");
    await userEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByText(/does not allow replicas/)).toBeInTheDocument();
  });
});
