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
import { driverList$ } from "@/store/driverList";
import { selectedDriverId$ } from "@/store/selectedDriverId";
import { DriverDetail } from "../components/DriverDetail";
import { PluginDetailSkeleton } from "../components/PluginDetail";
import { PluginSidebar, PluginSidebarSkeleton } from "../components/PluginSidebar";

function LoadErrorFallback({ error }: FallbackProps) {
  return (
    <section>
      <Alert variant="destructive">
        <AlertCircle />
        <AlertTitle>ドライバ一覧の取得に失敗しました</AlertTitle>
        <AlertDescription>
          {error instanceof Error ? error.message : String(error)}
        </AlertDescription>
      </Alert>
    </section>
  );
}

export default function Drivers() {
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
        <DriversContent />
      </Suspense>
    </ErrorBoundary>
  );
}

function DriversContent() {
  const { driversDir, drivers } = useAtomValue(driverList$);
  const [selectedId, setSelectedId] = useAtom(selectedDriverId$);

  const selected = drivers.find((d) => d.id === selectedId) ?? drivers[0];

  if (drivers.length === 0) {
    return (
      <section className="h-full">
        <Empty className="h-full">
          <EmptyHeader>
            <EmptyMedia variant="icon">
              <PackageOpen />
            </EmptyMedia>
            <EmptyTitle>ドライバが見つかりませんでした</EmptyTitle>
            <EmptyDescription>
              {driversDir} にドライバを配置してください。
            </EmptyDescription>
          </EmptyHeader>
        </Empty>
      </section>
    );
  }

  // Plugins と同じ全面レイアウト(main の p-4 を打ち消し、余白は各ペイン内側)。
  return (
    <section className="-m-4 flex h-[calc(100%+2rem)]">
      <PluginSidebar plugins={drivers} selectedId={selected?.id} onSelect={setSelectedId} />
      {selected && <DriverDetail driver={selected} />}
    </section>
  );
}
