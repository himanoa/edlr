import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, test, vi } from "vitest";
import type { RpcClient } from "@/rpc";
import type { ProfilerSeries, ProfilerSummary, SubjectSummary } from "@/store/profiler";
import Profiler from "./Profiler";

function makeSubject(overrides: Partial<SubjectSummary> = {}): SubjectSummary {
  return {
    id: "inara-uploader",
    subject: "plugin",
    calls_1m: 9,
    avg_us_1m: 1069,
    max_us_1m: 3929,
    errors_1m: 0,
    queue_len: 0,
    memory_bytes: 1572864,
    dropped: { busDeliveries: 0, events: 0 },
    ...overrides,
  };
}

const SUMMARY_FIXTURE: ProfilerSummary = {
  profilerLost: 0,
  subjects: [
    makeSubject(),
    makeSubject({ id: "tutorial-jump-log-go", subject: "plugin" }),
  ],
};

const SERIES_FIXTURE: ProfilerSeries = {
  from_ts: 1787068999,
  step: 1,
  points: [
    { calls: 1, errors: 0, avg_us: 10, max_us: 20, queue_len: 0, memory_bytes: 1572864 },
    null,
  ],
};

test("summary をテーブルに描画し、行選択で series を取得する", async () => {
  const client = {
    call: vi.fn(async (method: string) => {
      if (method === "profiler/summary") return SUMMARY_FIXTURE;
      if (method === "profiler/series") return SERIES_FIXTURE;
      throw new Error(`unexpected ${method}`);
    }),
    close: vi.fn(),
  };
  render(<Profiler makeClient={() => client as unknown as Pick<RpcClient, "call" | "close">} />);
  expect(await screen.findByText("inara-uploader")).toBeInTheDocument();
  await userEvent.click(screen.getByText("inara-uploader"));
  await waitFor(() =>
    expect(client.call).toHaveBeenCalledWith(
      "profiler/series",
      expect.objectContaining({ id: "inara-uploader" }),
    ),
  );
});

test("閾値超えの行が警告スタイルになる", async () => {
  const client = {
    call: vi.fn(async (method: string) => {
      if (method === "profiler/summary") {
        return {
          profilerLost: 0,
          subjects: [makeSubject({ id: "slow-plugin", max_us_1m: 2_000_000 })],
        };
      }
      if (method === "profiler/series") return SERIES_FIXTURE;
      throw new Error(`unexpected ${method}`);
    }),
    close: vi.fn(),
  };
  render(<Profiler makeClient={() => client as unknown as Pick<RpcClient, "call" | "close">} />);
  const row = (await screen.findByText("slow-plugin")).closest("tr");
  expect(row).toHaveClass("text-destructive");
});
