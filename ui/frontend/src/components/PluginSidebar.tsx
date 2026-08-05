import { Skeleton } from "@/components/ui/skeleton";
import { grantsPending } from "@/lib/grants";
import type { PluginInfo } from "../types/plugin";

/** 一覧表示に必要な最小形。PluginInfo / DriverInfo のどちらも満たす。 */
export type SidebarItem = Pick<
  PluginInfo,
  "id" | "name" | "description" | "state" | "capabilities" | "sidecars" | "filesystem"
> &
  Partial<Pick<PluginInfo, "bus" | "dashboard">>;

function dotColor(item: SidebarItem): string {
  if (item.state === "disabled") return "bg-red-400";
  if (grantsPending(item)) return "bg-yellow-400";
  return "bg-emerald-400";
}

type Props = {
  plugins: SidebarItem[];
  selectedId: string | undefined;
  onSelect: (id: string) => void;
};

export function PluginSidebarSkeleton() {
  return (
    <div className="w-64 shrink-0 space-y-2 border-r p-3">
      <Skeleton className="h-12 w-full" />
      <Skeleton className="h-12 w-full" />
      <Skeleton className="h-12 w-full" />
    </div>
  );
}

export function PluginSidebar({ plugins, selectedId, onSelect }: Props) {
  return (
    <nav className="w-64 shrink-0 overflow-y-auto border-r p-3">
      <ul className="m-0 list-none space-y-1 p-0">
        {plugins.map((p) => (
          <li key={p.id}>
            <button
              type="button"
              onClick={() => onSelect(p.id)}
              aria-current={p.id === selectedId}
              className={`w-full rounded-md px-3 py-2 text-left ${
                p.id === selectedId ? "bg-accent" : "hover:bg-accent/50"
              }`}
            >
              <span className="flex items-center gap-2">
                <span aria-hidden className={`size-2 shrink-0 rounded-full ${dotColor(p)}`} />
                <span className="truncate font-medium">{p.name}</span>
              </span>
              <span className="mt-0.5 block truncate text-xs text-muted-foreground">
                {p.description}
              </span>
            </button>
          </li>
        ))}
      </ul>
    </nav>
  );
}
