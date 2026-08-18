export interface LogEntry {
  id: number;
  kind: "journal" | "status" | "log" | "bus";
  timestamp?: string;
  event?: string;
  /** kind === "log" のみ: info | warn | error */
  level?: string;
  /** kind === "log" のみ: 整形済みログ本文(key=value フィールド込み) */
  message?: string;
  /** kind === "log" のみ: 発生元モジュールパス */
  target?: string;
  /** kind === "bus" のみ: 送信元ドライバ id / トピック名 / UTF-8 payload */
  driver?: string;
  topic?: string;
  payload?: string;
  raw: unknown;
}

/** クエリ DSL で使えるキー。`kind:log` のように書く。 */
export const QUERY_KEYS = ["kind", "level", "event", "target", "driver", "topic"] as const;
export type QueryKey = (typeof QUERY_KEYS)[number];

/** キーごとの既知の値(サジェスト用)。列挙できないキーは空。 */
export const QUERY_VALUES: Record<QueryKey, readonly string[]> = {
  kind: ["journal", "status", "log", "bus"],
  level: ["error", "warn", "info", "debug", "trace"],
  event: [],
  target: [],
  driver: [],
  topic: [],
};

export interface QueryToken {
  /** undefined なら自由文(全文の部分一致) */
  key?: QueryKey;
  value: string;
}

function isQueryKey(s: string): s is QueryKey {
  return (QUERY_KEYS as readonly string[]).includes(s);
}

/** "kind:log 自由文" を空白区切りでトークン列にする。未知キーは自由文扱い。 */
export function parseQuery(query: string): QueryToken[] {
  return query
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .map((word) => {
      const i = word.indexOf(":");
      if (i > 0) {
        const key = word.slice(0, i).toLowerCase();
        const value = word.slice(i + 1);
        if (isQueryKey(key) && value) return { key, value };
      }
      return { value: word };
    });
}

export function tokenLabel(t: QueryToken): string {
  return t.key ? `${t.key}:${t.value}` : t.value;
}

function field(e: LogEntry, key: QueryKey): string | undefined {
  return e[key];
}

function matchesFreeText(e: LogEntry, q: string): boolean {
  const name = (e.event ?? e.kind).toLowerCase();
  if (name.includes(q)) return true;
  if (e.kind === "log" && e.message?.toLowerCase().includes(q)) return true;
  return JSON.stringify(e.raw).toLowerCase().includes(q);
}

/**
 * Datadog 風のセマンティクス:
 * 同一キーのトークンは OR、キーをまたぐと AND、自由文は全て AND(部分一致)。
 * キー付きトークンは大文字小文字を無視した完全一致。
 */
export function filterByTokens(entries: LogEntry[], tokens: QueryToken[]): LogEntry[] {
  if (tokens.length === 0) return entries;
  const byKey = new Map<QueryKey, string[]>();
  const freeTexts: string[] = [];
  for (const t of tokens) {
    if (t.key) {
      const list = byKey.get(t.key) ?? [];
      list.push(t.value.toLowerCase());
      byKey.set(t.key, list);
    } else {
      freeTexts.push(t.value.toLowerCase());
    }
  }
  return entries.filter((e) => {
    for (const [key, values] of byKey) {
      const v = field(e, key)?.toLowerCase();
      if (v === undefined || !values.includes(v)) return false;
    }
    return freeTexts.every((q) => matchesFreeText(e, q));
  });
}

export function filterEntries(entries: LogEntry[], query: string): LogEntry[] {
  return filterByTokens(entries, parseQuery(query));
}

/**
 * 入力途中のテキストに対するサジェスト。
 * - "kin" → ["kind:"](キー候補)
 * - "kind:" / "kind:lo" → ["kind:log", ...](値候補)
 */
export function suggest(text: string): string[] {
  const t = text.trim().toLowerCase();
  const i = t.indexOf(":");
  if (i > 0) {
    const key = t.slice(0, i);
    if (!isQueryKey(key)) return [];
    const prefix = t.slice(i + 1);
    return QUERY_VALUES[key]
      .filter((v) => v.startsWith(prefix) && v !== prefix)
      .map((v) => `${key}:${v}`);
  }
  return QUERY_KEYS.filter((k) => k.startsWith(t)).map((k) => `${k}:`);
}
