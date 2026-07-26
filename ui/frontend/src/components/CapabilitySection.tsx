import { useEffect, useState } from "react";
import type { Capabilities } from "../types/plugin";

export default function CapabilitySection({
  capabilities,
  onToggle,
}: {
  capabilities: Capabilities;
  onToggle: (granted: boolean) => Promise<void>;
}) {
  const [granted, setGranted] = useState(capabilities.granted);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Keep in sync with the prop when the parent updates it from a fresh
  // server response (e.g. after a successful set-capabilities round-trip).
  useEffect(() => {
    setGranted(capabilities.granted);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [capabilities.granted]);

  if (capabilities.requests.length === 0) {
    return null;
  }

  const toggle = async (next: boolean) => {
    const previous = granted;
    setGranted(next);
    setSaving(true);
    setError(null);
    try {
      await onToggle(next);
    } catch (err) {
      setGranted(previous);
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
        <input
          type="checkbox"
          checked={granted}
          disabled={saving}
          onChange={(e) => toggle(e.target.checked)}
        />
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
