import type { ProjectSessionState } from "@local/agent-fabric-protocol";

type CoordinationLifecycleDefinition = Readonly<{
  legalTransitions: readonly ProjectSessionState[];
  currentMilestone: string;
  nextMilestone: string | null;
}>;

/** Canonical lifecycle definition consumed by both mutation and projection. */
const COORDINATION_LIFECYCLE = {
  draft: { legalTransitions: ["awaiting_launch"], currentMilestone: "draft", nextMilestone: "awaiting_launch" },
  awaiting_launch: { legalTransitions: ["launching", "launch_failed"], currentMilestone: "awaiting_launch", nextMilestone: "launching" },
  launching: { legalTransitions: ["active", "launch_failed", "launch_ambiguous"], currentMilestone: "launching", nextMilestone: "active" },
  active: { legalTransitions: ["quiescing", "visibility_degraded", "reconciling", "recovery_required", "quarantined"], currentMilestone: "active", nextMilestone: "quiescing" },
  quiescing: { legalTransitions: ["awaiting_acceptance", "active", "reconciling", "recovery_required", "quarantined"], currentMilestone: "quiescing", nextMilestone: "awaiting_acceptance" },
  awaiting_acceptance: { legalTransitions: ["active", "reconciling"], currentMilestone: "awaiting_acceptance", nextMilestone: "closed" },
  closed: { legalTransitions: [], currentMilestone: "closed", nextMilestone: null },
  launch_failed: { legalTransitions: ["awaiting_launch"], currentMilestone: "launch_failed", nextMilestone: "awaiting_launch" },
  launch_ambiguous: { legalTransitions: ["launching", "active", "reconciling", "recovery_required"], currentMilestone: "launch_ambiguous", nextMilestone: "recovery_required" },
  reconciling: { legalTransitions: ["active", "recovery_required", "quarantined"], currentMilestone: "reconciling", nextMilestone: "active" },
  visibility_degraded: { legalTransitions: ["active", "quiescing", "reconciling"], currentMilestone: "visibility_degraded", nextMilestone: "active" },
  recovery_required: { legalTransitions: ["reconciling", "active", "quarantined"], currentMilestone: "recovery_required", nextMilestone: "reconciling" },
  quarantined: { legalTransitions: ["reconciling", "recovery_required"], currentMilestone: "quarantined", nextMilestone: "recovery_required" },
  cancelled: { legalTransitions: [], currentMilestone: "cancelled", nextMilestone: null },
} as const satisfies Readonly<Record<ProjectSessionState, CoordinationLifecycleDefinition>>;

export function legalProjectSessionTransitions(
  state: ProjectSessionState,
): readonly ProjectSessionState[] {
  return COORDINATION_LIFECYCLE[state].legalTransitions;
}

export function projectRunLifecycleFacts(
  phase: string,
): Readonly<{ current: string; next: string | null }> | null {
  if (!Object.hasOwn(COORDINATION_LIFECYCLE, phase)) return null;
  const definition = COORDINATION_LIFECYCLE[phase as ProjectSessionState];
  return {
    current: definition.currentMilestone,
    next: definition.nextMilestone,
  };
}
