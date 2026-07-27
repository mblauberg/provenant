import {
  arrayOf,
  enumeration,
  identifier,
  integer,
  literal,
  nullable,
  objectCodec,
  parserBacked,
  timestamp,
  unionOf,
} from "../codec.js";
import {
  artifactRefCodec,
  positiveInteger,
  text,
} from "./common.js";

export function createOperatorProjectionFactCodecs() {
  const DECLARED_RUN_TASK_STATES = [
    "blocked", "ready", "active", "complete", "cancelled", "degraded",
  ] as const;
  const declaredRunTaskStateCountsCodec = objectCodec(
    Object.fromEntries(
      DECLARED_RUN_TASK_STATES.map((state) => [state, integer({ minimum: 0 })]),
    ),
  );
  const finiteDeclaredRunProgressBaseCodec = objectCodec({
    plan: literal("finite"),
    planRevision: positiveInteger,
    counts: declaredRunTaskStateCountsCodec,
    declaredTaskDenominator: positiveInteger,
  });
  const finiteDeclaredRunProgressCodec = parserBacked(
    finiteDeclaredRunProgressBaseCodec,
    (value, path) => {
      const progress = value as Record<string, unknown>;
      const counts = progress.counts as Record<string, number>;
      let remaining = progress.declaredTaskDenominator as number;
      for (const state of DECLARED_RUN_TASK_STATES) {
        const count = counts[state] as number;
        if (count > remaining) {
          throw new TypeError(`${path}.counts exceed declaredTaskDenominator`);
        }
        remaining -= count;
      }
      return value;
    },
    finiteDeclaredRunProgressBaseCodec.example,
  );
  const declaredRunProgressCodec = unionOf([
    objectCodec({ plan: literal("open"), counts: declaredRunTaskStateCountsCodec }),
    objectCodec({ plan: literal("unknown"), reason: text }),
    finiteDeclaredRunProgressCodec,
  ]);
  const runWorkstreamIdentityCodec = objectCodec({
    workstreamId: identifier,
    deliveryRunId: identifier,
    leadAgentId: identifier,
    state: enumeration(["active", "complete", "cancelled", "degraded", "abandoned"]),
    updatedAt: timestamp,
  });
  const runIdentityBaseCodec = objectCodec({
    runKind: literal("coordination"),
    chairAgentId: identifier,
    acceptedScopeRef: nullable(artifactRefCodec),
    currentPlanRef: nullable(artifactRefCodec),
    planRevision: nullable(positiveInteger),
    workstreams: arrayOf(runWorkstreamIdentityCodec, { maximum: 1024 }),
    lastEventAt: nullable(timestamp),
  });
  const runIdentityCodec = parserBacked(
    runIdentityBaseCodec,
    (value, path) => {
      const identity = value as Record<string, unknown>;
      const workstreams = identity.workstreams as ReadonlyArray<Record<string, unknown>>;
      const workstreamIds = new Set(workstreams.map((workstream) => workstream.workstreamId));
      const deliveryRunIds = new Set(workstreams.map((workstream) => workstream.deliveryRunId));
      if (workstreamIds.size !== workstreams.length || deliveryRunIds.size !== workstreams.length) {
        throw new TypeError(`${path}.workstreams must have unique workstreamId and deliveryRunId values`);
      }
      if ((identity.currentPlanRef === null) !== (identity.planRevision === null)) {
        throw new TypeError(`${path}.currentPlanRef and planRevision must be present or absent together`);
      }
      if (identity.currentPlanRef !== null && identity.acceptedScopeRef === null) {
        throw new TypeError(`${path}.acceptedScopeRef is required for a declared plan`);
      }
      return value;
    },
    runIdentityBaseCodec.example,
  );
  const validateRunPlanCorrelation = (value: unknown, path: string): unknown => {
    const run = value as Record<string, unknown>;
    const progress = run.declaredProgress as Record<string, unknown> | undefined;
    const identity = run.identity as Record<string, unknown> | undefined;
    if (progress?.plan === "finite" && identity !== undefined && progress.planRevision !== identity.planRevision) {
      throw new TypeError(`${path}.finite progress planRevision must match run identity planRevision`);
    }
    return value;
  };
  const workTaskStateCodec = enumeration(DECLARED_RUN_TASK_STATES);
  const workWorkflowFactsBaseCodec = objectCodec({
    workflowRevision: positiveInteger,
    objective: objectCodec({ observation: literal("Observed"), value: text }),
    dependencies: objectCodec({
      observation: literal("Observed"),
      dependencyRevision: positiveInteger,
      taskIds: arrayOf(identifier, { maximum: 1024, unique: true }),
    }),
    coordinationRun: objectCodec({
      observation: literal("Observed"),
      projectSessionId: identifier,
      coordinationRunId: identifier,
    }),
    workstream: unionOf([
      objectCodec({
        observation: literal("Observed"),
        workstreamId: identifier,
        deliveryRunId: identifier,
        workstreamRevision: positiveInteger,
        state: enumeration(["active", "complete", "cancelled", "degraded", "abandoned"]),
      }),
      objectCodec({ observation: literal("Unobserved") }),
      objectCodec({ observation: literal("Unknown"), reason: literal("MultipleWorkstreamBindings") }),
    ]),
    parentTask: objectCodec({ observation: literal("Unobserved") }),
    plan: unionOf([
      objectCodec({ observation: literal("Observed"), planRevision: positiveInteger }),
      objectCodec({ observation: literal("Unobserved") }),
    ]),
    task: objectCodec({
      observation: literal("Observed"),
      state: workTaskStateCodec,
      owner: unionOf([
        objectCodec({ observation: literal("Observed"), agentId: identifier, ownerLeaseGeneration: positiveInteger }),
        objectCodec({ observation: literal("Unobserved") }),
      ]),
    }),
    checks: objectCodec({
      observation: literal("Observed"),
      items: arrayOf(objectCodec({
        checkId: identifier,
        state: enumeration(["pending", "pass", "fail"]),
      }), { maximum: 256, unique: true }),
    }),
    barriers: objectCodec({
      observation: literal("Observed"),
      items: arrayOf(unionOf([
        objectCodec({ kind: literal("run"), barrierId: text, state: literal("closed") }),
        objectCodec({ kind: literal("stage"), barrierId: text, stageId: text, state: literal("closed") }),
        objectCodec({
          kind: literal("task-request"),
          barrierId: text,
          requestId: identifier,
          state: enumeration(["blocked", "released", "abandoned"]),
        }),
      ]), { maximum: 256, unique: true }),
    }),
    declaredWriteScopes: objectCodec({
      observation: literal("Observed"),
      leases: arrayOf(objectCodec({
        leaseId: identifier,
        generation: positiveInteger,
        state: enumeration(["active", "quarantined", "released"]),
        paths: arrayOf(text, { maximum: 256, unique: true }),
      }), { maximum: 128, unique: true }),
    }),
    runTaskStates: objectCodec({
      observation: literal("Observed"),
      counts: declaredRunTaskStateCountsCodec,
    }),
  });
  const assertSortedUnique = (values: readonly unknown[], path: string): void => {
    if (new Set(values).size !== values.length) throw new TypeError(`${path} must be unique`);
    const strings = values.map(String);
    if (strings.some((value, index) => index > 0 && value <= strings[index - 1]!)) {
      throw new TypeError(`${path} must be strictly sorted`);
    }
  };
  const workWorkflowFactsCodec = parserBacked(
    workWorkflowFactsBaseCodec,
    (value, path) => {
      const workflow = value as Record<string, unknown>;
      const dependencies = workflow.dependencies as { taskIds: unknown[] };
      const checks = workflow.checks as { items: Array<{ checkId: unknown }> };
      const barriers = workflow.barriers as { items: Array<{ kind: unknown; barrierId: unknown }> };
      const scopes = workflow.declaredWriteScopes as {
        leases: Array<{ leaseId: unknown; paths: unknown[] }>;
      };
      assertSortedUnique(dependencies.taskIds, `${path}.dependencies.taskIds`);
      assertSortedUnique(checks.items.map((item) => item.checkId), `${path}.checks.items`);
      assertSortedUnique(
        barriers.items.map((item) => `${String(item.kind)}:${String(item.barrierId)}`),
        `${path}.barriers.items`,
      );
      assertSortedUnique(scopes.leases.map((lease) => lease.leaseId), `${path}.declaredWriteScopes.leases`);
      for (const [index, lease] of scopes.leases.entries()) {
        assertSortedUnique(lease.paths, `${path}.declaredWriteScopes.leases[${String(index)}].paths`);
      }
      const task = workflow.task as { state: string };
      const states = workflow.runTaskStates as { counts: Record<string, number> };
      if ((states.counts[task.state] ?? 0) < 1) {
        throw new TypeError(`${path}.runTaskStates must include the projected task state`);
      }
      return value;
    },
    workWorkflowFactsBaseCodec.example,
  );
  const agentTeamTopologyMembershipCodec = objectCodec({
    teamId: identifier,
    teamGeneration: positiveInteger,
    relationship: enumeration(["Lead", "Member"]),
    leadAgentId: identifier,
  });
  const agentTopologyCodec = objectCodec({
    topologyRevision: positiveInteger,
    teams: objectCodec({
      observation: literal("Observed"),
      memberships: arrayOf(agentTeamTopologyMembershipCodec, { maximum: 4 }),
    }),
    supervisor: unionOf([
      objectCodec({ observation: literal("Observed"), agentId: identifier }),
      objectCodec({ observation: literal("Unobserved") }),
    ]),
    currentTask: unionOf([
      objectCodec({
        observation: literal("Observed"),
        taskId: identifier,
        taskRevision: positiveInteger,
        ownerLeaseGeneration: positiveInteger,
      }),
      objectCodec({ observation: literal("Unobserved") }),
      objectCodec({ observation: literal("Unknown"), reason: literal("MultipleActiveClaims") }),
    ]),
    nativeChildren: objectCodec({ observation: literal("Unobserved") }),
  });
  const factCandidates = (fact: Record<string, unknown>): Record<string, unknown>[] => (
    fact.freshness === "conflict"
      ? fact.candidates as Record<string, unknown>[]
      : fact.freshness === "unavailable"
        ? []
        : [fact.value as Record<string, unknown>]
  );
  const validateAgentTopology = (
    topology: Record<string, unknown> | undefined,
    agentId: unknown,
    snapshotRevision: unknown,
    path: string,
  ): void => {
    if (topology === undefined) return;
    if (topology.topologyRevision !== snapshotRevision) {
      throw new TypeError(`${path}.topologyRevision must match snapshotRevision`);
    }
    const teams = topology.teams as Record<string, unknown>;
    const memberships = teams.memberships as Record<string, unknown>[];
    const teamIds = new Set(memberships.map((membership) => membership.teamId));
    if (teamIds.size !== memberships.length) {
      throw new TypeError(`${path}.teams.memberships must have unique teamId values`);
    }
    for (const membership of memberships) {
      const relationship = membership.leadAgentId === agentId ? "Lead" : "Member";
      if (membership.relationship !== relationship) {
        throw new TypeError(`${path}.teams.memberships relationship must match leadAgentId`);
      }
    }
  };
  const validateWorkWorkflow = (
    workflow: Record<string, unknown> | undefined,
    enclosing: Record<string, unknown>,
    snapshotRevision: unknown,
    path: string,
  ): void => {
    if (workflow === undefined) return;
    if (workflow.workflowRevision !== snapshotRevision) {
      throw new TypeError(`${path}.workflowRevision must match snapshotRevision`);
    }
    const task = workflow.task as Record<string, unknown>;
    if (task.state !== enclosing.state) throw new TypeError(`${path}.task.state must match enclosing state`);
    const objective = workflow.objective as Record<string, unknown>;
    if (enclosing.objective !== undefined && objective.value !== enclosing.objective) {
      throw new TypeError(`${path}.objective must match enclosing objective`);
    }
    const owner = task.owner as Record<string, unknown>;
    if (enclosing.ownerAgentId !== undefined) {
      const projectedOwner = owner.observation === "Observed" ? owner.agentId : null;
      if (projectedOwner !== enclosing.ownerAgentId) {
        throw new TypeError(`${path}.task.owner must match enclosing ownerAgentId`);
      }
    }
    if (enclosing.checkState !== undefined) {
      const checks = (workflow.checks as { items: Array<{ state: string }> }).items;
      const expected = checks.length === 0
        ? "unknown"
        : checks.some((check) => check.state === "fail")
          ? "failing"
          : checks.some((check) => check.state === "pending")
            ? "pending"
            : "passing";
      if (enclosing.checkState !== expected) {
        throw new TypeError(`${path}.checks must match enclosing checkState`);
      }
    }
  };

  return {
    declaredRunProgressCodec,
    runIdentityCodec,
    validateRunPlanCorrelation,
    workWorkflowFactsCodec,
    agentTopologyCodec,
    factCandidates,
    validateAgentTopology,
    validateWorkWorkflow,
  };
}
