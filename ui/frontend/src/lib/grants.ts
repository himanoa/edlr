import type { Capabilities } from "@/types/plugin";

type GrantLike = { granted: boolean; staleGrant: boolean };

/** 権限を要求しているのに未承認(または要求が変わって要再承認)のものがあるか。 */
export function grantsPending(x: {
  capabilities: Capabilities;
  sidecars: GrantLike[];
  filesystem: GrantLike[];
  bus?: GrantLike[];
  dashboard?: GrantLike[];
}): boolean {
  const grants: GrantLike[] = [
    ...(x.capabilities.requests.length > 0 ? [x.capabilities] : []),
    ...x.sidecars,
    ...x.filesystem,
    ...(x.bus ?? []),
    ...(x.dashboard ?? []),
  ];
  return grants.some((g) => !g.granted || g.staleGrant);
}
