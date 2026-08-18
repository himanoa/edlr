const VIEW_WIDTH = 100;

type Props = {
  series: (number | null)[][];
  colors: string[];
  height?: number;
};

/** 系列ごとに null で分割した折れ線を SVG polyline で描く小さなチャート。軸・凡例は持たない。 */
export function Sparkline({ series, colors, height = 32 }: Props) {
  return (
    <svg
      viewBox={`0 0 ${VIEW_WIDTH} ${height}`}
      preserveAspectRatio="none"
      width="100%"
      height={height}
      role="img"
    >
      {series.map((points, si) => {
        // 系列ごとに min/max を独立して正規化する。桁の違う系列(queue と
        // memory 等)を同じスケールに載せると小さい方が潰れるため。
        const values = points.filter((v): v is number => v !== null);
        const max = values.length > 0 ? Math.max(...values, 0) : 1;
        const min = values.length > 0 ? Math.min(...values, 0) : 0;
        const range = max - min || 1;
        const toY = (v: number) => height - ((v - min) / range) * height;

        const n = points.length;
        const step = n > 1 ? VIEW_WIDTH / (n - 1) : 0;
        const runs: string[] = [];
        let current: string[] = [];
        points.forEach((v, i) => {
          if (v === null) {
            if (current.length > 0) {
              runs.push(current.join(" "));
              current = [];
            }
            return;
          }
          current.push(`${i * step},${toY(v)}`);
        });
        if (current.length > 0) runs.push(current.join(" "));

        return runs.map((pts, ri) => (
          <polyline
            key={`${si}-${ri}`}
            points={pts}
            fill="none"
            stroke={colors[si] ?? "currentColor"}
            strokeWidth={1.5}
            vectorEffect="non-scaling-stroke"
          />
        ));
      })}
    </svg>
  );
}
