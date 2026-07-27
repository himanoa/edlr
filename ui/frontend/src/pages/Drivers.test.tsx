import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { Drivers } from "./Drivers";

const driver = {
  id: "ed-state",
  name: "ED State",
  version: "0.1.0",
  description: "状態を配るドライバ",
  topics: [{ name: "current-system", retain: true, description: "現在のシステム" }],
  settings: [],
  values: {},
  capabilities: { requests: [], granted: false, staleGrant: false },
  sidecars: [],
  filesystem: [],
  state: "running" as const,
};

describe("Drivers page", () => {
  it("lists installed drivers with their topics", () => {
    render(<Drivers drivers={[driver]} driversDir="/home/u/.config/edlr/drivers" onReload={vi.fn()} />);
    expect(screen.getByText("ED State")).toBeInTheDocument();
    expect(screen.getByText("current-system")).toBeInTheDocument();
    expect(screen.getByText(/retain/i)).toBeInTheDocument();
  });

  it("shows an empty state with the drivers dir", () => {
    render(<Drivers drivers={[]} driversDir="/home/u/.config/edlr/drivers" onReload={vi.fn()} />);
    expect(screen.getByText(/\/home\/u\/\.config\/edlr\/drivers/)).toBeInTheDocument();
  });

  it("shows the disabled reason", () => {
    render(
      <Drivers
        drivers={[{ ...driver, state: "disabled" as const, reason: "on-message call failed" }]}
        driversDir="/d"
        onReload={vi.fn()}
      />,
    );
    expect(screen.getByText(/on-message call failed/)).toBeInTheDocument();
  });
});
