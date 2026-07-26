import type {
  DeclaredRunTaskStateCounts,
  RunProjectionTarget,
} from "@local/agent-fabric-protocol";

import { runProjectionDetailLines } from "./renderer-detail.js";
import type { ConsoleRunProjection } from "./protocol-adapter.js";

export const RUN_DRILL_DOWN_ANSWER_FIELDS = [
  "lead",
  "currentMilestone",
  "currentTask",
  "remainingWork",
  "nextDependency",
  "blockingIssue",
  "evidencePath",
] as const;

export type RunDrillDownAnswerField = typeof RUN_DRILL_DOWN_ANSWER_FIELDS[number];
export type RunDrillDownAnswers = Readonly<Record<RunDrillDownAnswerField, string>>;

export type RunDrillDownFixture = Readonly<{
  id: string;
  session: string;
  target: RunProjectionTarget;
  lead: string;
  phase: string | null;
  health: string | null;
  currentMilestone: string | null;
  nextMilestone: string | null;
  counts: DeclaredRunTaskStateCounts | null;
  task: Readonly<{
    taskId: string;
    state: "blocked" | "ready" | "active" | "complete" | "cancelled" | "degraded";
    objective: string;
    dependencies: readonly string[];
  }>;
  blockingTaskId: string | null;
  evidencePath: string;
  expectedAnswers: RunDrillDownAnswers;
}>;

export type RunDrillDownManifest = Readonly<{
  schemaVersion: 1;
  repetitions: number;
  maximumIdentificationMs: number;
  minimumFieldSuccessRate: number;
  fixtures: readonly RunDrillDownFixture[];
}>;

export type RunDrillDownEvaluationReport = Readonly<{
  observations: number;
  maximumDurationMs: number;
  fieldSuccessRate: number;
  proxyPassed: boolean;
  humanTimingClaim: false;
}>;

function record(value: unknown, path: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError(`${path} must be an object`);
  }
  return value as Record<string, unknown>;
}

function string(value: unknown, path: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${path} must be a non-empty string`);
  }
  return value;
}

function nullableString(value: unknown, path: string): string | null {
  return value === null ? null : string(value, path);
}

function integer(value: unknown, path: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new TypeError(`${path} must be a non-negative safe integer`);
  }
  return value as number;
}

function parseTarget(value: unknown, path: string): RunProjectionTarget {
  const target = record(value, path);
  if (target.kind === "coordination-run") {
    if (Object.keys(target).sort().join(",") !== "coordinationRunId,kind") {
      throw new TypeError(`${path} coordination target is not closed`);
    }
    return {
      kind: "coordination-run",
      coordinationRunId: string(target.coordinationRunId, `${path}.coordinationRunId`) as never,
    };
  }
  if (target.kind === "delivery-workstream") {
    if (
      Object.keys(target).sort().join(",") !==
        "coordinationRunId,deliveryRunId,kind,workstreamId"
    ) {
      throw new TypeError(`${path} delivery target is not closed`);
    }
    return {
      kind: "delivery-workstream",
      coordinationRunId: string(target.coordinationRunId, `${path}.coordinationRunId`) as never,
      deliveryRunId: string(target.deliveryRunId, `${path}.deliveryRunId`) as never,
      workstreamId: string(target.workstreamId, `${path}.workstreamId`) as never,
    };
  }
  throw new TypeError(`${path}.kind is invalid`);
}

function parseCounts(value: unknown, path: string): DeclaredRunTaskStateCounts | null {
  if (value === null) return null;
  const counts = record(value, path);
  const parsed = {
    blocked: integer(counts.blocked, `${path}.blocked`),
    ready: integer(counts.ready, `${path}.ready`),
    active: integer(counts.active, `${path}.active`),
    complete: integer(counts.complete, `${path}.complete`),
    cancelled: integer(counts.cancelled, `${path}.cancelled`),
    degraded: integer(counts.degraded, `${path}.degraded`),
  };
  if (Object.keys(counts).length !== Object.keys(parsed).length) {
    throw new TypeError(`${path} is not closed`);
  }
  return parsed;
}

function parseAnswers(value: unknown, path: string): RunDrillDownAnswers {
  const answers = record(value, path);
  if (
    Object.keys(answers).sort().join(",") !==
      [...RUN_DRILL_DOWN_ANSWER_FIELDS].sort().join(",")
  ) {
    throw new TypeError(`${path} is not closed`);
  }
  return Object.fromEntries(RUN_DRILL_DOWN_ANSWER_FIELDS.map((field) => [
    field,
    string(answers[field], `${path}.${field}`),
  ])) as RunDrillDownAnswers;
}

export function parseRunDrillDownManifest(value: unknown): RunDrillDownManifest {
  const manifest = record(value, "manifest");
  if (manifest.schemaVersion !== 1) throw new TypeError("manifest.schemaVersion must be 1");
  const repetitions = integer(manifest.repetitions, "manifest.repetitions");
  const maximumIdentificationMs = integer(
    manifest.maximumIdentificationMs,
    "manifest.maximumIdentificationMs",
  );
  if (repetitions < 3) throw new TypeError("manifest.repetitions must be at least 3");
  if (maximumIdentificationMs > 20_000) {
    throw new TypeError("manifest.maximumIdentificationMs must be at most 20000");
  }
  if (
    typeof manifest.minimumFieldSuccessRate !== "number" ||
    manifest.minimumFieldSuccessRate < 0.9 ||
    manifest.minimumFieldSuccessRate > 1
  ) {
    throw new TypeError("manifest.minimumFieldSuccessRate must be from 0.9 to 1");
  }
  if (!Array.isArray(manifest.fixtures) || manifest.fixtures.length === 0) {
    throw new TypeError("manifest.fixtures must be non-empty");
  }
  const fixtures = manifest.fixtures.map((value, index): RunDrillDownFixture => {
    const fixture = record(value, `manifest.fixtures[${String(index)}]`);
    const task = record(fixture.task, `manifest.fixtures[${String(index)}].task`);
    const state = string(task.state, `manifest.fixtures[${String(index)}].task.state`);
    if (!["blocked", "ready", "active", "complete", "cancelled", "degraded"].includes(state)) {
      throw new TypeError(`manifest.fixtures[${String(index)}].task.state is invalid`);
    }
    if (!Array.isArray(task.dependencies)) {
      throw new TypeError(`manifest.fixtures[${String(index)}].task.dependencies must be an array`);
    }
    return {
      id: string(fixture.id, `manifest.fixtures[${String(index)}].id`),
      session: string(fixture.session, `manifest.fixtures[${String(index)}].session`),
      target: parseTarget(fixture.target, `manifest.fixtures[${String(index)}].target`),
      lead: string(fixture.lead, `manifest.fixtures[${String(index)}].lead`),
      phase: nullableString(fixture.phase, `manifest.fixtures[${String(index)}].phase`),
      health: nullableString(fixture.health, `manifest.fixtures[${String(index)}].health`),
      currentMilestone: nullableString(
        fixture.currentMilestone,
        `manifest.fixtures[${String(index)}].currentMilestone`,
      ),
      nextMilestone: nullableString(
        fixture.nextMilestone,
        `manifest.fixtures[${String(index)}].nextMilestone`,
      ),
      counts: parseCounts(fixture.counts, `manifest.fixtures[${String(index)}].counts`),
      task: {
        taskId: string(task.taskId, `manifest.fixtures[${String(index)}].task.taskId`),
        state: state as RunDrillDownFixture["task"]["state"],
        objective: string(task.objective, `manifest.fixtures[${String(index)}].task.objective`),
        dependencies: task.dependencies.map((dependency, dependencyIndex) =>
          string(
            dependency,
            `manifest.fixtures[${String(index)}].task.dependencies[${String(dependencyIndex)}]`,
          )
        ),
      },
      blockingTaskId: nullableString(
        fixture.blockingTaskId,
        `manifest.fixtures[${String(index)}].blockingTaskId`,
      ),
      evidencePath: string(
        fixture.evidencePath,
        `manifest.fixtures[${String(index)}].evidencePath`,
      ),
      expectedAnswers: parseAnswers(
        fixture.expectedAnswers,
        `manifest.fixtures[${String(index)}].expectedAnswers`,
      ),
    };
  });
  return {
    schemaVersion: 1,
    repetitions,
    maximumIdentificationMs,
    minimumFieldSuccessRate: manifest.minimumFieldSuccessRate,
    fixtures,
  };
}

function observation<Value>(value: Value | null) {
  return value === null
    ? { observation: "Unobserved" as const }
    : { observation: "Observed" as const, value };
}

export function projectionForRunDrillDownFixture(
  fixture: RunDrillDownFixture,
): ConsoleRunProjection {
  const observedAt = "2026-07-26T00:00:00.000Z";
  const zeroCounts: DeclaredRunTaskStateCounts = {
    blocked: 0,
    ready: 0,
    active: 0,
    complete: 0,
    cancelled: 0,
    degraded: 0,
  };
  const workflowCounts = { ...(fixture.counts ?? zeroCounts) };
  if (workflowCounts[fixture.task.state] === 0) workflowCounts[fixture.task.state] = 1;
  const progress = fixture.counts === null
    ? { observation: "Unobserved" as const }
    : {
        observation: "Observed" as const,
        value: { plan: "open" as const, counts: fixture.counts },
      };
  const scope = fixture.target;
  const taskId = fixture.task.taskId as never;
  return {
    projectSessionId: fixture.session as never,
    target: fixture.target,
    readTransactionId: `run-evaluation:${fixture.id}`,
    composition: {
      projectSessionId: fixture.session as never,
      target: fixture.target,
      identity: {
        freshness: "live",
        source: "fabric",
        revision: 1,
        observedAt: observedAt as never,
        value: {
          acceptedScope: { observation: "Unobserved" },
          currentPlan: { observation: "Unobserved" },
          lead: observation(fixture.lead as never),
          phase: observation(fixture.phase),
          health: observation(fixture.health as never),
          currentMilestone: observation(fixture.currentMilestone),
          nextMilestone: observation(fixture.nextMilestone),
          lastEventAt: { observation: "Unobserved" },
        },
      },
      declaredProgress: {
        freshness: "live",
        source: "fabric",
        revision: 1,
        observedAt: observedAt as never,
        value: progress,
      },
    },
    work: [{
      itemId: fixture.task.taskId,
      itemRevision: 1,
      fact: {
        freshness: "live",
        source: "fabric",
        revision: 1,
        observedAt: observedAt as never,
        value: {
          summary: {
            kind: "work",
            state: fixture.task.state,
            checkState: "passing",
            workflow: {
              workflowRevision: 1,
              objective: { observation: "Observed", value: fixture.task.objective },
              dependencies: {
                observation: "Observed",
                dependencyRevision: 1,
                taskIds: fixture.task.dependencies as never,
              },
              coordinationRun: {
                observation: "Observed",
                projectSessionId: fixture.session as never,
                coordinationRunId: fixture.target.coordinationRunId,
              },
              workstream: fixture.target.kind === "delivery-workstream"
                ? {
                    observation: "Observed",
                    workstreamId: fixture.target.workstreamId,
                    deliveryRunId: fixture.target.deliveryRunId,
                    workstreamRevision: 1,
                    state: "active",
                  }
                : { observation: "Unobserved" },
              parentTask: { observation: "Unobserved" },
              plan: { observation: "Unobserved" },
              task: {
                observation: "Observed",
                state: fixture.task.state,
                owner: { observation: "Unobserved" },
              },
              checks: { observation: "Observed", items: [] },
              barriers: { observation: "Observed", items: [] },
              declaredWriteScopes: { observation: "Observed", leases: [] },
              runTaskStates: { observation: "Observed", counts: workflowCounts },
            },
          },
          detailRef: { kind: "task", taskId, expectedRevision: 1 },
          actionAvailability: { state: "read-only", reason: "state-ineligible" },
        },
      },
    }],
    agents: [],
    evidence: [],
    activity: [],
    issues: fixture.blockingTaskId === null
      ? []
      : [{
          kind: "task",
          scope,
          taskId: fixture.blockingTaskId as never,
          taskRevision: 1,
          state: "blocked",
          detailRef: {
            kind: "task",
            taskId: fixture.blockingTaskId as never,
            expectedRevision: 1,
          },
        }],
    evidencePaths: [{
      path: fixture.evidencePath as never,
      digest: `sha256:${"a".repeat(64)}` as never,
    }],
    evidencePathObservation: "Observed",
    evidencePathsUnobserved: 0,
  };
}

export function evaluateRunDrillDownProxy(
  manifest: RunDrillDownManifest,
): RunDrillDownEvaluationReport {
  let observations = 0;
  let correctFields = 0;
  let totalFields = 0;
  let maximumDurationMs = 0;
  for (const fixture of manifest.fixtures) {
    for (let repetition = 0; repetition < manifest.repetitions; repetition += 1) {
      const started = performance.now();
      const lines = runProjectionDetailLines(
        projectionForRunDrillDownFixture(fixture),
      );
      const durationMs = performance.now() - started;
      maximumDurationMs = Math.max(maximumDurationMs, durationMs);
      observations += 1;
      for (const field of RUN_DRILL_DOWN_ANSWER_FIELDS) {
        totalFields += 1;
        if (answerLines(lines, field).some((line) => line.includes(fixture.expectedAnswers[field]))) {
          correctFields += 1;
        }
      }
    }
  }
  const fieldSuccessRate = totalFields === 0 ? 0 : correctFields / totalFields;
  return {
    observations,
    maximumDurationMs,
    fieldSuccessRate,
    proxyPassed:
      maximumDurationMs <= manifest.maximumIdentificationMs &&
      fieldSuccessRate >= manifest.minimumFieldSuccessRate,
    humanTimingClaim: false,
  };
}

function answerLines(
  lines: readonly string[],
  field: RunDrillDownAnswerField,
): readonly string[] {
  switch (field) {
    case "lead": return lines.filter((line) => line.startsWith("Lead: "));
    case "currentMilestone": return lines.filter((line) => line.startsWith("Current milestone: "));
    case "currentTask": return lines.filter((line) => line.startsWith("Task "));
    case "remainingWork": return lines.filter((line) => line.startsWith("Work states"));
    case "nextDependency": return lines.filter((line) => line.startsWith("Dependency "));
    case "blockingIssue": return lines.filter((line) => line.startsWith("Blocking issue: "));
    case "evidencePath": return lines.filter((line) => line.startsWith("Evidence path: "));
    default: return assertNever(field);
  }
}

function assertNever(value: never): never {
  throw new TypeError(`unexpected run drill-down field: ${String(value)}`);
}

export function scoreRunDrillDownAnswers(
  fixture: RunDrillDownFixture,
  answers: Readonly<Partial<Record<RunDrillDownAnswerField, string>>>,
): number {
  const correct = RUN_DRILL_DOWN_ANSWER_FIELDS.filter(
    (field) => answers[field] === fixture.expectedAnswers[field],
  ).length;
  return correct / RUN_DRILL_DOWN_ANSWER_FIELDS.length;
}
