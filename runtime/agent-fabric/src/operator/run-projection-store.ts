import type {
  ProjectId,
  ProjectSessionId,
  ProjectionFact,
  RunIssueReference,
  RunProjectionPageEntry,
  RunProjectionPageRequest,
  RunProjectionPageResult,
  RunProjectionSection,
  RunScopedComposition,
  Timestamp,
  OperatorViewRow,
} from "@local/agent-fabric-protocol";
import { parseIdentifier, parseTimestamp } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import {
  integer,
  isRow,
  nullableText,
  row,
  text,
  type Row,
} from "../project-session/store-support.js";
import { projectDeclaredRunProgress } from "./declared-run-progress-projection.js";
import { projectRunIdentity } from "./run-identity-projection.js";
import { projectedRunHealth, projectedRunNextMilestone } from "./run-lifecycle-projection.js";
import type { AuthenticatedOperatorCredential } from "./store.js";
import {
  activityBudgetExceededRow,
  boundedActivityPagePrefix,
} from "./activity-projection.js";

export type RunProjectionStoreContext = Readonly<{
  database: Database.Database;
  clock: () => number;
  globalRevision: () => number;
  eventCursor: (projectId: ProjectId, projectSessionId: ProjectSessionId) => number;
  workRows: (
    projectId: ProjectId,
    projectSessionId: ProjectSessionId,
    authenticated: AuthenticatedOperatorCredential,
  ) => OperatorViewRow<"work">[];
  agentRows: (
    projectId: ProjectId,
    projectSessionId: ProjectSessionId,
    authenticated: AuthenticatedOperatorCredential,
  ) => OperatorViewRow<"agents">[];
  evidenceRows: (
    projectId: ProjectId,
    projectSessionId: ProjectSessionId,
    authenticated: AuthenticatedOperatorCredential,
  ) => OperatorViewRow<"evidence">[];
  activityRows: (
    projectId: ProjectId,
    projectSessionId: ProjectSessionId,
    authenticated: AuthenticatedOperatorCredential,
  ) => OperatorViewRow<"activity">[];
}>;

export function projectRunPage<Section extends RunProjectionSection>(
  context: RunProjectionStoreContext,
  request: RunProjectionPageRequest<Section>,
  authenticated: AuthenticatedOperatorCredential,
): RunProjectionPageResult<Section> {
  assertPageBounds(request.cursor, request.limit);
  const read = context.database.transaction((): RunProjectionPageResult<Section> => {
    const currentSnapshotRevision = context.globalRevision();
    if (request.snapshotRevision !== currentSnapshotRevision) {
      return {
        status: "resnapshot-required",
        projectSessionId: request.projectSessionId,
        target: request.target,
        section: request.section,
        reason: "snapshot-mismatch",
        currentSnapshotRevision,
        snapshotCursor: context.eventCursor(request.projectId, request.projectSessionId),
      } as RunProjectionPageResult<Section>;
    }
    const run = runTargetRow(context.database, request);
    const workRows = runScopedWorkRows(context, request, authenticated);
    const values = runSectionValues(context, request, run, workRows, authenticated);
    const requestedValues = values.slice(
      request.cursor,
      request.cursor + request.limit,
    );
    const composition = runScopedComposition(
      context,
      request,
      run,
      currentSnapshotRevision,
    );
    const entriesFor = (selected: readonly unknown[]) =>
      selected.map((value) => ({
        runScope: request.target,
        value,
      })) as unknown as readonly RunProjectionPageEntry<Section>[];
    const resultFor = (selected: readonly unknown[]) => ({
      status: "page",
      projectSessionId: request.projectSessionId,
      target: request.target,
      section: request.section,
      entries: entriesFor(selected),
      nextCursor: request.cursor + selected.length,
      hasMore: request.cursor + selected.length < values.length,
      snapshotRevision: currentSnapshotRevision,
      readTransactionId: `projection:${request.projectId}:${request.projectSessionId}:${String(currentSnapshotRevision)}`,
      composition,
    });
    const selected = request.section === "activity"
      ? boundedActivityPagePrefix(requestedValues, resultFor, {
          jsonValueFor: entriesFor,
          firstOverflow: (value) => activityBudgetExceededRow(
            value as OperatorViewRow<"activity">,
          ),
        })
      : requestedValues;
    return resultFor(selected) as RunProjectionPageResult<Section>;
  });
  return read();
}

function runTargetRow(
  database: Database.Database,
  request: RunProjectionPageRequest,
): Row {
  const run = oneScopedRow(database, `
    SELECT r.* FROM runs r
    JOIN project_sessions s ON s.project_session_id=r.project_session_id
    WHERE s.project_id=? AND r.project_session_id=? AND r.run_id=?
  `, [
    request.projectId,
    request.projectSessionId,
    request.target.coordinationRunId,
  ], "run projection target");
  if (request.target.kind === "delivery-workstream") {
    oneScopedRow(database, `
      SELECT workstream_id FROM workstreams
      WHERE project_session_id=? AND coordination_run_id=?
        AND delivery_run_id=? AND workstream_id=?
    `, [
      request.projectSessionId,
      request.target.coordinationRunId,
      request.target.deliveryRunId,
      request.target.workstreamId,
    ], "delivery workstream projection target");
  }
  return run;
}

function runScopedComposition(
  context: RunProjectionStoreContext,
  request: RunProjectionPageRequest,
  run: Row,
  snapshotRevision: number,
): RunScopedComposition {
  const runRevision = integer(run, "revision");
  const identity = projectRunIdentity(context.database, run);
  const observedAt = toTimestamp(context.clock(), "runScopedComposition.observedAt");
  if (request.target.kind === "delivery-workstream") {
    const target = request.target;
    const matches = identity.workstreams.filter((workstream) =>
      workstream.workstreamId === target.workstreamId &&
      workstream.deliveryRunId === target.deliveryRunId
    );
    if (matches.length !== 1) {
      throw new ProjectFabricCoreError(
        matches.length === 0 ? "NOT_FOUND" : "CONFLICT",
        "delivery workstream identity is not exact",
      );
    }
    const workstream = matches[0]!;
    return {
      projectSessionId: request.projectSessionId,
      target: request.target,
      identity: liveFact(snapshotRevision, workstream.updatedAt, {
        acceptedScope: { observation: "Unobserved" },
        currentPlan: { observation: "Unobserved" },
        lead: { observation: "Observed", value: workstream.leadAgentId },
        phase: { observation: "Unobserved" },
        health: { observation: "Unobserved" },
        currentMilestone: { observation: "Unobserved" },
        nextMilestone: { observation: "Unobserved" },
        lastEventAt: { observation: "Unobserved" },
      }),
      declaredProgress: liveFact(snapshotRevision, workstream.updatedAt, {
        observation: "Unobserved",
      }),
    };
  }
  const phase = text(run, "lifecycle_state");
  return {
    projectSessionId: request.projectSessionId,
    target: request.target,
    identity: liveFact(runRevision, observedAt, {
      acceptedScope: identity.acceptedScopeRef === null
        ? { observation: "Unobserved" }
        : { observation: "Observed", value: identity.acceptedScopeRef },
      currentPlan: identity.currentPlanRef === null || identity.planRevision === null
        ? { observation: "Unobserved" }
        : {
            observation: "Observed",
            value: {
              artifactRef: identity.currentPlanRef,
              planRevision: identity.planRevision,
            },
          },
      lead: { observation: "Observed", value: identity.chairAgentId },
      phase: { observation: "Observed", value: phase },
      health: { observation: "Observed", value: projectedRunHealth(phase) },
      currentMilestone: { observation: "Unobserved" },
      nextMilestone: { observation: "Observed", value: projectedRunNextMilestone(phase) },
      lastEventAt: identity.lastEventAt === null
        ? { observation: "Unobserved" }
        : { observation: "Observed", value: identity.lastEventAt },
    }),
    declaredProgress: liveFact(runRevision, observedAt, {
      observation: "Observed",
      value: projectDeclaredRunProgress(context.database, request.target.coordinationRunId),
    }),
  };
}

function runScopedWorkRows(
  context: RunProjectionStoreContext,
  request: RunProjectionPageRequest,
  authenticated: AuthenticatedOperatorCredential,
): OperatorViewRow<"work">[] {
  return context.workRows(
    request.projectId,
    request.projectSessionId,
    authenticated,
  ).filter((work) => projectionFactValues(work.fact).some((candidate) => {
    const workflow = candidate.summary.workflow;
    if (
      workflow === undefined ||
      workflow.coordinationRun.projectSessionId !== request.projectSessionId ||
      workflow.coordinationRun.coordinationRunId !== request.target.coordinationRunId
    ) {
      return false;
    }
    if (request.target.kind === "coordination-run") return true;
    return (
      workflow.workstream.observation === "Observed" &&
      workflow.workstream.workstreamId === request.target.workstreamId &&
      workflow.workstream.deliveryRunId === request.target.deliveryRunId
    );
  }));
}

function runSectionValues<Section extends RunProjectionSection>(
  context: RunProjectionStoreContext,
  request: RunProjectionPageRequest<Section>,
  run: Row,
  workRows: readonly OperatorViewRow<"work">[],
  authenticated: AuthenticatedOperatorCredential,
): readonly unknown[] {
  switch (request.section) {
    case "work": return workRows;
    case "agents": return runScopedAgentRows(context, request, run, workRows, authenticated);
    case "evidence": return runScopedEvidenceRows(context, request, workRows, authenticated);
    case "activity": return runScopedActivityRows(context, request, workRows, authenticated);
    case "issues": return runIssueReferences(context, request, workRows, authenticated);
    default: return assertNever(request.section);
  }
}

function runScopedAgentRows(
  context: RunProjectionStoreContext,
  request: RunProjectionPageRequest,
  run: Row,
  workRows: readonly OperatorViewRow<"work">[],
  authenticated: AuthenticatedOperatorCredential,
): OperatorViewRow<"agents">[] {
  const agentIds = new Set<string>();
  if (request.target.kind === "coordination-run") {
    for (const value of context.database.prepare(
      "SELECT agent_id FROM agents WHERE run_id=?",
    ).all(request.target.coordinationRunId)) {
      agentIds.add(text(row(value, "run projection agent"), "agent_id"));
    }
  } else {
    agentIds.add(text(run, "chair_agent_id"));
    const workstream = row(context.database.prepare(`
      SELECT lead_agent_id FROM workstreams
      WHERE project_session_id=? AND coordination_run_id=?
        AND delivery_run_id=? AND workstream_id=?
    `).get(
      request.projectSessionId,
      request.target.coordinationRunId,
      request.target.deliveryRunId,
      request.target.workstreamId,
    ), "run projection workstream");
    agentIds.add(text(workstream, "lead_agent_id"));
    for (const value of context.database.prepare(`
      WITH RECURSIVE scoped_teams(team_id) AS (
        SELECT custody.team_id
          FROM workstream_custody custody
          JOIN workstreams stream ON stream.workstream_id=custody.workstream_id
         WHERE stream.project_session_id=? AND stream.coordination_run_id=?
           AND stream.delivery_run_id=? AND stream.workstream_id=?
        UNION
        SELECT child.team_id
          FROM teams child
          JOIN scoped_teams parent ON child.parent_team_id=parent.team_id
         WHERE child.run_id=?
      )
      SELECT membership.agent_id
        FROM team_members membership
        JOIN scoped_teams scoped ON scoped.team_id=membership.team_id
       WHERE membership.run_id=?
      UNION
      SELECT team.leader_agent_id
        FROM teams team
        JOIN scoped_teams scoped ON scoped.team_id=team.team_id
       WHERE team.run_id=?
      ORDER BY agent_id
    `).all(
      request.projectSessionId,
      request.target.coordinationRunId,
      request.target.deliveryRunId,
      request.target.workstreamId,
      request.target.coordinationRunId,
      request.target.coordinationRunId,
      request.target.coordinationRunId,
    )) {
      agentIds.add(text(row(value, "run projection workstream team agent"), "agent_id"));
    }
    for (const work of workRows) {
      for (const candidate of projectionFactValues(work.fact)) {
        const owner = candidate.summary.workflow?.task.owner;
        if (owner?.observation === "Observed") agentIds.add(owner.agentId);
      }
    }
  }
  const roleOrder = { chair: 0, lead: 1, worker: 2, reviewer: 3 } as const;
  return context.agentRows(
    request.projectId,
    request.projectSessionId,
    authenticated,
  ).filter((agent) => agentIds.has(agent.itemId)).sort((left, right) => {
    const leftValue = projectionFactValues(left.fact)[0];
    const rightValue = projectionFactValues(right.fact)[0];
    const leftRole = leftValue?.summary.role ?? "worker";
    const rightRole = rightValue?.summary.role ?? "worker";
    return roleOrder[leftRole] - roleOrder[rightRole] || left.itemId.localeCompare(right.itemId);
  });
}

function runScopedEvidenceRows(
  context: RunProjectionStoreContext,
  request: RunProjectionPageRequest,
  workRows: readonly OperatorViewRow<"work">[],
  authenticated: AuthenticatedOperatorCredential,
): OperatorViewRow<"evidence">[] {
  const taskIds = new Set(workRows.map((work) => work.itemId));
  const evidenceIds = new Set(
    context.database.prepare(`
      SELECT artifact_id, task_id FROM artifacts
      WHERE run_id=? AND registry_state='active'
    `).all(request.target.coordinationRunId).filter((value) => {
      if (request.target.kind === "coordination-run") return true;
      return taskIds.has(nullableText(row(value, "run projection evidence"), "task_id") ?? "");
    }).map((value) => text(row(value, "run projection evidence"), "artifact_id")),
  );
  return context.evidenceRows(
    request.projectId,
    request.projectSessionId,
    authenticated,
  ).filter((evidence) => evidenceIds.has(evidence.itemId));
}

function runScopedActivityRows(
  context: RunProjectionStoreContext,
  request: RunProjectionPageRequest,
  workRows: readonly OperatorViewRow<"work">[],
  authenticated: AuthenticatedOperatorCredential,
): OperatorViewRow<"activity">[] {
  const taskIds = new Set(workRows.map((work) => work.itemId));
  const eventIds = new Set(
    context.database.prepare(`
      SELECT event_id, payload_json FROM events WHERE run_id=?
    `).all(request.target.coordinationRunId).filter((value) => {
      if (request.target.kind === "coordination-run") return true;
      const event = row(value, "run projection activity");
      const payload = jsonObject(text(event, "payload_json"), "run projection activity payload");
      if (typeof payload.taskId === "string") return taskIds.has(payload.taskId);
      if (typeof payload.messageId !== "string") return false;
      const messageValue = context.database.prepare(`
        SELECT audience_json FROM messages WHERE run_id=? AND message_id=?
      `).get(request.target.coordinationRunId, payload.messageId);
      if (!isRow(messageValue)) return false;
      const audience = jsonObject(
        text(messageValue, "audience_json"),
        "run projection activity audience",
      );
      return typeof audience.taskId === "string" && taskIds.has(audience.taskId);
    }).map((value) => text(row(value, "run projection activity"), "event_id")),
  );
  return context.activityRows(
    request.projectId,
    request.projectSessionId,
    authenticated,
  ).filter((activity) => {
    const candidates = projectionFactValues(activity.fact);
    return candidates.length > 0 && candidates.every((candidate) => (
      "group" in candidate.summary
        ? candidate.summary.group.members.every((member) => eventIds.has(member.eventId))
        : eventIds.has(activity.itemId)
    ));
  });
}

function runIssueReferences(
  context: RunProjectionStoreContext,
  request: RunProjectionPageRequest,
  workRows: readonly OperatorViewRow<"work">[],
  authenticated: AuthenticatedOperatorCredential,
): RunIssueReference[] {
  const coordinationScope = {
    kind: "coordination-run" as const,
    coordinationRunId: request.target.coordinationRunId,
  };
  const issues: RunIssueReference[] = context.database.prepare(`
    SELECT gate_id, revision, status FROM scoped_gates
    WHERE project_session_id=? AND coordination_run_id=?
      AND status IN ('pending','deferred')
    ORDER BY gate_id
  `).all(
    request.projectSessionId,
    request.target.coordinationRunId,
  ).map((value): RunIssueReference => {
    const gate = row(value, "run projection gate");
    return {
      kind: "gate",
      scope: coordinationScope,
      gateId: parseIdentifier<"GateId">(text(gate, "gate_id"), "runProjection.gateId"),
      gateRevision: integer(gate, "revision"),
      status: text(gate, "status") as "pending" | "deferred",
    };
  });
  for (const work of workRows) {
    const taskId = parseIdentifier<"TaskId">(work.itemId, "runProjection.taskId");
    if (work.fact.freshness === "conflict") {
      issues.push({
        kind: "task-fact-conflict",
        scope: request.target,
        taskId,
        taskRevision: work.itemRevision,
        detailRef: { kind: "task", taskId, expectedRevision: work.itemRevision },
      });
      continue;
    }
    for (const candidate of work.fact.freshness === "unavailable" ? [] : [work.fact.value]) {
      if (candidate.summary.state === "blocked" || candidate.summary.state === "degraded") {
        issues.push({
          kind: "task",
          scope: request.target,
          taskId,
          taskRevision: work.itemRevision,
          state: candidate.summary.state,
          detailRef: candidate.detailRef,
        });
      }
      for (const check of candidate.summary.workflow?.checks.items ?? []) {
        if (check.state !== "fail") continue;
        issues.push({
          kind: "failed-check",
          scope: request.target,
          taskId,
          taskRevision: work.itemRevision,
          checkId: check.checkId,
          detailRef: candidate.detailRef,
        });
      }
    }
  }
  for (const evidence of runScopedEvidenceRows(context, request, workRows, authenticated)) {
    if (evidence.fact.freshness !== "conflict") continue;
    issues.push({
      kind: "evidence-conflict",
      scope: request.target,
      evidenceId: evidence.itemId,
      evidenceRevision: evidence.itemRevision,
      detailRef: {
        kind: "evidence",
        evidenceId: evidence.itemId,
        expectedRevision: evidence.itemRevision,
      },
    });
  }
  return issues;
}

function projectionFactValues<T>(fact: ProjectionFact<T>): readonly T[] {
  if (fact.freshness === "unavailable") return [];
  if (fact.freshness === "conflict") return fact.candidates;
  return [fact.value];
}

function oneScopedRow(
  database: Database.Database,
  sql: string,
  bindings: readonly (string | number | null)[],
  label: string,
): Row {
  const value = database.prepare(sql).get(...bindings);
  if (!isRow(value)) throw new ProjectFabricCoreError("NOT_FOUND", `${label} not found`);
  return value;
}

function liveFact<T>(revision: number, observedAt: Timestamp, value: T): ProjectionFact<T> {
  return { freshness: "live", source: "fabric", revision, observedAt, value };
}

function toTimestamp(milliseconds: number, path: string): Timestamp {
  return parseTimestamp(new Date(milliseconds).toISOString(), path);
}

function jsonObject(serialized: string, label: string): Row {
  const parsed: unknown = JSON.parse(serialized);
  if (!isRow(parsed)) throw new Error(`${label} is not an object`);
  return parsed;
}

function assertPageBounds(cursor: number, limit: number): void {
  if (!Number.isSafeInteger(cursor) || cursor < 0) {
    throw new TypeError("projection cursor must be a non-negative safe integer");
  }
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100) {
    throw new TypeError("projection page limit must be a safe integer from 1 to 100");
  }
}

function assertNever(value: never): never {
  throw new TypeError(`unexpected run projection section: ${String(value)}`);
}
