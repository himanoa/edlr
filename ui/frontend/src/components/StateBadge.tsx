import { Badge } from "@/components/ui/badge";

export function StateBadge({
  state,
  reason,
  pending,
}: {
  state: "running" | "disabled";
  reason?: string;
  pending: boolean;
}) {
  if (state === "disabled") {
    return <Badge className="bg-red-950 text-red-400">無効{reason ? `: ${reason}` : ""}</Badge>;
  }
  if (pending) {
    return <Badge className="bg-yellow-950 text-yellow-400">権限承認待ち</Badge>;
  }
  return <Badge className="bg-emerald-950 text-emerald-400">有効</Badge>;
}
