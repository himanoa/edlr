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
  it("shows the driver, the topics and the reason", () => {
    render(<BusSection pluginId="translator" bus={[base]} onSetGrant={vi.fn()} />);
    expect(screen.getByText("ed-state")).toBeInTheDocument();
    expect(screen.getByText(/ship-status/)).toBeInTheDocument();
    expect(screen.getByText(/current-system/)).toBeInTheDocument();
    expect(screen.getByText(/現在システムを購読するため/)).toBeInTheDocument();
  });

  it("marks unresolved connections", () => {
    render(
      <BusSection pluginId="translator" bus={[{ ...base, resolved: false }]} onSetGrant={vi.fn()} />,
    );
    expect(screen.getByText("未解決")).toBeInTheDocument();
  });

  it("marks stale grants as needing re-approval", () => {
    render(
      <BusSection pluginId="translator" bus={[{ ...base, staleGrant: true }]} onSetGrant={vi.fn()} />,
    );
    expect(screen.getByText("要再承認")).toBeInTheDocument();
  });

  it("calls onSetGrant when approved", async () => {
    const onSetGrant = vi.fn();
    render(<BusSection pluginId="translator" bus={[base]} onSetGrant={onSetGrant} />);
    await userEvent.click(screen.getByRole("checkbox"));
    expect(onSetGrant).toHaveBeenCalledWith("translator", "ed-state", true);
  });
});
