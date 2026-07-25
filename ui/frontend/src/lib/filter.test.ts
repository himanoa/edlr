import { filterEntries, type LogEntry } from "./filter";

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
