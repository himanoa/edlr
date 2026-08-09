import { daemonHttpUrl, defaultWsUrl, parseWsMessage } from "./ws";

test("parses hello", () => {
  expect(parseWsMessage('{"type":"hello","protocol":1}')).toEqual({
    type: "hello",
    protocol: 1,
  });
});

test("parses journal and status events", () => {
  const j = parseWsMessage(
    '{"type":"event","kind":"journal","timestamp":"t","event":"FSDJump","raw":{}}',
  );
  expect(j).toMatchObject({ type: "event", kind: "journal", event: "FSDJump" });
  const s = parseWsMessage('{"type":"event","kind":"status","raw":{"Flags":1}}');
  expect(s).toMatchObject({ type: "event", kind: "status" });
});

test("parses log frames", () => {
  const msg = parseWsMessage(
    JSON.stringify({
      type: "event",
      kind: "log",
      timestamp: "2026-07-28T00:00:00.000Z",
      level: "warn",
      target: "edlr_core::x",
      message: "watch out",
    }),
  );
  expect(msg).toEqual({
    type: "event",
    kind: "log",
    timestamp: "2026-07-28T00:00:00.000Z",
    level: "warn",
    target: "edlr_core::x",
    message: "watch out",
  });
});

test("rejects log frames without level or message", () => {
  expect(
    parseWsMessage(JSON.stringify({ type: "event", kind: "log", timestamp: "t" })),
  ).toBeNull();
  expect(
    parseWsMessage(
      JSON.stringify({ type: "event", kind: "log", timestamp: "t", level: "info" }),
    ),
  ).toBeNull();
});

test("returns null for garbage or unknown types", () => {
  expect(parseWsMessage("not json")).toBeNull();
  expect(parseWsMessage('{"type":"mystery"}')).toBeNull();
  expect(parseWsMessage('{"type":"event","kind":"other","raw":{}}')).toBeNull();
});

test("defaultWsUrl derives from location on a plain http(s) page", () => {
  expect(
    defaultWsUrl({ protocol: "http:", host: "localhost:5173", hostname: "localhost" }),
  ).toBe("ws://localhost:5173/ws");
  expect(
    defaultWsUrl({ protocol: "https:", host: "example.com", hostname: "example.com" }),
  ).toBe("wss://example.com/ws");
});

test("defaultWsUrl falls back to the daemon default under Tauri (tauri.localhost)", () => {
  expect(
    defaultWsUrl({ protocol: "http:", host: "tauri.localhost", hostname: "tauri.localhost" }),
  ).toBe("ws://127.0.0.1:8137/ws");
});

test("defaultWsUrl falls back to the daemon default for a non-http protocol (tauri:)", () => {
  expect(defaultWsUrl({ protocol: "tauri:", host: "", hostname: "" })).toBe(
    "ws://127.0.0.1:8137/ws",
  );
});

test("daemonHttpUrl absolutizes against the page origin on a plain http(s) page", () => {
  expect(
    daemonHttpUrl("/plugin-ui/p/w/index.html", {
      protocol: "http:",
      host: "localhost:5173",
      hostname: "localhost",
    }),
  ).toBe("http://localhost:5173/plugin-ui/p/w/index.html");
});

test("daemonHttpUrl points at the daemon default under Tauri", () => {
  expect(
    daemonHttpUrl("/plugin-ui/p/w/index.html", {
      protocol: "http:",
      host: "tauri.localhost",
      hostname: "tauri.localhost",
    }),
  ).toBe("http://127.0.0.1:8137/plugin-ui/p/w/index.html");
});
