import type { RpcClient } from "@/rpc";

export type SubjectSummary = {
  id: string;
  subject: "plugin" | "driver";
  calls_1m: number;
  avg_us_1m: number;
  max_us_1m: number;
  errors_1m: number;
  queue_len: number;
  memory_bytes: number;
  dropped: { busDeliveries: number; events: number };
};

export type ProfilerSummary = {
  profilerLost: number;
  subjects: SubjectSummary[];
};

export type SeriesPoint = {
  calls: number;
  errors: number;
  avg_us: number;
  max_us: number;
  queue_len: number | null;
  memory_bytes: number | null;
} | null;

export type ProfilerSeries = {
  from_ts: number;
  step: number;
  points: SeriesPoint[];
};

export function fetchSummary(
  client: Pick<RpcClient, "call">,
): Promise<ProfilerSummary> {
  return client.call<ProfilerSummary>("profiler/summary");
}

export function fetchSeries(
  client: Pick<RpcClient, "call">,
  subject: "plugin" | "driver",
  id: string,
  seconds: number,
): Promise<ProfilerSeries> {
  return client.call<ProfilerSeries>("profiler/series", { subject, id, seconds });
}
