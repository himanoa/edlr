import { useEffect, useRef, useState } from "react";
import { Badge } from "@/components/ui/badge";
import {
  filterByTokens,
  parseQuery,
  suggest,
  tokenLabel,
  type LogEntry,
  type QueryToken,
} from "../lib/filter";
import { defaultWsUrl, useEventStream, type ConnectionState } from "../ws";
import { Checkbox } from "@/components/ui/checkbox";

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
  bus: "text-emerald-400",
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

/** Datadog 風クエリバー: 確定したトークンは Badge、入力中テキストは即時フィルタ。 */
function QueryBar({
  tokens,
  setTokens,
  text,
  setText,
}: {
  tokens: QueryToken[];
  setTokens: (t: QueryToken[]) => void;
  text: string;
  setText: (s: string) => void;
}) {
  const [focused, setFocused] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const suggestions = focused ? suggest(text) : [];

  const commit = (raw: string) => {
    const parsed = parseQuery(raw);
    if (parsed.length > 0) setTokens([...tokens, ...parsed]);
    setText("");
  };

  const applySuggestion = (s: string) => {
    if (s.endsWith(":")) {
      setText(s);
      inputRef.current?.focus();
    } else {
      commit(s);
    }
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if ((e.key === "Enter" || e.key === " ") && text.trim()) {
      e.preventDefault();
      commit(text);
    } else if (e.key === "Backspace" && text === "" && tokens.length > 0) {
      // Badge は文字単位ではなく丸ごと消す
      setTokens(tokens.slice(0, -1));
    } else if (e.key === "Escape") {
      inputRef.current?.blur();
    }
  };

  return (
    <div className="relative min-w-72 flex-1">
      <div
        className="flex min-h-9 flex-wrap items-center gap-1 rounded-md border border-input bg-transparent px-2 py-1 shadow-xs focus-within:border-ring focus-within:ring-[3px] focus-within:ring-ring/50"
        onClick={() => inputRef.current?.focus()}
      >
        {tokens.map((t, i) => (
          <Badge key={`${tokenLabel(t)}-${i}`} variant="secondary" className="gap-1 font-mono">
            {tokenLabel(t)}
            <button
              type="button"
              aria-label={`remove ${tokenLabel(t)}`}
              className="text-muted-foreground hover:text-foreground"
              onClick={(e) => {
                e.stopPropagation();
                setTokens(tokens.filter((_, j) => j !== i));
              }}
            >
              ×
            </button>
          </Badge>
        ))}
        <input
          ref={inputRef}
          className="min-w-32 flex-1 bg-transparent text-sm outline-none placeholder:text-muted-foreground"
          placeholder={tokens.length === 0 ? "kind:log level:error 自由文…" : ""}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={onKeyDown}
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
          aria-label="フィルタクエリ"
        />
      </div>
      {suggestions.length > 0 && (
        <ul
          role="listbox"
          className="absolute z-20 mt-1 w-56 rounded-md border bg-popover p-1 font-mono text-sm text-popover-foreground shadow-md"
        >
          {suggestions.map((s) => (
            <li key={s}>
              <button
                type="button"
                role="option"
                aria-selected={false}
                className="w-full rounded px-2 py-1 text-left hover:bg-accent"
                // blur より先に発火させて focus を保つ
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => applySuggestion(s)}
              >
                {s}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export default function Logs() {
  const { entries, connection } = useEventStream(defaultWsUrl());
  const [tokens, setTokens] = useState<QueryToken[]>([]);
  const [text, setText] = useState("");
  const [follow, setFollow] = useState(true);
  const bottomRef = useRef<HTMLDivElement>(null);
  const shown = filterByTokens(entries, [...tokens, ...parseQuery(text)]);

  useEffect(() => {
    if (follow && bottomRef.current?.scrollIntoView) {
      bottomRef.current.scrollIntoView({ behavior: "auto" });
    }
    // shown.length はクライアントバッファが上限(2000件)に達すると増加が止まるため、
    // 末尾エントリの id を依存に使い、上限到達後も新着で再発火させる
  }, [shown[shown.length - 1]?.id, follow]);

  return (
    <section>
      <div className="sticky top-0 z-10 -mx-4 -mt-4 mb-2 flex flex-wrap items-center gap-3 border-b bg-background px-4 py-2 text-sm">
        <QueryBar tokens={tokens} setTokens={setTokens} text={text} setText={setText} />
        <label className="flex items-center gap-1.5">
          <Checkbox
            checked={follow}
            onCheckedChange={(v) => setFollow(v === true)}
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
