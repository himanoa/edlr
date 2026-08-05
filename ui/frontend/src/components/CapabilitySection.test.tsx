import { act, render, screen } from "@testing-library/react";
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

test("shows a staleGrant warning together with the ungranted notice when the request changed", () => {
  // The server never emits `granted: true` alongside `staleGrant: true` --
  // `GrantsStore::state` always reports `{ granted: false, stale: true }`
  // for a changed request set (see core/src/plugin/grants.rs). Exercising
  // the actual shape the server sends catches regressions where the two
  // notices are wired as mutually exclusive when they must render together.
  const capabilities = makeCapabilities({ granted: false, staleGrant: true });
  render(<CapabilitySection capabilities={capabilities} onToggle={vi.fn()} />);

  expect(screen.getByText(/未承認/)).toBeInTheDocument();
  expect(screen.getByText(/外部通信できません/)).toBeInTheDocument();
  expect(screen.getByText(/再承認/)).toBeInTheDocument();
});

test("toggling the approval switch calls onToggle(true)", async () => {
  const onToggle = vi.fn().mockResolvedValue(undefined);
  const capabilities = makeCapabilities({ granted: false });
  render(<CapabilitySection capabilities={capabilities} onToggle={onToggle} />);

  await userEvent.click(screen.getByRole("checkbox"));

  expect(onToggle).toHaveBeenCalledWith(true);
});

test("a never-settling onToggle leaves the checkbox reflecting the confirmed prop, not optimistic local state", async () => {
  let resolveToggle: (() => void) | undefined;
  const onToggle = vi.fn(
    () =>
      new Promise<void>((resolve) => {
        resolveToggle = resolve;
      }),
  );
  const capabilities = makeCapabilities({ granted: false });
  render(<CapabilitySection capabilities={capabilities} onToggle={onToggle} />);

  const toggle = screen.getByRole("checkbox") as HTMLInputElement;
  await userEvent.click(toggle);

  // The RPC is in flight and has not resolved: the checkbox must still
  // reflect the last confirmed server state (ungranted), not a hopeful
  // "approved" flash, and the control should be disabled with a pending
  // indicator visible.
  expect(toggle).not.toBeChecked();
  expect(toggle.disabled).toBe(true);
  expect(screen.getByRole("status")).toBeInTheDocument();

  await act(async () => {
    resolveToggle?.();
    await Promise.resolve();
  });
});

test("a rejecting onToggle surfaces an error and reverts the toggle", async () => {
  const onToggle = vi.fn().mockRejectedValue(new Error("承認に失敗しました"));
  const capabilities = makeCapabilities({ granted: false });
  render(<CapabilitySection capabilities={capabilities} onToggle={onToggle} />);

  const toggle = screen.getByRole("checkbox") as HTMLInputElement;
  expect(toggle).not.toBeChecked();

  await userEvent.click(toggle);

  expect(await screen.findByText("承認に失敗しました")).toBeInTheDocument();
  expect(toggle).not.toBeChecked();
});
