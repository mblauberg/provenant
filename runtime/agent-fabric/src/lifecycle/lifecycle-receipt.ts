export type LifecycleAction =
  | Readonly<{
    action: "workspace-trust";
    outcome: "resolved" | "enrolled" | "already-trusted" | "failed";
    mutated: boolean;
    alreadyTrusted: boolean;
    trustRetained: boolean;
    trustRecordDigest: string | null;
    establishmentKind: "automatic-bootstrap" | "local-operator" | null;
    boundaryKind: "git" | "project-marker" | "non-git" | null;
    boundaryEvidenceDigest: string | null;
    requestAttemptId: string;
    bootstrapAttemptId: string | null;
  }>
  | Readonly<{
    action: "daemon";
    outcome: "attached" | "started";
    mutated: boolean;
    pid: number;
    socketPath: string;
  }>
  | Readonly<{
    action: "custody";
    outcome: "committed" | "replayed" | "reconciled";
    mutated: boolean;
    projectId: string;
    runId: string;
    generation: string;
  }>
  | Readonly<{
    action: "seat-generation";
    outcome: "installed" | "replayed";
    mutated: boolean;
    generation: string;
    previousGeneration: string | null;
  }>
  | Readonly<{
    action: "legacy-bootstrap-provenance";
    outcome: "recorded";
    mutated: boolean;
    generation: string;
  }>
  | Readonly<{
    action: "identity-smoke";
    outcome: "passed" | "failed";
    mutated: false;
    deadlineMs: number;
    elapsedMs: number;
    agentId: string | null;
    mailboxWatermark: number | null;
    code: string | null;
  }>;

export type LifecycleActionReceipt = Readonly<{
  schemaVersion: 1;
  kind: "agent-fabric-lifecycle-action";
  canonicalRoot: string;
  seat: "claude" | "codex";
  generation: string;
  mutated: boolean;
  healthy: boolean;
  actions: readonly LifecycleAction[];
  failure?: Readonly<{
    phase: string;
    message: string;
    code: string | null;
    evidence?: DaemonStaleBuildEvidence;
  }>;
}>;

/** Typed, non-secret context for a live incumbent blocked by build freshness. */
export type DaemonStaleBuildEvidence = Readonly<{
  kind: "daemon-stale-build";
  expectedRuntimeBuildIdentity: string;
  currentRuntimeBuildIdentity: string | null;
  pid: number;
  socketPath: string;
  electionGeneration: number;
  daemonInstanceGeneration: number;
  gate: "reconciliation-required";
}>;

type ReceiptRecord = Record<string, unknown>;

function receiptRecord(value: unknown): ReceiptRecord | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as ReceiptRecord
    : null;
}

function receiptString(value: unknown): string | null {
  return typeof value === "string" && value.length <= 4096 ? value : null;
}

function receiptBoolean(value: unknown): boolean | null {
  return typeof value === "boolean" ? value : null;
}

function receiptNullableString(value: unknown): string | null | undefined {
  return value === null ? null : value === undefined ? undefined : receiptString(value);
}

function validatedDaemonStaleBuildEvidence(value: unknown): DaemonStaleBuildEvidence | null {
  const record = receiptRecord(value);
  if (record === null) return null;
  const expectedKeys = [
    "currentRuntimeBuildIdentity",
    "daemonInstanceGeneration",
    "electionGeneration",
    "expectedRuntimeBuildIdentity",
    "gate",
    "kind",
    "pid",
    "socketPath",
  ];
  const actualKeys = Object.keys(record).sort();
  if (actualKeys.length !== expectedKeys.length || actualKeys.some((key, index) => key !== expectedKeys[index])) return null;
  const expectedRuntimeBuildIdentity = record.expectedRuntimeBuildIdentity;
  const currentRuntimeBuildIdentity = record.currentRuntimeBuildIdentity;
  const pid = record.pid;
  const socketPath = record.socketPath;
  const electionGeneration = record.electionGeneration;
  const daemonInstanceGeneration = record.daemonInstanceGeneration;
  if (
    record.kind !== "daemon-stale-build" ||
    typeof expectedRuntimeBuildIdentity !== "string" ||
    !/^sha256:[a-f0-9]{64}$/u.test(expectedRuntimeBuildIdentity) ||
    !(currentRuntimeBuildIdentity === null || (
      typeof currentRuntimeBuildIdentity === "string" &&
      /^sha256:[a-f0-9]{64}$/u.test(currentRuntimeBuildIdentity)
    )) ||
    !Number.isSafeInteger(pid) || (pid as number) < 1 ||
    typeof socketPath !== "string" || socketPath.length === 0 || socketPath.length > 4096 ||
    !Number.isSafeInteger(electionGeneration) || (electionGeneration as number) < 1 ||
    !Number.isSafeInteger(daemonInstanceGeneration) || (daemonInstanceGeneration as number) < 1 ||
    record.gate !== "reconciliation-required"
  ) return null;
  return {
    kind: "daemon-stale-build",
    expectedRuntimeBuildIdentity,
    currentRuntimeBuildIdentity,
    pid: pid as number,
    socketPath,
    electionGeneration: electionGeneration as number,
    daemonInstanceGeneration: daemonInstanceGeneration as number,
    gate: "reconciliation-required",
  };
}

function validatedLifecycleAction(value: unknown): LifecycleAction | null {
  const record = receiptRecord(value);
  if (record === null) return null;
  const action = record.action;
  if (typeof action !== "string") return null;
  if (action === "workspace-trust") {
    const outcome = record.outcome;
    const mutated = receiptBoolean(record.mutated);
    const alreadyTrusted = receiptBoolean(record.alreadyTrusted);
    const trustRetained = receiptBoolean(record.trustRetained);
    const trustRecordDigest = receiptNullableString(record.trustRecordDigest);
    const establishmentKind = record.establishmentKind;
    const boundaryKind = record.boundaryKind;
    const boundaryEvidenceDigest = receiptNullableString(record.boundaryEvidenceDigest);
    const requestAttemptId = receiptString(record.requestAttemptId);
    const bootstrapAttemptId = receiptNullableString(record.bootstrapAttemptId);
    if (
      !["resolved", "enrolled", "already-trusted", "failed"].includes(String(outcome)) ||
      mutated === null || alreadyTrusted === null || trustRetained === null ||
      trustRecordDigest === undefined ||
      ![null, "automatic-bootstrap", "local-operator"].includes(establishmentKind as string | null) ||
      ![null, "git", "project-marker", "non-git"].includes(boundaryKind as string | null) ||
      boundaryEvidenceDigest === undefined || requestAttemptId === null || bootstrapAttemptId === undefined
    ) return null;
    return {
      action,
      outcome: outcome as "resolved" | "enrolled" | "already-trusted" | "failed",
      mutated,
      alreadyTrusted,
      trustRetained,
      trustRecordDigest,
      establishmentKind: establishmentKind as "automatic-bootstrap" | "local-operator" | null,
      boundaryKind: boundaryKind as "git" | "project-marker" | "non-git" | null,
      boundaryEvidenceDigest,
      requestAttemptId,
      bootstrapAttemptId,
    };
  }
  if (action === "daemon") {
    const outcome = record.outcome;
    const mutated = receiptBoolean(record.mutated);
    const pid = record.pid;
    const socketPath = receiptString(record.socketPath);
    if ((!(["attached", "started"] as unknown[]).includes(outcome)) || mutated === null ||
      !Number.isSafeInteger(pid) || (pid as number) < 1 || socketPath === null) return null;
    return { action, outcome: outcome as "attached" | "started", mutated, pid: pid as number, socketPath };
  }
  if (action === "custody") {
    const outcome = record.outcome;
    const mutated = receiptBoolean(record.mutated);
    const projectId = receiptString(record.projectId);
    const runId = receiptString(record.runId);
    const generation = receiptString(record.generation);
    if ((outcome !== "committed" && outcome !== "replayed" && outcome !== "reconciled") ||
      mutated === null || projectId === null || runId === null || generation === null) return null;
    if ((outcome === "committed") !== mutated || (outcome === "reconciled" && mutated)) return null;
    return { action, outcome, mutated, projectId, runId, generation };
  }
  if (action === "seat-generation") {
    const outcome = record.outcome;
    const mutated = receiptBoolean(record.mutated);
    const generation = receiptString(record.generation);
    const previousGeneration = receiptNullableString(record.previousGeneration);
    if ((!(["installed", "replayed"] as unknown[]).includes(outcome)) || mutated === null ||
      generation === null || previousGeneration === undefined) return null;
    return { action, outcome: outcome as "installed" | "replayed", mutated, generation, previousGeneration };
  }
  if (action === "legacy-bootstrap-provenance") {
    const outcome = record.outcome;
    const mutated = receiptBoolean(record.mutated);
    const generation = receiptString(record.generation);
    if (outcome !== "recorded" || mutated === null || generation === null) return null;
    return { action, outcome, mutated, generation };
  }
  if (action === "identity-smoke") {
    const outcome = record.outcome;
    const mutated = receiptBoolean(record.mutated);
    const deadlineMs = record.deadlineMs;
    const elapsedMs = record.elapsedMs;
    const agentId = receiptNullableString(record.agentId);
    const mailboxWatermark = record.mailboxWatermark;
    const code = receiptNullableString(record.code);
    if ((!(["passed", "failed"] as unknown[]).includes(outcome)) || mutated !== false ||
      !Number.isSafeInteger(deadlineMs) || (deadlineMs as number) < 0 ||
      !Number.isSafeInteger(elapsedMs) || (elapsedMs as number) < 0 || agentId === undefined ||
      !(mailboxWatermark === null || (Number.isSafeInteger(mailboxWatermark) && (mailboxWatermark as number) >= 0)) ||
      code === undefined) return null;
    return {
      action,
      outcome: outcome as "passed" | "failed",
      mutated,
      deadlineMs: deadlineMs as number,
      elapsedMs: elapsedMs as number,
      agentId,
      mailboxWatermark: mailboxWatermark as number | null,
      code,
    };
  }
  return null;
}

export function validateLifecycleActionReceipt(value: unknown): LifecycleActionReceipt | null {
  const record = receiptRecord(value);
  if (record === null || record.schemaVersion !== 1 || record.kind !== "agent-fabric-lifecycle-action") return null;
  const canonicalRoot = receiptString(record.canonicalRoot);
  const seat = record.seat;
  const generation = receiptString(record.generation);
  const mutated = receiptBoolean(record.mutated);
  const healthy = receiptBoolean(record.healthy);
  if (canonicalRoot === null || (seat !== "claude" && seat !== "codex") || generation === null ||
    mutated === null || healthy === null || !Array.isArray(record.actions)) return null;
  const actions = record.actions.map(validatedLifecycleAction);
  if (actions.some((action): action is null => action === null)) return null;
  const validatedActions = actions as LifecycleAction[];
  const failure = record.failure;
  let safeFailure: Readonly<{
    phase: string;
    message: string;
    code: string | null;
    evidence?: DaemonStaleBuildEvidence;
  }> | undefined;
  if (failure !== undefined) {
    const failureRecord = receiptRecord(failure);
    const phase = failureRecord === null ? null : receiptString(failureRecord.phase);
    const message = failureRecord === null ? null : receiptString(failureRecord.message);
    const code = failureRecord === null ? null : receiptNullableString(failureRecord.code);
    const evidenceValue = failureRecord?.evidence;
    if (phase === null || message === null || code === undefined) return null;
    if (evidenceValue === undefined) {
      safeFailure = { phase, message, code };
    } else {
      const evidence = validatedDaemonStaleBuildEvidence(evidenceValue);
      if (evidence === null) return null;
      safeFailure = { phase, message, code, evidence };
    }
  }
  if (mutated !== validatedActions.some((action) => action.mutated)) return null;
  if (healthy && (safeFailure !== undefined || validatedActions.some((action) => action.outcome === "failed"))) return null;
  return {
    schemaVersion: 1,
    kind: "agent-fabric-lifecycle-action",
    canonicalRoot,
    seat,
    generation,
    mutated,
    healthy,
    actions: validatedActions,
    ...(safeFailure === undefined ? {} : { failure: safeFailure }),
  };
}

export function attachLifecycleReceipt<T extends Error>(
  error: T,
  receipt: LifecycleActionReceipt,
): T & { receipt: LifecycleActionReceipt } {
  Object.defineProperty(error, "receipt", { configurable: true, enumerable: true, value: receipt, writable: false });
  return error as T & { receipt: LifecycleActionReceipt };
}

function errorCode(error: unknown): string | null {
  return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string"
    ? error.code
    : null;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function errorStaleBuildEvidence(error: unknown): DaemonStaleBuildEvidence | undefined {
  if (typeof error !== "object" || error === null || !("staleBuildEvidence" in error)) return undefined;
  const evidence = validatedDaemonStaleBuildEvidence(error.staleBuildEvidence);
  return evidence === null ? undefined : evidence;
}

export function lifecycleFailureReceipt(input: {
  canonicalRoot: string;
  seat: "claude" | "codex";
  generation: string;
  actions: readonly LifecycleAction[];
  phase: string;
  cause: unknown;
}): LifecycleActionReceipt {
  const evidence = errorStaleBuildEvidence(input.cause);
  return {
    schemaVersion: 1,
    kind: "agent-fabric-lifecycle-action",
    canonicalRoot: input.canonicalRoot,
    seat: input.seat,
    generation: input.generation,
    mutated: input.actions.some((action) => action.mutated),
    healthy: false,
    actions: input.actions,
    failure: {
      phase: input.phase,
      message: errorMessage(input.cause),
      code: errorCode(input.cause),
      ...(evidence === undefined ? {} : { evidence }),
    },
  };
}

export function currentLedgerGeneration(actions: readonly LifecycleAction[], fallback: string): string {
  for (const action of [...actions].reverse()) {
    if (action.action === "seat-generation" || action.action === "custody") return action.generation;
  }
  return fallback;
}
