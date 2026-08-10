import { Suspense, useState, type RefObject } from "react";
import ReactGridLayout, { useContainerWidth } from "react-grid-layout";
import "react-grid-layout/css/styles.css";
import WidgetHost from "../components/WidgetHost";
import { defaultWsUrl, useEventStream } from "../ws";
import type { LogEntry } from "../lib/filter";
import {
  GRID_COLS,
  loadLayout,
  mergeLayout,
  saveLayout,
  type LayoutItem,
} from "../lib/widgetLayout";
import { useAtomValue } from "jotai";
import { widgets$ } from "@/store/widgets";
import { ErrorBoundary, FallbackProps } from "react-error-boundary";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { AlertCircle } from "lucide-react";
import { Skeleton } from "@/components/ui/skeleton";

function LoadErrorFallback({ error }: FallbackProps) {
  return (
    <section>
      <Alert variant="destructive">
        <AlertCircle />
        <AlertTitle>ダッシュボードの取得に失敗しました</AlertTitle>
        <AlertDescription>
          {error instanceof Error ? error.message : String(error)}
        </AlertDescription>
      </Alert>
    </section>
  );
}

/**
 * プラグイン拡張可能なダッシュボード。
 *
 * `dashboard/list` で grant 済みウィジェットを取得し、react-grid-layout の
 * グリッドに配置する。タイトルバーのドラッグで再配置、右下ハンドルでリサイズ
 * でき、配置は localStorage に保存する(manifest `size` は初期幅のみに使う)。
 * イベントは親がここで一本の WS(`useEventStream`)から受け、各
 * `WidgetHost` がプラグインの events フィルタに従ってウィジェットへ配る。
 *
 * widgets$ の読み取りはサスペンドするため、Suspense/ErrorBoundary の内側の
 * WidgetGrid で行う(この階層で読むと自分の境界では捕まえられない)。
 */
export default function Dashboard() {
  const { entries } = useEventStream(defaultWsUrl());

  return (
    <ErrorBoundary FallbackComponent={LoadErrorFallback}>
      <Suspense fallback={<DashboardSkeleton />}>
        <WidgetGrid entries={entries} />
      </Suspense>
    </ErrorBoundary>
  );
}

function WidgetGrid({ entries }: { entries: LogEntry[] }) {
  const { widgets } = useAtomValue(widgets$);
  const { width, containerRef, mounted } = useContainerWidth();
  const [layout, setLayout] = useState(() =>
    mergeLayout(
      loadLayout(),
      widgets.map((w) => ({ key: `${w.plugin}/${w.widget}`, size: w.size })),
    ),
  );

  if (widgets.length === 0) {
    return (
      <section>
        <p className="text-sm text-muted-foreground">
          承認済みのウィジェットがありません。Plugins 画面でウィジェットを承認すると、ここに表示されます。
        </p>
      </section>
    );
  }

  return (
    // RGL v2 の ref は React 19 スタイル(`RefObject<T | null>`)なので
    // React 18 の型に合わせてキャストする
    <section ref={containerRef as RefObject<HTMLElement>}>
      {mounted && (
        <ReactGridLayout
          layout={layout}
          width={width}
          gridConfig={{ cols: GRID_COLS, rowHeight: 80 }}
          // カード全体をドラッグ可能にするとウィジェット内の操作と衝突する
          // ため、掴めるのはタイトルバーだけにする
          dragConfig={{ handle: ".widget-drag-handle" }}
          onLayoutChange={(next) => {
            const items: LayoutItem[] = next.map(({ i, x, y, w, h }) => ({ i, x, y, w, h }));
            setLayout(items);
            saveLayout(items);
          }}
        >
          {widgets.map((w) => (
            <article
              key={`${w.plugin}/${w.widget}`}
              className="widget-card flex min-w-0 flex-col overflow-hidden rounded-lg border bg-card px-4 py-3"
            >
              <h2 className="widget-drag-handle mb-2 cursor-move text-base font-semibold">
                {w.title}
              </h2>
              {w.state !== "running" ? (
                <p className="text-muted-foreground">プラグインが停止しています</p>
              ) : !w.resolved ? (
                <p className="text-muted-foreground">entry ファイルが見つかりません</p>
              ) : (
                <WidgetHost entry={w} entries={entries} />
              )}
            </article>
          ))}
        </ReactGridLayout>
      )}
    </section>
  );
}

function DashboardSkeleton() {
  return <Skeleton className="aspect-video w-full" />;
}
