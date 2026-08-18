import { useEffect, useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Sparkline } from "@/components/Sparkline";
import { RpcClient } from "@/rpc";
import {
  fetchSeries,
  fetchSummary,
  type ProfilerSeries,
  type SubjectSummary,
} from "@/store/profiler";
import { defaultWsUrl } from "@/ws";

const POLL_MS = 2000;
const MAX_US_WARN = 1_000_000;
const QUEUE_LEN_WARN = 48;

function formatUs(us: number): string {
  if (us >= 1_000_000) return `${(us / 1_000_000).toFixed(2)}s`;
  if (us >= 1_000) return `${(us / 1_000).toFixed(1)}ms`;
  return `${us}µs`;
}

function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${bytes}B`;
}

function isWarn(s: SubjectSummary): boolean {
  return s.max_us_1m > MAX_US_WARN || s.queue_len > QUEUE_LEN_WARN;
}

type Props = {
  makeClient?: () => Pick<RpcClient, "call" | "close">;
};

export default function Profiler({
  makeClient = () => new RpcClient(defaultWsUrl()),
}: Props) {
  const [subjects, setSubjects] = useState<SubjectSummary[]>([]);
  const [profilerLost, setProfilerLost] = useState(0);
  const [selected, setSelected] = useState<SubjectSummary | null>(null);
  const [rangeSeconds, setRangeSeconds] = useState<300 | 3600>(300);
  const [series, setSeries] = useState<ProfilerSeries | null>(null);

  useEffect(() => {
    let cancelled = false;
    const client = makeClient();
    const poll = () => {
      fetchSummary(client).then((res) => {
        if (cancelled) return;
        setSubjects(res.subjects);
        setProfilerLost(res.profilerLost);
      });
    };
    poll();
    const timer = setInterval(poll, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
      client.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!selected) {
      setSeries(null);
      return;
    }
    let cancelled = false;
    const client = makeClient();
    const poll = () => {
      fetchSeries(client, selected.subject, selected.id, rangeSeconds).then((res) => {
        if (!cancelled) setSeries(res);
      });
    };
    poll();
    const timer = setInterval(poll, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
      client.close();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected?.subject, selected?.id, rangeSeconds]);

  const calls = series?.points.map((p) => (p ? p.calls : null)) ?? [];
  const errors = series?.points.map((p) => (p ? p.errors : null)) ?? [];
  const avg = series?.points.map((p) => (p ? p.avg_us : null)) ?? [];
  const max = series?.points.map((p) => (p ? p.max_us : null)) ?? [];
  const queue = series?.points.map((p) => p?.queue_len ?? null) ?? [];
  const memory = series?.points.map((p) => p?.memory_bytes ?? null) ?? [];

  return (
    <section>
      {profilerLost > 0 && (
        <p className="mb-2 text-xs text-destructive">
          プロファイラのイベントを {profilerLost} 件取りこぼしました
        </p>
      )}
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b text-left text-muted-foreground">
            <th className="py-1 pr-2">名前</th>
            <th className="py-1 pr-2">calls/min</th>
            <th className="py-1 pr-2">avg</th>
            <th className="py-1 pr-2">max</th>
            <th className="py-1 pr-2">errors</th>
            <th className="py-1 pr-2">queue</th>
            <th className="py-1 pr-2">dropped</th>
            <th className="py-1 pr-2">memory</th>
          </tr>
        </thead>
        <tbody>
          {subjects.map((s) => (
            <tr
              key={`${s.subject}:${s.id}`}
              className={`cursor-pointer border-b border-border/50 hover:bg-accent/50 ${
                isWarn(s) ? "text-destructive" : ""
              } ${selected?.id === s.id && selected.subject === s.subject ? "bg-accent/50" : ""}`}
              onClick={() => setSelected(s)}
            >
              <td className="py-1 pr-2">
                <Badge variant="secondary" className="mr-1.5">
                  {s.subject === "plugin" ? "plugin" : "driver"}
                </Badge>
                {s.id}
              </td>
              <td className="py-1 pr-2">{s.calls_1m}</td>
              <td className="py-1 pr-2">{formatUs(s.avg_us_1m)}</td>
              <td className="py-1 pr-2">{formatUs(s.max_us_1m)}</td>
              <td className="py-1 pr-2">{s.errors_1m}</td>
              <td className="py-1 pr-2">{s.queue_len}</td>
              <td className="py-1 pr-2">
                {s.dropped.busDeliveries + s.dropped.events}
              </td>
              <td className="py-1 pr-2">{formatBytes(s.memory_bytes)}</td>
            </tr>
          ))}
        </tbody>
      </table>

      {selected && (
        <div className="mt-4">
          <div className="mb-2 flex items-center gap-2">
            <span className="text-sm text-muted-foreground">{selected.id}</span>
            <Button
              variant={rangeSeconds === 300 ? "secondary" : "ghost"}
              size="sm"
              onClick={() => setRangeSeconds(300)}
            >
              5分
            </Button>
            <Button
              variant={rangeSeconds === 3600 ? "secondary" : "ghost"}
              size="sm"
              onClick={() => setRangeSeconds(3600)}
            >
              1時間
            </Button>
          </div>
          <div className="space-y-3">
            <div>
              <p className="text-xs text-muted-foreground">calls / errors</p>
              <Sparkline series={[calls, errors]} colors={["var(--chart-1, #38bdf8)", "var(--destructive, #f87171)"]} />
            </div>
            <div>
              <p className="text-xs text-muted-foreground">avg / max (µs)</p>
              <Sparkline series={[avg, max]} colors={["#a78bfa", "#f87171"]} />
            </div>
            <div>
              <p className="text-xs text-muted-foreground">queue / memory</p>
              <Sparkline series={[queue, memory]} colors={["#34d399", "#fbbf24"]} />
            </div>
          </div>
        </div>
      )}
    </section>
  );
}
