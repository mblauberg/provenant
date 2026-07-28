import type { CurrentMcpSeatBinding } from "./contracts.js";
import { canonicalJson, sha256 } from "../persistence/row-codec.js";

export type CurrentMcpSeatGenerationIdentity = Readonly<{
  canonicalRoot: string;
  projectSessionId: string;
  sessionRevision: number;
  sessionGeneration: number;
  runId: string;
  runRevision: number;
  chairAgentId: string;
  chairGeneration: number;
  chairLeaseId: string;
  expiresAt: string;
  bindings: readonly CurrentMcpSeatBinding[];
}>;

export function currentMcpSeatGeneration(identity: CurrentMcpSeatGenerationIdentity): Readonly<{
  generation: string;
  bindingJson: string;
}> {
  const bindingJson = canonicalJson({
    ...identity,
    bindings: identity.bindings
      .map((binding) => ({ ...binding }))
      .sort((left, right) => left.seat.localeCompare(right.seat)),
  });
  return { generation: sha256(bindingJson), bindingJson };
}
