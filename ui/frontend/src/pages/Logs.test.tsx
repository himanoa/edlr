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
