import { useState } from "react";
import type { Capabilities } from "../types/plugin";
import { Checkbox } from "@/components/ui/checkbox";

export default function CapabilitySection({
  capabilities,
  onToggle,
}: {
  capabilities: Capabilities;
  onToggle: (granted: boolean) => Promise<void>;
}) {
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (capabilities.requests.length === 0) {
    return null;
  }

  const toggle = async (next: boolean) => {
    setSaving(true);
    setError(null);
    try {
      await onToggle(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="mt-3 border-t border-border pt-3">
      <ul className="m-0 mb-2 list-none p-0 text-[0.85rem]">
        {capabilities.requests.map((req, i) => (
          <li key={i} className="flex flex-wrap gap-2 py-0.5 text-muted-foreground">
            <span className="rounded bg-accent px-1.5 text-sky-400">{req.kind}</span>
            <span className="font-mono">{req.hosts.join(", ")}</span>
            <span>{req.reason}</span>
          </li>
        ))}
      </ul>
      <label className="flex items-center justify-between gap-4 py-1.5">
        <span>外部通信を承認する</span>
        {/*
          `checked` is driven by the confirmed `capabilities.granted` prop,
          never by local optimistic state: a `set-capabilities` RPC that
          never settles (network hang, daemon restart mid-call, ...) must
          not leave the checkbox showing "approved" while the daemon
          actually granted nothing. `saving` only disables the control and
          surfaces a pending indicator; it never fabricates the checked
          state.
        */}
        <Checkbox
          checked={capabilities.granted}
          disabled={saving}
          onCheckedChange={(v) => toggle(v === true)}
        />
        {saving && (
          <span className="ml-1.5 text-xs text-muted-foreground" role="status">
            確認中…
          </span>
        )}
      </label>
      {!capabilities.granted && (
        <p className="mt-1.5 text-sm text-yellow-400">
          未承認 — このプラグインは外部通信できません
        </p>
      )}
      {capabilities.staleGrant && (
        <p className="mt-1.5 text-sm font-semibold text-yellow-400">
          要求が変わったため再承認が必要です
        </p>
      )}
      {error && <p className="mt-1.5 text-sm text-red-400">{error}</p>}
    </div>
  );
}
