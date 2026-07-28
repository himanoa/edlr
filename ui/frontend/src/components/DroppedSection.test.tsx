import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { DroppedSection } from "./DroppedSection";

describe("DroppedSection", () => {
  it("renders nothing when nothing has been dropped", () => {
    const { container } = render(<DroppedSection dropped={{ events: 0, busDeliveries: 0 }} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("reports dropped journal events", () => {
    render(<DroppedSection dropped={{ events: 12, busDeliveries: 0 }} />);
    expect(screen.getByText(/12 journal events/)).toBeInTheDocument();
    expect(screen.queryByText(/bus deliveries/)).not.toBeInTheDocument();
  });

  it("reports dropped bus deliveries", () => {
    render(<DroppedSection dropped={{ events: 0, busDeliveries: 3 }} />);
    expect(screen.getByText(/3 bus deliveries/)).toBeInTheDocument();
    expect(screen.queryByText(/journal events/)).not.toBeInTheDocument();
  });

  it("reports both kinds together", () => {
    render(<DroppedSection dropped={{ events: 12, busDeliveries: 3 }} />);
    expect(screen.getByText(/12 journal events, 3 bus deliveries/)).toBeInTheDocument();
  });

  it("says the dropped work is not replayed", () => {
    render(<DroppedSection dropped={{ events: 1, busDeliveries: 0 }} />);
    expect(screen.getByText(/not replayed/)).toBeInTheDocument();
  });
});
