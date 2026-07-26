import {
  arrayOf,
  boolean,
  enumeration,
  identifier,
  integer,
  literal,
  objectCodec,
  parserBacked,
  timestamp,
  unionOf,
  type Codec,
} from "../codec.js";
import {
  artifactRefCodec,
  credentialCodec,
  positiveInteger,
  projectionFact,
  text,
} from "./common.js";

type RecordValidator = (
  value: Record<string, unknown> | undefined,
  enclosing: Record<string, unknown>,
  revision: unknown,
  path: string,
) => void;

export function createRunProjectionCodecs(dependencies: Readonly<{
  declaredRunProgressCodec: Codec<unknown>;
  workRowCodec: Codec<unknown>;
  agentRowCodec: Codec<unknown>;
  evidenceRowCodec: Codec<unknown>;
  activityRowCodec: Codec<unknown>;
  taskDetailRefCodec: Codec<unknown>;
  evidenceDetailRefCodec: Codec<unknown>;
  factCandidates: (fact: Record<string, unknown>) => readonly Record<string, unknown>[];
  validateWorkWorkflow: RecordValidator;
  validateAgentTopology: (
    topology: Record<string, unknown> | undefined,
    agentId: unknown,
    revision: unknown,
    path: string,
  ) => void;
}>): Readonly<{
  target: Codec<unknown>;
  input: Codec<unknown>;
  result: Codec<unknown>;
}> {
  const coordinationTarget = objectCodec({
    kind: literal("coordination-run"),
    coordinationRunId: identifier,
  });
  const deliveryTarget = objectCodec({
    kind: literal("delivery-workstream"),
    coordinationRunId: identifier,
    deliveryRunId: identifier,
    workstreamId: identifier,
  });
  const target = unionOf([coordinationTarget, deliveryTarget]);
  const observation = (valueCodec: Codec<unknown>): Codec<unknown> => unionOf([
    objectCodec({ observation: literal("Observed"), value: valueCodec }),
    objectCodec({ observation: literal("Unobserved") }),
    objectCodec({ observation: literal("Unknown"), reason: literal("ContradictoryFacts") }),
  ]);
  const identity = objectCodec({
    acceptedScope: observation(artifactRefCodec),
    currentPlan: observation(objectCodec({
      artifactRef: artifactRefCodec,
      planRevision: positiveInteger,
    })),
    lead: observation(identifier),
    phase: observation(text),
    health: observation(enumeration([
      "healthy", "degraded", "blocked", "quarantined", "unknown",
    ])),
    currentMilestone: observation(text),
    nextMilestone: observation(text),
    lastEventAt: observation(timestamp),
  });
  const composition = objectCodec({
    projectSessionId: identifier,
    target,
    identity: projectionFact(identity),
    declaredProgress: projectionFact(observation(dependencies.declaredRunProgressCodec)),
  });
  const issue = unionOf([
    objectCodec({
      kind: literal("gate"),
      scope: coordinationTarget,
      gateId: identifier,
      gateRevision: positiveInteger,
      status: enumeration(["pending", "deferred"]),
    }),
    objectCodec({
      kind: literal("task"),
      scope: target,
      taskId: identifier,
      taskRevision: positiveInteger,
      state: enumeration(["blocked", "degraded"]),
      detailRef: dependencies.taskDetailRefCodec,
    }),
    objectCodec({
      kind: literal("failed-check"),
      scope: target,
      taskId: identifier,
      taskRevision: positiveInteger,
      checkId: identifier,
      detailRef: dependencies.taskDetailRefCodec,
    }),
    objectCodec({
      kind: literal("task-fact-conflict"),
      scope: target,
      taskId: identifier,
      taskRevision: positiveInteger,
      detailRef: dependencies.taskDetailRefCodec,
    }),
    objectCodec({
      kind: literal("evidence-conflict"),
      scope: target,
      evidenceId: identifier,
      evidenceRevision: positiveInteger,
      detailRef: dependencies.evidenceDetailRefCodec,
    }),
  ]);
  const entry = (valueCodec: Codec<unknown>): Codec<unknown> => objectCodec(
    { value: valueCodec },
    { runScope: target },
  );
  const sameTarget = (
    left: Record<string, unknown>,
    right: Record<string, unknown>,
  ): boolean => (
    left.kind === right.kind &&
    left.coordinationRunId === right.coordinationRunId &&
    (left.kind !== "delivery-workstream" || (
      left.deliveryRunId === right.deliveryRunId &&
      left.workstreamId === right.workstreamId
    ))
  );
  const validateWork = (
    row: Record<string, unknown>,
    pageTarget: Record<string, unknown>,
    projectSessionId: unknown,
    snapshotRevision: unknown,
    path: string,
  ): void => {
    const fact = row.fact as Record<string, unknown>;
    if (row.itemRevision !== fact.revision) {
      throw new TypeError(`${path} item revision does not match fact revision`);
    }
    for (const candidate of dependencies.factCandidates(fact)) {
      const summary = candidate.summary as Record<string, unknown>;
      const detailRef = candidate.detailRef as Record<string, unknown>;
      if (detailRef.taskId !== row.itemId) {
        throw new TypeError(`${path} task identity does not match reference`);
      }
      const workflow = summary.workflow as Record<string, unknown> | undefined;
      dependencies.validateWorkWorkflow(
        workflow,
        summary,
        snapshotRevision,
        `${path}.fact.workflow`,
      );
      if (workflow === undefined) continue;
      const coordination = workflow.coordinationRun as Record<string, unknown>;
      if (
        coordination.projectSessionId !== projectSessionId ||
        coordination.coordinationRunId !== pageTarget.coordinationRunId
      ) {
        throw new TypeError(`${path}.fact.workflow coordination scope does not match page target`);
      }
      if (pageTarget.kind === "delivery-workstream") {
        const workstream = workflow.workstream as Record<string, unknown>;
        if (
          workstream.observation !== "Observed" ||
          workstream.deliveryRunId !== pageTarget.deliveryRunId ||
          workstream.workstreamId !== pageTarget.workstreamId
        ) {
          throw new TypeError(`${path}.fact.workflow workstream scope does not match page target`);
        }
      }
    }
  };
  const pageVariant = (section: string, valueCodec: Codec<unknown>): Codec<unknown> => objectCodec({
    status: literal("page"),
    projectSessionId: identifier,
    target,
    section: literal(section),
    entries: arrayOf(entry(valueCodec), { maximum: 256 }),
    nextCursor: integer(),
    hasMore: boolean,
    snapshotRevision: positiveInteger,
    readTransactionId: identifier,
  }, { composition });
  const resultBase = unionOf([
    pageVariant("work", dependencies.workRowCodec),
    pageVariant("agents", dependencies.agentRowCodec),
    pageVariant("evidence", dependencies.evidenceRowCodec),
    pageVariant("activity", dependencies.activityRowCodec),
    pageVariant("issues", issue),
    objectCodec({
      status: literal("resnapshot-required"),
      projectSessionId: identifier,
      target,
      section: enumeration(["work", "agents", "evidence", "activity", "issues"]),
      reason: literal("snapshot-mismatch"),
      currentSnapshotRevision: positiveInteger,
      snapshotCursor: integer(),
    }),
  ]);
  const result = parserBacked(
    resultBase,
    (value) => {
      const page = value as Record<string, unknown>;
      if (page.status !== "page") return value;
      const pageTarget = page.target as Record<string, unknown>;
      const pageComposition = page.composition as Record<string, unknown> | undefined;
      if (
        pageComposition !== undefined &&
        (
          pageComposition.projectSessionId !== page.projectSessionId ||
          !sameTarget(pageComposition.target as Record<string, unknown>, pageTarget)
        )
      ) {
        throw new TypeError("runProjectionPage composition scope does not match page target");
      }
      const entries = page.entries as Array<Record<string, unknown>>;
      for (const [index, projectedEntry] of entries.entries()) {
        const path = `runProjectionPage.entries[${String(index)}]`;
        if (
          projectedEntry.runScope !== undefined &&
          !sameTarget(projectedEntry.runScope as Record<string, unknown>, pageTarget)
        ) {
          throw new TypeError(`${path}.runScope does not match page target`);
        }
        const projected = projectedEntry.value as Record<string, unknown>;
        if (page.section !== "issues") {
          const fact = projected.fact as Record<string, unknown>;
          if (projected.itemRevision !== fact.revision) {
            throw new TypeError(`${path} item revision does not match fact revision`);
          }
        }
        if (page.section === "work") {
          validateWork(
            projected,
            pageTarget,
            page.projectSessionId,
            page.snapshotRevision,
            path,
          );
        } else if (page.section === "agents") {
          const fact = projected.fact as Record<string, unknown>;
          for (const candidate of dependencies.factCandidates(fact)) {
            const summary = candidate.summary as Record<string, unknown>;
            dependencies.validateAgentTopology(
              summary.topology as Record<string, unknown> | undefined,
              projected.itemId,
              page.snapshotRevision,
              `${path}.fact.topology`,
            );
          }
        } else if (page.section === "issues") {
          const issueScope = projected.scope as Record<string, unknown>;
          const expectedScope = projected.kind === "gate"
            ? {
                kind: "coordination-run",
                coordinationRunId: pageTarget.coordinationRunId,
              }
            : pageTarget;
          if (!sameTarget(issueScope, expectedScope)) {
            throw new TypeError(`${path}.value.scope does not match page target`);
          }
          if (
            (
              projected.kind === "task" ||
              projected.kind === "failed-check" ||
              projected.kind === "task-fact-conflict"
            ) &&
            (
              (projected.detailRef as Record<string, unknown>).taskId !== projected.taskId ||
              (projected.detailRef as Record<string, unknown>).expectedRevision !== projected.taskRevision
            )
          ) {
            throw new TypeError(`${path}.value task identity or revision does not match reference`);
          }
          if (
            projected.kind === "evidence-conflict" &&
            (
              (projected.detailRef as Record<string, unknown>).evidenceId !== projected.evidenceId ||
              (projected.detailRef as Record<string, unknown>).expectedRevision !== projected.evidenceRevision
            )
          ) {
            throw new TypeError(`${path}.value evidence identity or revision does not match reference`);
          }
        }
      }
      return value;
    },
    resultBase.example,
  );
  const input = objectCodec({
    credential: credentialCodec,
    projectId: identifier,
    projectSessionId: identifier,
    target,
    snapshotRevision: positiveInteger,
    section: enumeration(["work", "agents", "evidence", "activity", "issues"]),
    cursor: integer(),
    limit: integer({ minimum: 1, maximum: 100 }),
  });
  return { target, input, result };
}
