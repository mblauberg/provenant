import type { RunProjection } from "@local/agent-fabric-protocol";

export function projectedRunHealth(phase: string): RunProjection["health"] {
  if (phase === "active" || phase === "awaiting_acceptance") return "healthy";
  if (phase === "quarantined") return "quarantined";
  if (phase === "recovery_required" || phase === "launch_ambiguous") return "blocked";
  if (phase === "visibility_degraded" || phase === "reconciling") return "degraded";
  return "unknown";
}

/**
 * The health one observed lifecycle state establishes, or `null` when the state
 * is outside the mapped set. `projectedRunHealth` answers every phase because
 * its plain-field callers require a total function; its `"unknown"` fallback is
 * a default, not an observation. A three-state projection must render that
 * absence as `Unobserved` rather than stamp `Observed` on the default -- the
 * rendered `unknown` differs from the `Unknown`/`ContradictoryFacts` arm by one
 * character of case and asserts something entirely different.
 */
export function observedRunHealth(phase: string): RunProjection["health"] | null {
  const health = projectedRunHealth(phase);
  return health === "unknown" ? null : health;
}

export function projectedRunNextMilestone(phase: string): string {
  if (phase === "active") return "quiescing";
  if (phase === "quiescing") return "awaiting_acceptance";
  if (phase === "awaiting_acceptance") return "closed";
  if (phase === "reconciling" || phase === "launch_ambiguous") return "reconciled state";
  return "next valid lifecycle transition";
}
