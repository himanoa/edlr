import { render } from "@testing-library/react";
import { expect, test } from "vitest";
import { Sparkline } from "./Sparkline";

test("null 点で polyline が分割される", () => {
  const { container } = render(
    <Sparkline series={[[1, 2, null, 3, 4]]} colors={["red"]} />,
  );
  const polylines = container.querySelectorAll("polyline");
  // null を挟んで [1,2] と [3,4] の2本の連続区間に分割される
  expect(polylines.length).toBe(2);
});

test("null を含まない系列は 1 本の polyline になる", () => {
  const { container } = render(
    <Sparkline series={[[1, 2, 3]]} colors={["red"]} />,
  );
  expect(container.querySelectorAll("polyline").length).toBe(1);
});
