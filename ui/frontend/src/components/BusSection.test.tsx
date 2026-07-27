import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { BusSection } from "./BusSection";

const base = {
  driver: "ed-state",
  publish: ["ship-status"],
  subscribe: ["current-system"],
  reason: "現在システムを購読するため",
  granted: false,
  staleGrant: false,
  resolved: true,
};

describe("BusSection", () => {
  it("shows the driver, the topics and the reason, with neither badge for a resolved, non-stale grant", () => {
    render(<BusSection pluginId="translator" bus={[base]} onSetGrant={vi.fn()} />);
    expect(screen.getByText("ed-state")).toBeInTheDocument();
    expect(screen.getByText(/ship-status/)).toBeInTheDocument();
    expect(screen.getByText(/current-system/)).toBeInTheDocument();
    expect(screen.getByText(/現在システムを購読するため/)).toBeInTheDocument();
    expect(screen.queryByText("未解決")).not.toBeInTheDocument();
    expect(screen.queryByText("要再承認")).not.toBeInTheDocument();
  });

  it("marks unresolved connections, and only shows that badge when unresolved", () => {
    const unresolved = render(
      <BusSection pluginId="translator" bus={[{ ...base, resolved: false }]} onSetGrant={vi.fn()} />,
    );
    expect(unresolved.getByText("未解決")).toBeInTheDocument();
    unresolved.unmount();

    const resolved = render(
      <BusSection pluginId="translator" bus={[{ ...base, resolved: true }]} onSetGrant={vi.fn()} />,
    );
    expect(resolved.queryByText("未解決")).not.toBeInTheDocument();
  });

  it("marks stale grants as needing re-approval, and only shows that badge when stale", () => {
    const stale = render(
      <BusSection pluginId="translator" bus={[{ ...base, staleGrant: true }]} onSetGrant={vi.fn()} />,
    );
    expect(stale.getByText("要再承認")).toBeInTheDocument();
    stale.unmount();

    const fresh = render(
      <BusSection pluginId="translator" bus={[{ ...base, staleGrant: false }]} onSetGrant={vi.fn()} />,
    );
    expect(fresh.queryByText("要再承認")).not.toBeInTheDocument();
  });

  it("calls onSetGrant when approved", async () => {
    const onSetGrant = vi.fn();
    render(<BusSection pluginId="translator" bus={[base]} onSetGrant={onSetGrant} />);
    await userEvent.click(screen.getByRole("checkbox"));
    expect(onSetGrant).toHaveBeenCalledWith("translator", "ed-state", true);
  });
});
