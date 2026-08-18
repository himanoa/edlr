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

test("桁違いの2系列でも各系列が独立に正規化され、両方とも可視域に描画される", () => {
  const { container } = render(
    <Sparkline series={[[0, 48, 0], [0, 4_000_000, 1_572_864]]} colors={["red", "blue"]} />,
  );
  const polylines = container.querySelectorAll("polyline");
  expect(polylines.length).toBe(2);
  const ys = (el: Element) =>
    (el.getAttribute("points") ?? "")
      .split(" ")
      .map((p) => Number(p.split(",")[1]));
  // 小さい方(queue 相当)の系列も、大きい方に潰されず y 座標にばらつきが残る
  const smallSeriesYs = ys(polylines[0]);
  expect(new Set(smallSeriesYs).size).toBeGreaterThan(1);
});
