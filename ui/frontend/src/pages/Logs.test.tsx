import { act, render } from "@testing-library/react";
import { useEffect, useState } from "react";
import { beforeEach, expect, test, vi } from "vitest";
import type { LogEntry } from "../lib/filter";
import Logs from "./Logs";

const CLIENT_BUFFER_LIMIT = 2000;

let subscribers: Array<() => void> = [];
let mockEntries: LogEntry[] = [];

function setMockEntries(entries: LogEntry[]) {
  mockEntries = entries;
  subscribers.forEach((fn) => fn());
}

vi.mock("../ws", () => ({
  defaultWsUrl: () => "ws://test/ws",
  useEventStream: () => {
    const [, setTick] = useState(0);
    useEffect(() => {
      const fn = () => setTick((t) => t + 1);
      subscribers.push(fn);
      return () => {
        subscribers = subscribers.filter((s) => s !== fn);
      };
    }, []);
    return { entries: mockEntries, connection: "open" as const };
  },
}));

beforeEach(() => {
  mockEntries = [];
  subscribers = [];
});

function makeEntry(id: number): LogEntry {
  return { id, kind: "journal", event: "E", raw: {} };
}

test("auto-scroll re-fires on new entries even after the client buffer hits its cap", () => {
  const scrollIntoView = vi.fn();
  Element.prototype.scrollIntoView = scrollIntoView;

  const initial = Array.from({ length: CLIENT_BUFFER_LIMIT }, (_, i) => makeEntry(i));
  mockEntries = initial;
  render(<Logs />);
  expect(scrollIntoView).toHaveBeenCalledTimes(1);

  // Buffer is at the cap: appending one more entry while dropping the oldest keeps
  // the length constant at CLIENT_BUFFER_LIMIT, but the last entry's id changes.
  const capped = [...initial.slice(1), makeEntry(CLIENT_BUFFER_LIMIT)];
  expect(capped.length).toBe(CLIENT_BUFFER_LIMIT);
  act(() => {
    setMockEntries(capped);
  });

  expect(scrollIntoView).toHaveBeenCalledTimes(2);
});

test("renders log entries with level badge and message", () => {
  mockEntries = [
    { id: 1, kind: "log", timestamp: "t1", level: "warn", message: "watch out", raw: {} },
  ];
  const { getByText } = render(<Logs />);
  expect(getByText("watch out")).toBeTruthy();
  expect(getByText("warn")).toBeTruthy();
});

test("kind filter checkboxes hide unchecked kinds", async () => {
  const userEvent = (await import("@testing-library/user-event")).default;
  mockEntries = [
    { id: 1, kind: "journal", timestamp: "t", event: "FSDJump", raw: {} },
    { id: 2, kind: "log", timestamp: "t", level: "info", message: "daemon log line", raw: {} },
  ];
  const { getByText, queryByText, getByRole } = render(<Logs />);
  expect(getByText("daemon log line")).toBeTruthy();
  await act(async () => {
    await userEvent.click(getByRole("checkbox", { name: "log" }));
  });
  expect(queryByText("daemon log line")).toBeNull();
  expect(getByText("FSDJump")).toBeTruthy();
});
