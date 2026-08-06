import { Suspense, useEffect, useRef, useState } from "react";
import WidgetFrame from "../components/WidgetFrame";
import { RpcClient } from "../rpc";
import { defaultWsUrl, useEventStream } from "../ws";
import type { DashboardListEntry, WidgetSize } from "../types/plugin";
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
 */
export default function Dashboard() {
  const { entries } = useEventStream(defaultWsUrl());
  const { widgets } = useAtomValue(widgets$)


  return (
    <ErrorBoundary FallbackComponent={LoadErrorFallback}>
      <section>
        <div className="grid grid-cols-1 gap-4 min-[900px]:grid-cols-3">
          <Suspense fallback={<DashboardSkeleton />}>
            {widgets?.map((w) => (
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
          </Suspense>
        </div>
      </section>
    </ErrorBoundary>
  );
}

function DashboardSkeleton() {
  return (
    <Skeleton className="aspect-video w-full" />
  )
}
