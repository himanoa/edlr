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
  // ツールバーのレベルトグルにも "warn" があるので、バッジの span に限定する。
  expect(getByText("warn", { selector: "span" })).toBeTruthy();
});

test("committing a kind:journal query as a badge hides other kinds", async () => {
  const userEvent = (await import("@testing-library/user-event")).default;
  mockEntries = [
    { id: 1, kind: "journal", timestamp: "t", event: "FSDJump", raw: {} },
    { id: 2, kind: "log", timestamp: "t", level: "info", message: "daemon log line", raw: {} },
  ];
  const { getByText, queryByText, getByLabelText } = render(<Logs />);
  expect(getByText("daemon log line")).toBeTruthy();
  const input = getByLabelText("フィルタクエリ");
  await act(async () => {
    await userEvent.type(input, "kind:journal{Enter}");
  });
  // 確定したトークンは Badge になり、入力欄は空になる
  expect(getByText("kind:journal")).toBeTruthy();
  expect((input as HTMLInputElement).value).toBe("");
  expect(queryByText("daemon log line")).toBeNull();
  expect(getByText("FSDJump")).toBeTruthy();
});

test("backspace on empty input removes the whole last badge", async () => {
  const userEvent = (await import("@testing-library/user-event")).default;
  mockEntries = [
    { id: 1, kind: "journal", timestamp: "t", event: "FSDJump", raw: {} },
    { id: 2, kind: "log", timestamp: "t", level: "info", message: "daemon log line", raw: {} },
  ];
  const { getByText, queryByText, getByLabelText } = render(<Logs />);
  const input = getByLabelText("フィルタクエリ");
  await act(async () => {
    await userEvent.type(input, "kind:journal{Enter}");
  });
  expect(queryByText("daemon log line")).toBeNull();
  await act(async () => {
    await userEvent.type(input, "{Backspace}");
  });
  expect(queryByText("kind:journal")).toBeNull();
  expect(getByText("daemon log line")).toBeTruthy();
});

test("focusing the input shows key suggestions and clicking one fills the input", async () => {
  const userEvent = (await import("@testing-library/user-event")).default;
  mockEntries = [];
  const { getByLabelText, getByRole, queryByRole } = render(<Logs />);
  const input = getByLabelText("フィルタクエリ") as HTMLInputElement;
  expect(queryByRole("listbox")).toBeNull();
  await act(async () => {
    await userEvent.click(input);
  });
  expect(getByRole("listbox")).toBeTruthy();
  await act(async () => {
    await userEvent.click(getByRole("option", { name: "kind:" }));
  });
  expect(input.value).toBe("kind:");
  // キー確定後は値のサジェストに切り替わる
  expect(getByRole("option", { name: "kind:journal" })).toBeTruthy();
});
