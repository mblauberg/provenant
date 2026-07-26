import type { RunProjection } from "@local/agent-fabric-protocol";

export function projectedRunHealth(phase: string): RunProjection["health"] {
  if (phase === "active" || phase === "awaiting_acceptance") return "healthy";
  if (phase === "quarantined") return "quarantined";
  if (phase === "recovery_required" || phase === "launch_ambiguous") return "blocked";
  if (phase === "visibility_degraded" || phase === "reconciling") return "degraded";
  return "unknown";
}

export function projectedRunNextMilestone(phase: string): string {
  if (phase === "active") return "quiescing";
  if (phase === "quiescing") return "awaiting_acceptance";
  if (phase === "awaiting_acceptance") return "closed";
  if (phase === "reconciling" || phase === "launch_ambiguous") return "reconciled state";
  return "next valid lifecycle transition";
}
