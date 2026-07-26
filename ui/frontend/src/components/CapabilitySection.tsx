import { useState } from "react";
import type { Capabilities } from "../types/plugin";

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
    <div className="capability-section">
      <ul className="capability-list">
        {capabilities.requests.map((req, i) => (
          <li key={i} className="capability-request">
            <span className="capability-kind">{req.kind}</span>
            <span className="capability-hosts">{req.hosts.join(", ")}</span>
            <span className="capability-reason">{req.reason}</span>
          </li>
        ))}
      </ul>
      <label className="form-row capability-toggle">
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
        <input
          type="checkbox"
          checked={capabilities.granted}
          disabled={saving}
          onChange={(e) => toggle(e.target.checked)}
        />
        {saving && (
          <span className="capability-pending" role="status">
            確認中…
          </span>
        )}
      </label>
      {!capabilities.granted && (
        <p className="capability-notice">未承認 — このプラグインは外部通信できません</p>
      )}
      {capabilities.staleGrant && (
        <p className="capability-warning">要求が変わったため再承認が必要です</p>
      )}
      {error && <p className="form-error">{error}</p>}
    </div>
  );
}
