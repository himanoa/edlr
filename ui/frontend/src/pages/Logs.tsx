import { useEffect, useRef, useState } from "react";
import { filterEntries, type LogEntry } from "../lib/filter";
import { defaultWsUrl, useEventStream, type ConnectionState } from "../ws";

function ConnectionBadge({ state }: { state: ConnectionState }) {
  const label = { connecting: "接続中…", open: "接続済み", closed: "切断" }[state];
  return <span className={`badge badge-${state}`}>{label}</span>;
}

function Row({ entry }: { entry: LogEntry }) {
  const [open, setOpen] = useState(false);
  return (
    <li className="log-row" onClick={() => setOpen((o) => !o)}>
      <span className="log-time">{entry.timestamp ?? "-"}</span>
      <span className={`log-kind log-kind-${entry.kind}`}>{entry.kind}</span>
      {entry.kind === "log" ? (
        <>
          <span className={`log-level log-level-${entry.level}`}>{entry.level}</span>
          <span className="log-message">{entry.message}</span>
        </>
      ) : (
        <span className="log-event">{entry.event ?? "Status"}</span>
      )}
      {open && <pre className="log-raw">{JSON.stringify(entry.raw, null, 2)}</pre>}
    </li>
  );
}

const KINDS = ["journal", "status", "log"] as const;
type Kind = (typeof KINDS)[number];

export default function Logs() {
  const { entries, connection } = useEventStream(defaultWsUrl());
  const [query, setQuery] = useState("");
  const [follow, setFollow] = useState(true);
  const [kinds, setKinds] = useState<Record<Kind, boolean>>({
    journal: true,
    status: true,
    log: true,
  });
  const bottomRef = useRef<HTMLDivElement>(null);
  const shown = filterEntries(entries, query).filter((e) => kinds[e.kind]);

  useEffect(() => {
    if (follow && bottomRef.current?.scrollIntoView) {
      bottomRef.current.scrollIntoView({ behavior: "auto" });
    }
    // shown.length はクライアントバッファが上限(2000件)に達すると増加が止まるため、
    // 末尾エントリの id を依存に使い、上限到達後も新着で再発火させる
  }, [shown[shown.length - 1]?.id, follow]);

  return (
    <section className="logs">
      <div className="logs-toolbar">
        <input
          placeholder="フィルタ(イベント名・内容)"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        {KINDS.map((k) => (
          <label key={k} className="logs-kind-toggle">
            <input
              type="checkbox"
              aria-label={k}
              checked={kinds[k]}
              onChange={(e) => setKinds((prev) => ({ ...prev, [k]: e.target.checked }))}
            />
            {k}
          </label>
        ))}
        <label>
          <input
            type="checkbox"
            checked={follow}
            onChange={(e) => setFollow(e.target.checked)}
          />
          自動スクロール
        </label>
        <ConnectionBadge state={connection} />
        <span className="logs-count">{shown.length} / {entries.length} 件</span>
      </div>
      <ul className="log-list">
        {shown.map((e) => (
          <Row key={e.id} entry={e} />
        ))}
      </ul>
      <div ref={bottomRef} />
    </section>
  );
}
