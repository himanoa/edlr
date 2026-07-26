import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, test, vi } from "vitest";
import type { Capabilities } from "../types/plugin";
import CapabilitySection from "./CapabilitySection";

function makeCapabilities(overrides: Partial<Capabilities> = {}): Capabilities {
  return {
    requests: [{ kind: "http", hosts: ["api.example.com"], reason: "天気を取得するため" }],
    granted: false,
    staleGrant: false,
    ...overrides,
  };
}

test("renders nothing when there are no requests", () => {
  const capabilities = makeCapabilities({ requests: [], granted: true });
  const { container } = render(
    <CapabilitySection capabilities={capabilities} onToggle={vi.fn()} />,
  );
  expect(container).toBeEmptyDOMElement();
});

test("shows the kind, hosts, and reason for each request", () => {
  const capabilities = makeCapabilities({
    requests: [
      { kind: "http", hosts: ["api.example.com", "cdn.example.com"], reason: "天気を取得するため" },
    ],
  });
  render(<CapabilitySection capabilities={capabilities} onToggle={vi.fn()} />);

  expect(screen.getByText(/http/)).toBeInTheDocument();
  expect(screen.getByText(/api\.example\.com/)).toBeInTheDocument();
  expect(screen.getByText(/cdn\.example\.com/)).toBeInTheDocument();
  expect(screen.getByText(/天気を取得するため/)).toBeInTheDocument();
});

test("shows an ungranted notice when not granted", () => {
  const capabilities = makeCapabilities({ granted: false });
  render(<CapabilitySection capabilities={capabilities} onToggle={vi.fn()} />);

  expect(screen.getByText(/未承認/)).toBeInTheDocument();
  expect(screen.getByText(/外部通信できません/)).toBeInTheDocument();
});

test("shows a staleGrant warning when the request changed", () => {
  const capabilities = makeCapabilities({ granted: true, staleGrant: true });
  render(<CapabilitySection capabilities={capabilities} onToggle={vi.fn()} />);

  expect(screen.getByText(/再承認/)).toBeInTheDocument();
});

test("toggling the approval switch calls onToggle(true)", async () => {
  const onToggle = vi.fn().mockResolvedValue(undefined);
  const capabilities = makeCapabilities({ granted: false });
  render(<CapabilitySection capabilities={capabilities} onToggle={onToggle} />);

  await userEvent.click(screen.getByRole("checkbox"));

  expect(onToggle).toHaveBeenCalledWith(true);
});

test("a rejecting onToggle surfaces an error and reverts the toggle", async () => {
  const onToggle = vi.fn().mockRejectedValue(new Error("承認に失敗しました"));
  const capabilities = makeCapabilities({ granted: false });
  render(<CapabilitySection capabilities={capabilities} onToggle={onToggle} />);

  const toggle = screen.getByRole("checkbox") as HTMLInputElement;
  expect(toggle.checked).toBe(false);

  await userEvent.click(toggle);

  expect(await screen.findByText("承認に失敗しました")).toBeInTheDocument();
  expect(toggle.checked).toBe(false);
});
