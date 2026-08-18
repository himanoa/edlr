import { filterEntries, parseQuery, suggest, type LogEntry } from "./filter";

const entries: LogEntry[] = [
  { id: 1, kind: "journal", timestamp: "t1", event: "FSDJump", raw: { StarSystem: "Sol" } },
  { id: 2, kind: "journal", timestamp: "t2", event: "Docked", raw: { StationName: "Abraham Lincoln" } },
  { id: 3, kind: "status", raw: { Flags: 16777240 } },
];

test("empty query returns everything", () => {
  expect(filterEntries(entries, "")).toHaveLength(3);
  expect(filterEntries(entries, "  ")).toHaveLength(3);
});

test("matches event name case-insensitively", () => {
  expect(filterEntries(entries, "fsdjump").map((e) => e.id)).toEqual([1]);
});

test("matches raw JSON content", () => {
  expect(filterEntries(entries, "lincoln").map((e) => e.id)).toEqual([2]);
  expect(filterEntries(entries, "16777240").map((e) => e.id)).toEqual([3]);
});

test("status entries match by kind name", () => {
  expect(filterEntries(entries, "status").map((e) => e.id)).toEqual([3]);
});

test("matches log entries by message text", () => {
  const log: LogEntry = {
    id: 10,
    kind: "log",
    timestamp: "t",
    level: "info",
    message: "plugin widgety started",
    raw: {},
  };
  expect(filterEntries([log], "widgety")).toHaveLength(1);
  expect(filterEntries([log], "nomatch")).toHaveLength(0);
});

test("parseQuery splits keyed tokens and free text", () => {
  expect(parseQuery("kind:log level:error boom")).toEqual([
    { key: "kind", value: "log" },
    { key: "level", value: "error" },
    { value: "boom" },
  ]);
  // 未知キーや値なしは自由文扱い
  expect(parseQuery("foo:bar kind:")).toEqual([{ value: "foo:bar" }, { value: "kind:" }]);
});

test("keyed tokens match exactly; same key ORs, different keys AND", () => {
  const logs: LogEntry[] = [
    { id: 1, kind: "log", level: "error", message: "boom", raw: {} },
    { id: 2, kind: "log", level: "info", message: "ok", raw: {} },
    { id: 3, kind: "journal", event: "FSDJump", raw: {} },
  ];
  expect(filterEntries(logs, "kind:log").map((e) => e.id)).toEqual([1, 2]);
  expect(filterEntries(logs, "level:error level:info").map((e) => e.id)).toEqual([1, 2]);
  expect(filterEntries(logs, "kind:log level:error").map((e) => e.id)).toEqual([1]);
  // キー付き条件はそのフィールドを持たないエントリを除外する
  expect(filterEntries(logs, "level:error").map((e) => e.id)).toEqual([1]);
  expect(filterEntries(logs, "kind:log boom").map((e) => e.id)).toEqual([1]);
});

test("suggest offers keys, then values for enumerable keys", () => {
  expect(suggest("")).toEqual(["kind:", "level:", "event:", "target:", "driver:", "topic:"]);
  expect(suggest("ki")).toEqual(["kind:"]);
  expect(suggest("kind:")).toEqual(["kind:journal", "kind:status", "kind:log", "kind:bus"]);
  expect(suggest("level:e")).toEqual(["level:error"]);
  expect(suggest("event:")).toEqual([]);
  expect(suggest("nosuchkey:x")).toEqual([]);
});
