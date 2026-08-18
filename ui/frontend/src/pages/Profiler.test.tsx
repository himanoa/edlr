import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
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
      expect.objectContaining({ subject: "plugin", id: "inara-uploader" }),
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

function makeSummaryClient() {
  return {
    call: vi.fn(async (method: string) => {
      if (method === "profiler/summary") return SUMMARY_FIXTURE;
      if (method === "profiler/series") return SERIES_FIXTURE;
      throw new Error(`unexpected ${method}`);
    }),
    close: vi.fn(),
  };
}

// フェイクタイマーを使うテストは testing-library の findByText/waitFor(実タイマー依存)
// を避け、vi.waitFor(フェイクタイマーを自動で進める)と同期の getByText を使う。
afterEach(() => vi.useRealTimers());

test("summary は2秒ごとに再取得される", async () => {
  vi.useFakeTimers();
  const client = makeSummaryClient();
  render(<Profiler makeClient={() => client as unknown as Pick<RpcClient, "call" | "close">} />);
  await vi.waitFor(() => expect(client.call).toHaveBeenCalledTimes(1));

  await vi.advanceTimersByTimeAsync(2000);
  expect(client.call).toHaveBeenCalledTimes(2);

  await vi.advanceTimersByTimeAsync(2000);
  expect(client.call).toHaveBeenCalledTimes(3);
});

test("アンマウントで client.close が呼ばれ、以後タイマーが進んでも再取得しない", async () => {
  vi.useFakeTimers();
  const client = makeSummaryClient();
  const { unmount } = render(
    <Profiler makeClient={() => client as unknown as Pick<RpcClient, "call" | "close">} />,
  );
  await vi.waitFor(() => expect(client.call).toHaveBeenCalledTimes(1));

  unmount();
  expect(client.close).toHaveBeenCalledTimes(1);

  const callsAtUnmount = client.call.mock.calls.length;
  await vi.advanceTimersByTimeAsync(10_000);
  expect(client.call).toHaveBeenCalledTimes(callsAtUnmount);
});

test("summary の取得が失敗しても unhandled rejection にならず、次のポーリングで回復する", async () => {
  vi.useFakeTimers();
  let calls = 0;
  const client = {
    call: vi.fn(async (method: string) => {
      if (method !== "profiler/summary") throw new Error(`unexpected ${method}`);
      calls += 1;
      if (calls === 1) throw new Error("daemon down");
      return SUMMARY_FIXTURE;
    }),
    close: vi.fn(),
  };
  render(<Profiler makeClient={() => client as unknown as Pick<RpcClient, "call" | "close">} />);
  await vi.waitFor(() => expect(client.call).toHaveBeenCalledTimes(1));

  await vi.advanceTimersByTimeAsync(2000);
  await vi.waitFor(() => expect(client.call).toHaveBeenCalledTimes(2));
  await vi.waitFor(() => expect(screen.getByText("inara-uploader")).toBeInTheDocument());
});

test("選択行の切り替えで古い series クライアントが close され、新しい対象を取得する", async () => {
  vi.useFakeTimers();
  // makeClient は summary 用に1回(マウント時)、series 用に選択のたびに1回呼ばれる。
  // 呼び出し順で [0]=summary, [1]=1回目の選択, [2]=2回目の選択 と特定できる。
  const clients: ReturnType<typeof makeSummaryClient>[] = [];
  const makeClient = () => {
    const c = makeSummaryClient();
    clients.push(c);
    return c as unknown as Pick<RpcClient, "call" | "close">;
  };

  render(<Profiler makeClient={makeClient} />);
  await vi.waitFor(() => expect(clients[0]?.call).toHaveBeenCalledTimes(1));
  await vi.waitFor(() => expect(screen.getByText("inara-uploader")).toBeInTheDocument());

  fireEvent.click(screen.getByText("inara-uploader"));
  await vi.waitFor(() => expect(clients.length).toBe(2));
  const firstSeriesClient = clients[1];
  await vi.waitFor(() =>
    expect(firstSeriesClient.call).toHaveBeenCalledWith(
      "profiler/series",
      expect.objectContaining({ id: "inara-uploader" }),
    ),
  );

  fireEvent.click(screen.getByText("tutorial-jump-log-go"));
  await vi.waitFor(() => expect(firstSeriesClient.close).toHaveBeenCalledTimes(1));
  await vi.waitFor(() => expect(clients.length).toBe(3));
  const secondSeriesClient = clients[2];
  await vi.waitFor(() =>
    expect(secondSeriesClient.call).toHaveBeenCalledWith(
      "profiler/series",
      expect.objectContaining({ id: "tutorial-jump-log-go" }),
    ),
  );

  // 切り替え後は古いクライアントに対してポーリングが増えない
  const staleCalls = firstSeriesClient.call.mock.calls.length;
  await vi.advanceTimersByTimeAsync(4000);
  expect(firstSeriesClient.call).toHaveBeenCalledTimes(staleCalls);
});
