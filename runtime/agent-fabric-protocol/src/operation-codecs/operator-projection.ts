import {
  arrayOf,
  boolean,
  enumeration,
  identifier,
  integer,
  jsonValue,
  literal,
  nullable,
  objectCodec,
  parserBacked,
  relativePath,
  timestamp,
  unionOf,
  type Codec,
} from "../codec.js";
import { FABRIC_OPERATIONS } from "../operations.js";
import {
  absoluteFilesystemPathCodec,
  artifactRefCodec,
  credentialCodec,
  jsonRecord,
  object,
  positiveInteger,
  projectionFact,
  semanticShapeCodec,
  stringList,
  text,
  type OperationCodecFragment,
  type OperationShapeFragment,
} from "./common.js";
import { createActivityNarrativeCodecs } from "./activity-narrative.js";

export const OPERATOR_PROJECTION_INPUT_SHAPES = {
  [FABRIC_OPERATIONS.projectDiscover]: object(["credential", "projectId", "after", "limit"]),
  [FABRIC_OPERATIONS.projectionSnapshot]: object(["credential", "projectId"], ["projectSessionId"]),
  [FABRIC_OPERATIONS.projectionPage]: object(["credential", "projectId", "view", "after", "limit"], ["projectSessionId"]),
  [FABRIC_OPERATIONS.projectionEvents]: object(["credential", "projectId", "after", "limit"], ["projectSessionId"]),
  [FABRIC_OPERATIONS.projectionViewPage]: object(["credential", "projectId", "view", "snapshotRevision", "cursor", "limit"], ["projectSessionId"]),
  [FABRIC_OPERATIONS.projectionDetailRead]: object(["credential", "projectId", "snapshotRevision", "detailRef"], ["projectSessionId"]),
} as const satisfies OperationShapeFragment;

export const OPERATOR_PROJECTION_RESULT_SHAPES = {
  [FABRIC_OPERATIONS.projectDiscover]: object(["project", "sessions"]),
  [FABRIC_OPERATIONS.projectionSnapshot]: object(["schemaVersion", "snapshotRevision", "readTransactionId", "project", "session", "runs", "attention", "capacity", "cursor", "stateDigest"]),
  [FABRIC_OPERATIONS.projectionPage]: object(["view", "page"]),
  [FABRIC_OPERATIONS.projectionEvents]: object(["status"], ["events", "nextCursor", "hasMore", "snapshotRevision", "readTransactionId", "reason", "currentSnapshotRevision", "snapshotCursor"]),
  [FABRIC_OPERATIONS.projectionViewPage]: object(["status", "view"], ["rows", "nextCursor", "hasMore", "snapshotRevision", "readTransactionId", "reason", "currentSnapshotRevision", "snapshotCursor"]),
  [FABRIC_OPERATIONS.projectionDetailRead]: object(["status"], ["detailRef", "detail", "snapshotRevision", "readTransactionId", "reason", "currentSnapshotRevision"]),
} as const satisfies OperationShapeFragment;

export function createOperatorProjectionOperationCodecFragment(dependencies: Readonly<{
  projectSessionCodec: Codec<unknown>;
  gitRepositorySummaryCodec: Codec<unknown>;
  gitRepositoryProjectionCodec: Codec<unknown>;
}>) {
  const { projectSessionCodec, gitRepositorySummaryCodec, gitRepositoryProjectionCodec } = dependencies;
  const runProjectionCodec = objectCodec({
    runId: identifier,
    phase: text,
    chairAgentId: identifier,
    nextMilestone: text,
    health: enumeration(["healthy", "degraded", "blocked", "quarantined", "unknown"]),
  }, { projectSessionId: identifier });
  const nativeNotificationDeliverySummaryCodec = objectCodec({
    targetIntegration: literal("native-desktop"),
    status: enumeration(["available", "unavailable", "stale"]),
    journalState: enumeration(["missing", "pending", "claimed", "sent", "failed", "deduplicated", "ambiguous"]),
    deliveryItemRevision: nullable(positiveInteger),
    claimGeneration: nullable(integer({ minimum: 0 })),
    integrationState: enumeration(["absent", "available", "unavailable", "stale"]),
    observedAt: timestamp,
  });
  const attentionItemCodec = objectCodec({
    itemId: identifier,
    revision: positiveInteger,
    label: enumeration(["Decision", "Approval", "Blocked", "FYI"]),
    priority: enumeration(["safety-integrity", "critical-path", "expiring-authority", "acceptance-ready", "advisory"]),
    title: text,
    sourceFreshness: enumeration(["live", "snapshot", "stale", "unavailable", "conflict"]),
    lastEventAt: timestamp,
    duplicateCount: integer(),
  }, { nativeNotification: nativeNotificationDeliverySummaryCodec });

  const operatorActionAvailabilityCodec = unionOf([
    objectCodec({
      state: literal("read-only"),
      reason: enumeration(["feature-unavailable", "authority-insufficient", "state-ineligible"]),
    }),
    objectCodec({
      state: literal("available"),
      actions: arrayOf(enumeration([
        "pause",
        "resume",
        "cancel",
        "steer",
        "project-session-launch",
        "chair-bridge-recovery",
        "chair-live-handoff",
        "project-session-drain",
        "project-session-stop",
        "daemon-drain",
        "daemon-stop",
        "git",
        "git-authorise",
        "git-operation-draft",
        "git-custody-resolve",
        "agent-lifecycle-recovery",
        "registered-external-effect",
        "provider-route-integrity-retire",
        "promotion",
      ]), { minimum: 1, maximum: 19, unique: true }),
      requiresPreview: literal(true),
    }),
  ]);

  const operatorDetailRefCodec = unionOf([
    objectCodec({ kind: literal("project"), projectId: identifier, expectedRevision: positiveInteger }),
    objectCodec({ kind: literal("session"), projectSessionId: identifier, expectedRevision: positiveInteger }),
    objectCodec(
      { kind: literal("run"), coordinationRunId: identifier, expectedRevision: positiveInteger },
      { projectSessionId: identifier },
    ),
    objectCodec({ kind: literal("task"), taskId: identifier, expectedRevision: positiveInteger }),
    objectCodec({ kind: literal("agent"), agentId: identifier, expectedRevision: positiveInteger }),
    objectCodec({ kind: literal("evidence"), evidenceId: identifier, expectedRevision: positiveInteger }),
    objectCodec({ kind: literal("activity"), eventId: identifier, expectedRevision: positiveInteger }),
    objectCodec({ kind: literal("activity"), groupId: identifier, expectedRevision: positiveInteger }),
    objectCodec({ kind: literal("activity"), groupId: identifier, eventId: identifier, expectedRevision: positiveInteger, contentOffset: integer({ minimum: 0 }) }),
    objectCodec({ kind: literal("system"), componentId: identifier, expectedRevision: positiveInteger }),
  ]);
  const projectDetailRefCodec = objectCodec({
    kind: literal("project"),
    projectId: identifier,
    expectedRevision: positiveInteger,
  });
  const runDetailRefCodec = objectCodec({
    kind: literal("run"),
    coordinationRunId: identifier,
    expectedRevision: positiveInteger,
  }, { projectSessionId: identifier });
  const taskDetailRefCodec = objectCodec({ kind: literal("task"), taskId: identifier, expectedRevision: positiveInteger });
  const agentDetailRefCodec = objectCodec({ kind: literal("agent"), agentId: identifier, expectedRevision: positiveInteger });
  const evidenceDetailRefCodec = objectCodec({
    kind: literal("evidence"),
    evidenceId: identifier,
    expectedRevision: positiveInteger,
  });
  const activityDetailRefCodec = unionOf([
    objectCodec({ kind: literal("activity"), eventId: identifier, expectedRevision: positiveInteger }),
    objectCodec({ kind: literal("activity"), groupId: identifier, expectedRevision: positiveInteger }),
    objectCodec({ kind: literal("activity"), groupId: identifier, eventId: identifier, expectedRevision: positiveInteger, contentOffset: integer({ minimum: 0 }) }),
  ]);
  const systemDetailRefCodec = objectCodec({
    kind: literal("system"),
    componentId: identifier,
    expectedRevision: positiveInteger,
  });
  const messageBodyRefCodec = objectCodec({
    projectSessionId: identifier,
    messageId: identifier,
    expectedRevision: positiveInteger,
  });
  const { activityNarrativeGroupCodec, groupedActivityDetailCodec, activityNarrativeMemberContentDetailCodec } =
    createActivityNarrativeCodecs({ artifactRefCodec, messageBodyRefCodec });

  const attentionSummaryCodec = objectCodec({
    kind: literal("attention"),
    label: enumeration(["Decision", "Approval", "Blocked", "FYI"]),
    priority: enumeration(["safety-integrity", "critical-path", "expiring-authority", "acceptance-ready", "advisory"]),
    title: text,
  }, {
    gateBinding: objectCodec({
      gateId: identifier,
      gateRevision: positiveInteger,
      coordinationRunId: identifier,
    }),
    nativeNotification: nativeNotificationDeliverySummaryCodec,
  });
  const projectSummaryCodec = objectCodec(
    { kind: literal("project"), goal: text, acceptedScopeRef: nullable(artifactRefCodec), repositoryRevision: text },
    { repository: gitRepositorySummaryCodec },
  );
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
  const runSummaryBaseCodec = objectCodec({
    kind: literal("run"),
    phase: text,
    health: enumeration(["healthy", "degraded", "blocked", "quarantined", "unknown"]),
    nextMilestone: text,
  }, { projectSessionId: identifier, declaredProgress: declaredRunProgressCodec, identity: runIdentityCodec });
  const runSummaryCodec = parserBacked(
    runSummaryBaseCodec,
    validateRunPlanCorrelation,
    runSummaryBaseCodec.example,
  );
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
  const workSummaryCodec = objectCodec({
    kind: literal("work"),
    state: text,
    checkState: enumeration(["pending", "passing", "failing", "unknown"]),
  }, { workflow: workWorkflowFactsCodec });
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
  const agentSummaryCodec = objectCodec({
    kind: literal("agent"),
    role: enumeration(["chair", "lead", "worker", "reviewer"]),
    lifecycle: text,
    contextPressure: enumeration(["low", "medium", "high", "unknown"]),
  }, { topology: agentTopologyCodec });
  const evidenceSummaryCodec = objectCodec({
    kind: literal("evidence"),
    evidenceKind: enumeration(["artifact", "diff", "test", "review", "receipt"]),
    status: enumeration(["pass", "fail", "pending", "informational"]),
    provenance: text,
  });
  const activitySummaryFields = {
    kind: literal("activity"),
    summary: text,
    occurredAt: timestamp,
  };
  const activitySummaryCodec = unionOf([
    objectCodec({
      ...activitySummaryFields,
      activityKind: literal("message"),
      messageBodyRef: messageBodyRefCodec,
    }),
    objectCodec({
      ...activitySummaryFields,
      activityKind: enumeration(["decision", "lifecycle", "operation"]),
    }),
    objectCodec({ ...activitySummaryFields, group: activityNarrativeGroupCodec }),
  ]);
  const systemSummaryCodec = objectCodec({
    kind: literal("system"),
    systemKind: enumeration(["daemon", "adapter", "trust", "seat", "integration"]),
    state: enumeration(["healthy", "degraded", "stale", "unavailable", "conflict"]),
    detail: text,
  });

  function operatorViewRowCodec(summary: Codec<unknown>, detailRef: Codec<unknown>): Codec<unknown> {
    return objectCodec({
      itemId: identifier,
      itemRevision: positiveInteger,
      fact: projectionFact(objectCodec({ summary, detailRef, actionAvailability: operatorActionAvailabilityCodec })),
    });
  }

  const attentionRowCodec = operatorViewRowCodec(attentionSummaryCodec, operatorDetailRefCodec);
  const projectRowCodec = operatorViewRowCodec(projectSummaryCodec, projectDetailRefCodec);
  const runRowCodec = operatorViewRowCodec(runSummaryCodec, runDetailRefCodec);
  const workRowCodec = operatorViewRowCodec(workSummaryCodec, taskDetailRefCodec);
  const agentRowCodecV2 = operatorViewRowCodec(agentSummaryCodec, agentDetailRefCodec);
  const evidenceRowCodec = operatorViewRowCodec(evidenceSummaryCodec, evidenceDetailRefCodec);
  const activityRowCodec = operatorViewRowCodec(activitySummaryCodec, activityDetailRefCodec);
  const systemRowCodec = operatorViewRowCodec(systemSummaryCodec, systemDetailRefCodec);

  function operatorViewPageVariant(view: string, row: Codec<unknown>): Codec<unknown> {
    return objectCodec({
      status: literal("page"),
      view: literal(view),
      rows: arrayOf(row, { maximum: 256 }),
      nextCursor: integer(),
      hasMore: boolean,
      snapshotRevision: positiveInteger,
      readTransactionId: identifier,
    });
  }
  const operatorViewPageInputCodec = objectCodec({
    credential: credentialCodec,
    projectId: identifier,
    view: enumeration(["attention", "project", "runs", "work", "agents", "evidence", "activity", "system"]),
    snapshotRevision: positiveInteger,
    cursor: integer(),
    limit: integer({ minimum: 1, maximum: 256 }),
  }, { projectSessionId: identifier });
  const operatorViewPageBaseCodec = unionOf([
    operatorViewPageVariant("attention", attentionRowCodec),
    operatorViewPageVariant("project", projectRowCodec),
    operatorViewPageVariant("runs", runRowCodec),
    operatorViewPageVariant("work", workRowCodec),
    operatorViewPageVariant("agents", agentRowCodecV2),
    operatorViewPageVariant("evidence", evidenceRowCodec),
    operatorViewPageVariant("activity", activityRowCodec),
    operatorViewPageVariant("system", systemRowCodec),
    objectCodec({
      status: literal("resnapshot-required"),
      view: enumeration(["attention", "project", "runs", "work", "agents", "evidence", "activity", "system"]),
      reason: enumeration(["snapshot-mismatch", "retention-gap", "project-cursor-mismatch", "cursor-overflow"]),
      currentSnapshotRevision: positiveInteger,
      snapshotCursor: integer(),
    }),
  ]);
  const operatorViewPageResultCodec = parserBacked(
    operatorViewPageBaseCodec,
    (value) => {
      if (Reflect.get(value as object, "status") !== "page") return value;
      const view = Reflect.get(value as object, "view");
      const snapshotRevision = Reflect.get(value as object, "snapshotRevision");
      const rows = Reflect.get(value as object, "rows") as Array<Record<string, unknown>>;
      for (const [index, row] of rows.entries()) {
        const fact = row.fact as Record<string, unknown>;
        if (row.itemRevision !== fact.revision) {
          throw new TypeError(`operatorViewPage.rows[${String(index)}] item revision does not match fact revision`);
        }
        for (const candidate of factCandidates(fact)) {
          const summary = candidate.summary as Record<string, unknown>;
          if (view === "agents") {
            validateAgentTopology(
              summary.topology as Record<string, unknown> | undefined,
              row.itemId,
              snapshotRevision,
              `operatorViewPage.rows[${String(index)}].fact.topology`,
            );
          }
          if (view === "work") {
            const detailRef = candidate.detailRef as Record<string, unknown>;
            if (summary.workflow !== undefined && detailRef.taskId !== row.itemId) {
              throw new TypeError(`operatorViewPage.rows[${String(index)}] task identity does not match reference`);
            }
            validateWorkWorkflow(
              summary.workflow as Record<string, unknown> | undefined,
              summary,
              snapshotRevision,
              `operatorViewPage.rows[${String(index)}].fact.workflow`,
            );
          }
          if (view === "activity" && "group" in summary) {
            const group = summary.group as Record<string, unknown>;
            const detailRef = candidate.detailRef as Record<string, unknown>;
            const sourceRange = group.sourceRange as Record<string, unknown>;
            const occurredAtRange =
              group.occurredAtRange as Record<string, unknown>;
            if (
              row.itemId !== group.groupId ||
              row.itemRevision !== sourceRange.last ||
              detailRef.groupId !== group.groupId ||
              detailRef.expectedRevision !== sourceRange.last ||
              summary.occurredAt !== occurredAtRange.last
            ) {
              throw new TypeError(
                `operatorViewPage.rows[${String(index)}] activity group bindings do not match`,
              );
            }
          }
        }
      }
      return value;
    },
    operatorViewPageBaseCodec.example,
  );

  const runDetailBaseCodec = objectCodec({
    kind: literal("run"),
    coordinationRunId: identifier,
    phase: text,
    chairAgentId: identifier,
    chairGeneration: positiveInteger,
    health: enumeration(["healthy", "degraded", "blocked", "quarantined", "unknown"]),
  }, { projectSessionId: identifier, declaredProgress: declaredRunProgressCodec, identity: runIdentityCodec });
  const runDetailCodec = parserBacked(
    runDetailBaseCodec,
    validateRunPlanCorrelation,
    runDetailBaseCodec.example,
  );
  const operatorDetailCodec = unionOf([
    objectCodec(
      {
        kind: literal("project"),
        projectId: identifier,
        canonicalRoot: absoluteFilesystemPathCodec,
        goal: text,
        acceptedScopeRef: nullable(artifactRefCodec),
        repositoryRevision: text,
      },
      { repository: gitRepositoryProjectionCodec },
    ),
    objectCodec({
      kind: literal("session"),
      projectSessionId: identifier,
      mode: enumeration(["coordinated", "independent"]),
      state: enumeration([
        "draft", "awaiting_launch", "launching", "active", "quiescing", "awaiting_acceptance", "closed",
        "launch_failed", "launch_ambiguous", "reconciling", "visibility_degraded", "recovery_required",
        "quarantined", "cancelled",
      ]),
      generation: positiveInteger,
      membershipRevision: integer(),
    }),
    runDetailCodec,
    objectCodec(
      { kind: literal("task"), taskId: identifier, objective: text, state: text, ownerAgentId: nullable(identifier) },
      { workflow: workWorkflowFactsCodec },
    ),
    objectCodec({
      kind: literal("agent"),
      agentId: identifier,
      role: enumeration(["chair", "lead", "worker", "reviewer"]),
      lifecycle: text,
      provider: text,
      providerSessionGeneration: positiveInteger,
    }, { topology: agentTopologyCodec }),
    objectCodec({
      kind: literal("evidence"),
      evidenceId: identifier,
      evidenceKind: enumeration(["artifact", "diff", "test", "review", "receipt"]),
      artifactRef: artifactRefCodec,
      sourceKind: enumeration(["project-file", "run-file", "git-private-diff"]),
      publisherKind: enumeration(["agent", "operator", "fabric", "project", "migration"]),
      publisherRef: identifier,
      projectSessionId: nullable(identifier),
      coordinationRunId: nullable(identifier),
      taskId: nullable(identifier),
      createdAt: timestamp,
      status: enumeration(["pass", "fail", "pending", "informational"]),
    }),
    objectCodec({
      kind: literal("activity"),
      eventId: identifier,
      activityKind: literal("message"),
      summary: text,
      occurredAt: timestamp,
      messageBodyRef: messageBodyRefCodec,
    }),
    objectCodec({
      kind: literal("activity"),
      eventId: identifier,
      activityKind: enumeration(["decision", "lifecycle", "operation"]),
      summary: text,
      occurredAt: timestamp,
    }),
    groupedActivityDetailCodec,
    activityNarrativeMemberContentDetailCodec,
    objectCodec({
      kind: literal("system"),
      componentId: identifier,
      systemKind: enumeration(["daemon", "adapter", "trust", "seat", "integration"]),
      state: enumeration(["healthy", "degraded", "stale", "unavailable", "conflict"]),
      generation: positiveInteger,
      detail: text,
    }),
  ]);
  const operatorDetailReadInputCodec = objectCodec({
    credential: credentialCodec,
    projectId: identifier,
    snapshotRevision: positiveInteger,
    detailRef: operatorDetailRefCodec,
  }, { projectSessionId: identifier });
  const operatorDetailReadBaseCodec = unionOf([
    objectCodec({
      status: literal("current"),
      detailRef: operatorDetailRefCodec,
      detail: projectionFact(operatorDetailCodec),
      snapshotRevision: positiveInteger,
      readTransactionId: identifier,
    }),
    objectCodec({
      status: literal("resnapshot-required"),
      reason: enumeration(["snapshot-mismatch", "detail-revision-changed"]),
      currentSnapshotRevision: positiveInteger,
    }),
  ]);
  const operatorDetailReadResultCodec = parserBacked(
    operatorDetailReadBaseCodec,
    (value) => {
      if (Reflect.get(value as object, "status") !== "current") return value;
      const detailRef = Reflect.get(value as object, "detailRef") as Record<string, unknown>;
      const fact = Reflect.get(value as object, "detail") as Record<string, unknown>;
      if (detailRef.expectedRevision !== fact.revision) {
        throw new TypeError("operatorDetailRead detail revision does not match reference");
      }
      const values: Record<string, unknown>[] = fact.freshness === "conflict"
        ? fact.candidates as Record<string, unknown>[]
        : fact.freshness === "unavailable"
          ? []
          : [fact.value as Record<string, unknown>];
      if (values.some((detail) => detail.kind !== detailRef.kind)) {
        throw new TypeError("operatorDetailRead detail kind does not match reference");
      }
      for (const detail of values) {
        if (detail.kind === "agent") {
          if (detail.agentId !== detailRef.agentId) {
            throw new TypeError("operatorDetailRead agent identity does not match reference");
          }
          validateAgentTopology(
            detail.topology as Record<string, unknown> | undefined,
            detail.agentId,
            Reflect.get(value as object, "snapshotRevision"),
            "operatorDetailRead.detail.topology",
          );
        }
        if (detail.kind === "task") {
          if (detail.workflow !== undefined && detail.taskId !== detailRef.taskId) {
            throw new TypeError("operatorDetailRead task identity does not match reference");
          }
          validateWorkWorkflow(
            detail.workflow as Record<string, unknown> | undefined,
            detail,
            Reflect.get(value as object, "snapshotRevision"),
            "operatorDetailRead.detail.workflow",
          );
        }
        if (detail.kind === "activity" && "group" in detail) {
          const group = detail.group as Record<string, unknown>;
          const sourceRange = group.sourceRange as Record<string, unknown>;
          if (
            detailRef.groupId !== group.groupId ||
            detailRef.expectedRevision !== sourceRange.last ||
            fact.revision !== sourceRange.last
          ) {
            throw new TypeError("operatorDetailRead activity group bindings do not match");
          }
        }
        if (detail.kind === "activity" && "groupId" in detail && "eventId" in detail) {
          if (
            detailRef.groupId !== detail.groupId ||
            detailRef.eventId !== detail.eventId ||
            detailRef.contentOffset !== detail.contentOffset ||
            detailRef.expectedRevision !== detail.sourceRevision ||
            fact.revision !== detail.sourceRevision
          ) {
            throw new TypeError("operatorDetailRead activity member bindings do not match");
          }
        }
        if (detail.kind !== "run" || detail.identity === undefined) continue;
        const identity = detail.identity as Record<string, unknown>;
        if (identity.chairAgentId !== detail.chairAgentId) {
          throw new TypeError("operatorDetailRead identity chair must match the enclosing run chair");
        }
      }
      return value;
    },
    operatorDetailReadBaseCodec.example,
  );

  const projectIdentityCodec = objectCodec({ projectId: identifier, canonicalRoot: text });
  const projectViewItemCodec = objectCodec({
    projectId: identifier,
    goal: text,
    acceptedScopeRef: nullable(artifactRefCodec),
    repositoryRevision: text,
    github: projectionFact(objectCodec({ repository: text, openPullRequests: integer() })),
  });
  const workViewItemCodec = objectCodec({
    taskId: identifier,
    workstreamId: nullable(identifier),
    parentTaskId: nullable(identifier),
    state: text,
    ownerAgentId: nullable(identifier),
    sourcePrefixes: arrayOf(relativePath, { maximum: 128, unique: true }),
    worktreePath: nullable(text),
    barrierIds: stringList,
    checkState: enumeration(["pending", "passing", "failing", "unknown"]),
  });
  const agentViewItemCodec = objectCodec({
    agentId: identifier,
    stableTaskId: nullable(identifier),
    stableWorkstreamId: nullable(identifier),
    role: enumeration(["chair", "lead", "worker", "reviewer"]),
    provider: text,
    modelFamily: text,
    providerSessionRef: nullable(identifier),
    providerSessionGeneration: integer(),
    lifecycle: text,
    contextPressure: enumeration(["low", "medium", "high", "unknown"]),
    visibility: projectionFact(objectCodec({ paneRef: nullable(identifier) })),
  });
  const evidenceViewItemCodec = objectCodec({
    evidenceId: identifier,
    kind: enumeration(["artifact", "diff", "test", "review", "receipt"]),
    artifactRef: artifactRefCodec,
    taskId: nullable(identifier),
    provenance: text,
    status: enumeration(["pass", "fail", "pending", "informational"]),
  });
  const activityViewItemFields = {
    eventId: identifier,
    actorId: nullable(identifier),
    taskId: nullable(identifier),
    summary: text,
    occurredAt: timestamp,
    sourceRevision: integer(),

  };
  const activityViewItemCodec = unionOf([
    objectCodec(
      { ...activityViewItemFields, kind: literal("message"), messageBodyRef: messageBodyRefCodec },
      { group: activityNarrativeGroupCodec },
    ),
    objectCodec(
      { ...activityViewItemFields, kind: enumeration(["decision", "lifecycle", "operation"]) },
      { group: activityNarrativeGroupCodec },
    ),
  ]);
  const systemViewItemCodec = objectCodec({
    componentId: identifier,
    kind: enumeration(["daemon", "adapter", "trust", "seat", "integration"]),
    state: enumeration(["healthy", "degraded", "stale", "unavailable", "conflict"]),
    generation: integer(),
    expiresAt: nullable(timestamp),
    detail: text,
  });
  function projectionPageDataCodec(itemCodec: Codec<unknown>): Codec<unknown> {
    return projectionFact(objectCodec({
      items: arrayOf(itemCodec, { maximum: 256 }),
      nextCursor: integer(),
      hasMore: boolean,
    }));
  }
  const projectionPageResultCodec = unionOf([
    objectCodec({ view: literal("attention"), page: projectionPageDataCodec(attentionItemCodec) }),
    objectCodec({ view: literal("project"), page: projectionPageDataCodec(projectViewItemCodec) }),
    objectCodec({ view: literal("runs"), page: projectionPageDataCodec(runProjectionCodec) }),
    objectCodec({ view: literal("work"), page: projectionPageDataCodec(workViewItemCodec) }),
    objectCodec({ view: literal("agents"), page: projectionPageDataCodec(agentViewItemCodec) }),
    objectCodec({ view: literal("evidence"), page: projectionPageDataCodec(evidenceViewItemCodec) }),
    objectCodec({ view: literal("activity"), page: projectionPageDataCodec(activityViewItemCodec) }),
    objectCodec({ view: literal("system"), page: projectionPageDataCodec(systemViewItemCodec) }),
  ]);
  const projectionEventCodec = objectCodec({
    cursor: positiveInteger,
    projectSessionId: identifier,
    kind: text,
    revision: positiveInteger,
    occurredAt: timestamp,
    payload: jsonValue,
  });
  const projectSessionDiscoveryCodec = objectCodec({
    projectSessionId: identifier,
    mode: enumeration(["coordinated", "independent"]),
    state: enumeration([
      "draft", "awaiting_launch", "launching", "active", "quiescing", "awaiting_acceptance", "closed",
      "launch_failed", "launch_ambiguous", "reconciling", "visibility_degraded", "recovery_required",
      "quarantined", "cancelled",
    ]),
    revision: positiveInteger,
    generation: positiveInteger,
    lastEventAt: timestamp,
  });
  const discoveredSessionsCodec = projectionFact(objectCodec({
    items: arrayOf(projectSessionDiscoveryCodec, { maximum: 256 }),
    nextCursor: integer(),
    hasMore: boolean,
  }));


  const projectionEventsResultCodec = unionOf([
    objectCodec({
      status: literal("continuation"),
      events: arrayOf(projectionEventCodec, { maximum: 256 }),
      nextCursor: integer(),
      hasMore: boolean,
      snapshotRevision: positiveInteger,
      readTransactionId: identifier,
    }),
    objectCodec({
      status: literal("resnapshot-required"),
      reason: enumeration(["retention-gap", "project-cursor-mismatch", "cursor-overflow"]),
      currentSnapshotRevision: positiveInteger,
      snapshotCursor: integer(),
    }),
  ]);

  const projectionFieldCodec = (
    operation: Parameters<typeof semanticShapeCodec>[0],
    field: string,
    direction: Parameters<typeof semanticShapeCodec>[1],
  ): Codec<unknown> | undefined => {
    if (field === "schemaVersion" && operation === FABRIC_OPERATIONS.projectionSnapshot && direction === "result") {
      return literal(1);
    }
    if (field === "after" && direction === "input") return integer();
    if (field === "view") return enumeration(["attention", "project", "runs", "work", "agents", "evidence", "activity", "system"]);
    if (field === "project") return projectionFact(projectIdentityCodec);
    if (field === "session") return projectionFact(nullable(projectSessionCodec));
    if (field === "runs") return projectionFact(arrayOf(runProjectionCodec, { maximum: 256 }));
    if (field === "attention") return projectionFact(arrayOf(attentionItemCodec, { maximum: 256 }));
    if (field === "capacity") return projectionFact(jsonRecord);
    if (field === "sessions") return discoveredSessionsCodec;
    return undefined;
  };

  const semantic = (
    operation: keyof typeof OPERATOR_PROJECTION_INPUT_SHAPES,
    direction: Parameters<typeof semanticShapeCodec>[1],
  ): Codec<unknown> => semanticShapeCodec(
    operation,
    direction,
    direction === "input" ? OPERATOR_PROJECTION_INPUT_SHAPES[operation] : OPERATOR_PROJECTION_RESULT_SHAPES[operation],
    projectionFieldCodec,
  );

  return {
    [FABRIC_OPERATIONS.projectDiscover]: {
      input: semantic(FABRIC_OPERATIONS.projectDiscover, "input"),
      result: semantic(FABRIC_OPERATIONS.projectDiscover, "result"),
    },
    [FABRIC_OPERATIONS.projectionSnapshot]: {
      input: semantic(FABRIC_OPERATIONS.projectionSnapshot, "input"),
      result: semantic(FABRIC_OPERATIONS.projectionSnapshot, "result"),
    },
    [FABRIC_OPERATIONS.projectionPage]: {
      input: semantic(FABRIC_OPERATIONS.projectionPage, "input"),
      result: projectionPageResultCodec,
    },
    [FABRIC_OPERATIONS.projectionEvents]: {
      input: semantic(FABRIC_OPERATIONS.projectionEvents, "input"),
      result: projectionEventsResultCodec,
    },
    [FABRIC_OPERATIONS.projectionViewPage]: {
      input: operatorViewPageInputCodec,
      result: operatorViewPageResultCodec,
    },
    [FABRIC_OPERATIONS.projectionDetailRead]: {
      input: operatorDetailReadInputCodec,
      result: operatorDetailReadResultCodec,
    },
  } satisfies OperationCodecFragment;
}
