import { useEffect, useRef, useState } from "react";
import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { filterEntries, type LogEntry } from "../lib/filter";
import { defaultWsUrl, useEventStream, type ConnectionState } from "../ws";

const CONNECTION_STYLE: Record<ConnectionState, string> = {
  open: "bg-emerald-950 text-emerald-400",
  connecting: "bg-yellow-950 text-yellow-400",
  closed: "bg-red-950 text-red-400",
};

function ConnectionBadge({ state }: { state: ConnectionState }) {
  const label = { connecting: "接続中…", open: "接続済み", closed: "切断" }[state];
  return <Badge className={CONNECTION_STYLE[state]}>{label}</Badge>;
}

const KIND_STYLE: Record<string, string> = {
  journal: "text-sky-400",
  status: "text-violet-400",
  log: "text-foreground",
};

const LEVEL_STYLE: Record<string, string> = {
  error: "text-red-400",
  warn: "text-yellow-400",
  info: "text-sky-400",
  debug: "text-muted-foreground",
  trace: "text-muted-foreground",
};

function Row({ entry }: { entry: LogEntry }) {
  const [open, setOpen] = useState(false);
  return (
    <li
      className="flex cursor-pointer flex-wrap gap-3 border-b border-border/50 px-2 py-1 hover:bg-accent/50"
      onClick={() => setOpen((o) => !o)}
    >
      <span className="text-muted-foreground">{entry.timestamp ?? "-"}</span>
      <span className={KIND_STYLE[entry.kind] ?? ""}>{entry.kind}</span>
      {entry.kind === "log" ? (
        <>
          <span className={LEVEL_STYLE[entry.level ?? ""] ?? ""}>{entry.level}</span>
          <span>{entry.message}</span>
        </>
      ) : (
        <span>{entry.event ?? "Status"}</span>
      )}
      {open && (
        <pre className="mt-1 basis-full overflow-x-auto rounded bg-card p-2">
          {JSON.stringify(entry.raw, null, 2)}
        </pre>
      )}
    </li>
  );
}

const KINDS = ["journal", "status", "log"] as const;
type Kind = (typeof KINDS)[number];

const LEVELS = ["error", "warn", "info", "debug", "trace"] as const;
type Level = (typeof LEVELS)[number];

/** kind=log のみレベルで絞る。未知・欠損レベルは隠さず出す(隠すと気付けない)。 */
function levelShown(entry: LogEntry, levels: Record<Level, boolean>): boolean {
  if (entry.kind !== "log") {
    return true;
  }
  const level = entry.level as Level | undefined;
  return level === undefined || !(level in levels) || levels[level];
}

export default function Logs() {
  const { entries, connection } = useEventStream(defaultWsUrl());
  const [query, setQuery] = useState("");
  const [follow, setFollow] = useState(true);
  const [kinds, setKinds] = useState<Record<Kind, boolean>>({
    journal: true,
    status: true,
    log: true,
  });
  const [levels, setLevels] = useState<Record<Level, boolean>>({
    error: true,
    warn: true,
    info: true,
    debug: true,
    trace: true,
  });
  const bottomRef = useRef<HTMLDivElement>(null);
  const shown = filterEntries(entries, query)
    .filter((e) => kinds[e.kind])
    .filter((e) => levelShown(e, levels));

  useEffect(() => {
    if (follow && bottomRef.current?.scrollIntoView) {
      bottomRef.current.scrollIntoView({ behavior: "auto" });
    }
    // shown.length はクライアントバッファが上限(2000件)に達すると増加が止まるため、
    // 末尾エントリの id を依存に使い、上限到達後も新着で再発火させる
  }, [shown[shown.length - 1]?.id, follow]);

  return (
    <section>
      <div className="mb-2 flex flex-wrap items-center gap-3 text-sm">
        <Input
          className="w-72"
          placeholder="フィルタ(イベント名・内容)"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        {KINDS.map((k) => (
          <label key={k} className="flex items-center gap-1.5">
            <input
              type="checkbox"
              aria-label={k}
              checked={kinds[k]}
              onChange={(e) => setKinds((prev) => ({ ...prev, [k]: e.target.checked }))}
            />
            {k}
          </label>
        ))}
        {LEVELS.map((lv) => (
          <label key={lv} className={`flex items-center gap-1.5 ${LEVEL_STYLE[lv]}`}>
            <input
              type="checkbox"
              aria-label={lv}
              checked={levels[lv]}
              onChange={(e) => setLevels((prev) => ({ ...prev, [lv]: e.target.checked }))}
            />
            {lv}
          </label>
        ))}
        <label className="flex items-center gap-1.5">
          <input
            type="checkbox"
            checked={follow}
            onChange={(e) => setFollow(e.target.checked)}
          />
          自動スクロール
        </label>
        <ConnectionBadge state={connection} />
        <span className="ml-auto text-muted-foreground">
          {shown.length} / {entries.length} 件
        </span>
      </div>
      <ul className="m-0 list-none p-0 font-mono text-[0.85rem]">
        {shown.map((e) => (
          <Row key={e.id} entry={e} />
        ))}
      </ul>
      <div ref={bottomRef} />
    </section>
  );
}
