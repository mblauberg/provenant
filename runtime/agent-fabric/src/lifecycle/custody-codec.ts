import { createHash } from "node:crypto";

import { canonicalJson } from "../persistence/row-codec.js";

export type LifecycleCustodyState =
  | "awaiting-boundary"
  | "prepared"
  | "dispatched"
  | "accepted"
  | "ambiguous"
  | "provider-terminal"
  | "committing"
  | "finalized";

export type LifecycleCustodyDisposition =
  | "none"
  | "adopted"
  | "no-effect"
  | "quarantined"
  | "superseded"
  | "abandoned";

export function revisionBody(input: Readonly<{
  custodyId: string;
  revision: number;
  state: LifecycleCustodyState;
  disposition: LifecycleCustodyDisposition;
  proofKind?: string;
  terminalEvidenceDigest: string | null;
}>): Readonly<Record<string, unknown>> {
  return {
    schemaVersion: 1,
    sourceKind: "custody",
    custodyId: input.custodyId,
    revision: input.revision,
    state: input.state,
    disposition: input.disposition,
    proofKind: input.proofKind ?? "none",
    terminalEvidenceDigest: input.terminalEvidenceDigest,
  };
}

export function lifecycleDigest(domain: string, value: unknown): string {
  return `sha256:${createHash("sha256")
    .update(`agent-fabric.lifecycle.v1\0${domain}\0`)
    .update(canonicalJson(value))
    .digest("hex")}`;
}

export function custodyRef(
  runId: string,
  agentId: string,
  custodyId: string,
  revision: number,
): Readonly<Record<string, unknown>> {
  return { schemaVersion: 1, runId, agentId, custodyId, custodyRevision: revision };
}
