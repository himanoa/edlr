import { useAtom, useAtomValue } from "jotai";
import { AlertCircle, PackageOpen } from "lucide-react";
import { Suspense } from "react";
import { ErrorBoundary, type FallbackProps } from "react-error-boundary";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { pluginList$ } from "@/store/pluginList";
import { selectedPluginId$ } from "@/store/selectedPluginId";
import { PluginDetail, PluginDetailSkeleton } from "../components/PluginDetail";
import { PluginSidebar, PluginSidebarSkeleton } from "../components/PluginSidebar";

function LoadErrorFallback({ error }: FallbackProps) {
  return (
    <section>
      <Alert variant="destructive">
        <AlertCircle />
        <AlertTitle>プラグイン一覧の取得に失敗しました</AlertTitle>
        <AlertDescription>
          {error instanceof Error ? error.message : String(error)}
        </AlertDescription>
      </Alert>
    </section>
  );
}

export default function Plugins() {
  return (
    <ErrorBoundary FallbackComponent={LoadErrorFallback}>
      <Suspense
        fallback={
          <section role="status" className="-m-4 flex h-[calc(100%+2rem)]">
            <span className="sr-only">読み込み中…</span>
            <PluginSidebarSkeleton />
            <PluginDetailSkeleton />
          </section>
        }
      >
        <PluginsEmpty />
      </Suspense>
    </ErrorBoundary>
  );
}

function PluginsEmpty() {
  const { pluginsDir, plugins } = useAtomValue(pluginList$);
  const [selectedId, setSelectedId] = useAtom(selectedPluginId$);

  const selected = plugins.find((p) => p.id === selectedId) ?? plugins[0];

  if (plugins.length === 0) {
    return (
      <section className="h-full">
        <Empty className="h-full">
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <PackageOpen />
            </EmptyMedia>
            <EmptyTitle>プラグインが見つかりませんでした</EmptyTitle>
            <EmptyDescription>
              {pluginsDir} にプラグインを配置してください。
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      </section>
    );
  }

  // main の p-4 を打ち消して全面に広げる。サイドバーの縦線を上下いっぱいに
  // 通し、article のスクロールバーを右端に付けるため。余白は各ペインの内側。
  return (
    <section className="-m-4 flex h-[calc(100%+2rem)]">
      <PluginSidebar plugins={plugins} selectedId={selected?.id} onSelect={setSelectedId} />
      {selected && <PluginDetail plugin={selected} />}
    </section>
  );
}
