import { Suspense } from "react";
import WidgetFrame from "../components/WidgetFrame";
import { defaultWsUrl, useEventStream } from "../ws";
import type { LogEntry } from "../lib/filter";
import type { WidgetSize } from "../types/plugin";
import { useAtomValue } from "jotai";
import { widgets$ } from "@/store/widgets";
import { ErrorBoundary, FallbackProps } from "react-error-boundary";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { AlertCircle } from "lucide-react";
import { Skeleton } from "@/components/ui/skeleton";

const SPAN: Record<WidgetSize, number> = { small: 1, medium: 2, large: 3 };

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
 * `dashboard/list` で grant 済みウィジェットを取得し、CSS Grid に自動配置
 * する(small=1 / medium=2 / large=3 カラムスパン、並びは登録順)。
 * イベントは親がここで一本の WS(`useEventStream`)から受け、各
 * `WidgetFrame` がプラグインの events フィルタに従って iframe へ転送する。
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
    <section>
      <div className="grid grid-cols-1 gap-4 min-[900px]:grid-cols-3">
        {widgets.map((w) => (
          <article
            key={`${w.plugin}/${w.widget}`}
            className="widget-card min-w-0 rounded-lg border bg-card px-4 py-3 max-[900px]:!col-span-1"
            style={{ gridColumn: `span ${SPAN[w.size]}` }}
          >
            <h2 className="mb-2 text-base font-semibold">{w.title}</h2>
            {w.state !== "running" ? (
              <p className="text-muted-foreground">プラグインが停止しています</p>
            ) : !w.resolved ? (
              <p className="text-muted-foreground">entry ファイルが見つかりません</p>
            ) : (
              <WidgetFrame entry={w} entries={entries} />
            )}
          </article>
        ))}
      </div>
    </section>
  );
}

function DashboardSkeleton() {
  return <Skeleton className="aspect-video w-full" />;
}
