import { parseWsMessage } from "./ws";

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

test("returns null for garbage or unknown types", () => {
  expect(parseWsMessage("not json")).toBeNull();
  expect(parseWsMessage('{"type":"mystery"}')).toBeNull();
  expect(parseWsMessage('{"type":"event","kind":"other","raw":{}}')).toBeNull();
});
