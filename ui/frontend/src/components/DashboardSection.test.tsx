import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { DashboardSection } from "./DashboardSection";
import type { DashboardWidget } from "../types/plugin";

const base: DashboardWidget = {
  id: "status",
  title: "Ship Status",
  entry: "ui/status/index.html",
  size: "medium",
  granted: false,
  staleGrant: false,
  resolved: true,
};

describe("DashboardSection", () => {
  it("renders nothing when no widgets are declared", () => {
    const { container } = render(
      <DashboardSection pluginId="p" dashboard={[]} onSetGrant={vi.fn()} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("shows title, size and calls onSetGrant on approve", async () => {
    const onSetGrant = vi.fn().mockResolvedValue(undefined);
    render(<DashboardSection pluginId="p" dashboard={[base]} onSetGrant={onSetGrant} />);
    expect(screen.getByText("Ship Status")).toBeInTheDocument();
    expect(screen.getByText(/medium/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("checkbox"));
    expect(onSetGrant).toHaveBeenCalledWith("p", "status", true);
  });

  it("shows unresolved and stale badges and disables granting unresolved widgets", () => {
    render(
      <DashboardSection
        pluginId="p"
        dashboard={[
          { ...base, resolved: false },
          { ...base, id: "b", title: "B", staleGrant: true },
        ]}
        onSetGrant={vi.fn()}
      />,
    );
    expect(screen.getByText("未解決")).toBeInTheDocument();
    expect(screen.getByText("要再承認")).toBeInTheDocument();
    // 未解決の widget は承認(ON)できない
    const checkboxes = screen.getAllByRole("checkbox");
    expect(checkboxes[0]).toBeDisabled();
    expect(checkboxes[1]).not.toBeDisabled();
  });

  it("allows revoking a granted widget even when unresolved", () => {
    render(
      <DashboardSection
        pluginId="p"
        dashboard={[{ ...base, granted: true, resolved: false }]}
        onSetGrant={vi.fn()}
      />,
    );
    expect(screen.getByRole("checkbox")).not.toBeDisabled();
  });
});
