import type {
  OperatorActionCommitRequest,
  OperatorActionIntent,
  OperatorActionReceipt,
  OperatorActionReconcileRequest,
  OperatorActionStatus,
  Sha256Digest,
  Timestamp,
} from "@local/agent-fabric-protocol";
import { parseTimestamp, requiredOperatorActionForIntent } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError, type AuthenticatedOperatorContext } from "../project-session/contracts.js";
import { canonicalJson, type Row } from "../persistence/row-codec.js";
import type { AuthenticatedOperatorCredential } from "../operator/store.js";
import {
  OperatorActionJournal,
  digestValue,
  nonLaunchReceipt,
  parseStoredReceipt,
  type StoredPendingAction,
  type StoredPreviewEnvelope,
} from "../operator/action-journal.js";
import type { OperatorActionCurrentState } from "../operator/action-store.js";

export type LifecycleRecoveryIntent = Extract<OperatorActionIntent, { kind: "agent-lifecycle-recovery" }>;

export type OperatorLifecycleRecoveryCurrentState = Readonly<{
  revision: number;
  projectSessionId: string;
  coordinationRunId: string;
  agentId: string;
  sessionRevision: number;
  sessionGeneration: number;
  runRevision: number;
  agentRevision: number;
  source: LifecycleRecoveryIntent["source"];
  sourceRevision: number;
  principalGeneration: number;
  providerGeneration: number;
  bridgeGeneration: number;
  contextRevision: number;
  bridgeOwnerKind: LifecycleRecoveryIntent["bridgeOwnerKind"];
  chairLeaseGeneration: number | null;
  gate: Readonly<{ gateId: string; revision: number; status: "approved" }>;
  recoveryCapability: Readonly<{
    capabilityId: string;
    revision: number;
    capabilityHash: string;
  }> | null;
  checkpoint: Readonly<{
    ref: Extract<LifecycleRecoveryIntent, { path: "fresh-rotate" }>["checkpointRef"];
    digest: string;
    validationReceiptDigest: string | null;
  }> | null;
}>;

export type OperatorLifecycleRecoveryInspection = Readonly<{
  intent: LifecycleRecoveryIntent;
  inspectionDigest: Sha256Digest;
}>;

export type OperatorLifecycleRecoveryCommit = Readonly<{
  status: "committed" | "ambiguous" | "pending" | "no-effect";
  recoveryId: string;
  path: LifecycleRecoveryIntent["path"];
  evidenceDigest: Sha256Digest;
}>;

export interface OperatorLifecycleRecoveryCustodyPort {
  readLifecycleRecoveryCurrentState(intent: LifecycleRecoveryIntent): Promise<OperatorLifecycleRecoveryCurrentState>;
  inspectLifecycleRecovery(intent: LifecycleRecoveryIntent): Promise<OperatorLifecycleRecoveryInspection>;
  prepareLifecycleFreshRotateInTransaction(input: Readonly<{
    inspection: OperatorLifecycleRecoveryInspection;
    operatorId: string;
    operatorCommandId: string;
  }>): OperatorLifecycleRecoveryCommit;
  prepareLifecycleAbandonInTransaction(input: Readonly<{
    inspection: OperatorLifecycleRecoveryInspection;
    operatorId: string;
    operatorCommandId: string;
  }>): OperatorLifecycleRecoveryCommit;
  lifecycleRecoveryStatus(operatorId: string, operatorCommandId: string): OperatorLifecycleRecoveryCommit;
  reconcileLifecycleRecovery(operatorId: string, operatorCommandId: string): Promise<OperatorLifecycleRecoveryCommit>;
}

/**
 * Byte-moved from `OperatorActionStore#readCurrentState`'s `agent-lifecycle-recovery` branch,
 * `#validateCurrentState`'s `agent-lifecycle-recovery` branch, `#commitLifecycleRecovery`,
 * `#lifecycleRecoveryStatus`/`lifecycleRecoveryStatusResult`, and `#reconcileLifecycleRecovery`
 * (S4a). Preserves: inspection outside the transaction, all mutation in one immediate
 * transaction, and zero provider I/O. Auth (`authenticateCommand`) is injected from the facade,
 * which keeps ownership of authentication per the S4 plan.
 */
export class OperatorLifecycleActionAdapter {
  readonly #database: Database.Database;
  readonly #journal: OperatorActionJournal;
  readonly #custody: OperatorLifecycleRecoveryCustodyPort;
  readonly #clock: () => number;
  readonly #actionSessionId: (authenticated: AuthenticatedOperatorCredential, intent: OperatorActionIntent) => string;

  constructor(options: Readonly<{
    database: Database.Database;
    journal: OperatorActionJournal;
    custody: OperatorLifecycleRecoveryCustodyPort;
    clock: () => number;
    actionSessionId: (authenticated: AuthenticatedOperatorCredential, intent: OperatorActionIntent) => string;
  }>) {
    this.#database = options.database;
    this.#journal = options.journal;
    this.#custody = options.custody;
    this.#clock = options.clock;
    this.#actionSessionId = options.actionSessionId;
  }

  async readCurrentState(intent: LifecycleRecoveryIntent): Promise<Extract<OperatorActionCurrentState, { kind: "agent-lifecycle-recovery" }>> {
    const state = await this.#custody.readLifecycleRecoveryCurrentState(intent);
    return { kind: "agent-lifecycle-recovery", revision: state.revision, state };
  }

  async commit(
    context: AuthenticatedOperatorContext,
    request: OperatorActionCommitRequest,
    envelope: StoredPreviewEnvelope,
    intent: LifecycleRecoveryIntent,
    authenticated: AuthenticatedOperatorCredential,
    payloadHash: string,
    current: OperatorActionCurrentState,
  ): Promise<OperatorActionReceipt> {
    const custody = this.#custody;
    const inspection = await custody.inspectLifecycleRecovery(intent);
    if (canonicalJson(inspection.intent) !== canonicalJson(intent)) {
      throw new ProjectFabricCoreError("STALE_REVISION", "lifecycle recovery inspection crossed its intent");
    }
    const provisionalState = {
      status: "pending" as const,
      commandId: request.command.commandId,
      phase: "prepared" as const,
      attemptGeneration: 1,
    };
    const provisionalReceipt: OperatorActionReceipt = {
      commandId: request.command.commandId,
      previewId: envelope.preview.previewId,
      previewRevision: envelope.preview.previewRevision,
      intentDigest: envelope.preview.intentDigest,
      beforeStateDigest: envelope.preview.beforeStateDigest,
      afterStateDigest: digestValue(provisionalState, "operatorLifecycleRecoveryCommit.provisionalStateDigest"),
      evidenceRefs: envelope.preview.evidenceRefs,
      committedAt: toTimestamp(this.#clock(), "operatorLifecycleRecoveryCommit.committedAt"),
    };
    const prepare = this.#database.transaction(():
      | { kind: "replay"; receipt: OperatorActionReceipt }
      | { kind: "prepared"; receipt: OperatorActionReceipt } => {
      const concurrentReplay = this.#journal.commandReplay(context.operatorId, request.command.commandId, payloadHash);
      if (concurrentReplay !== null) return { kind: "replay", receipt: concurrentReplay };
      const latest = this.#journal.previewRow(request.previewId);
      this.#journal.assertPreviewClaim(latest, request.command.commandId);
      const outcome = intent.path === "fresh-rotate"
        ? custody.prepareLifecycleFreshRotateInTransaction({
            inspection,
            operatorId: context.operatorId,
            operatorCommandId: request.command.commandId,
          })
        : custody.prepareLifecycleAbandonInTransaction({
            inspection,
            operatorId: context.operatorId,
            operatorCommandId: request.command.commandId,
          });
      const receipt: OperatorActionReceipt = outcome.status === "committed"
        ? {
            ...provisionalReceipt,
            afterStateDigest: digestValue(outcome, "operatorLifecycleRecoveryCommit.afterStateDigest"),
          }
        : provisionalReceipt;
      const action: { status: "terminal"; commandId: string; receipt: OperatorActionReceipt } | StoredPendingAction = outcome.status === "committed"
        ? { status: "terminal", commandId: request.command.commandId, receipt }
        : { ...provisionalState, receipt };
      this.#database.prepare(`
        UPDATE operator_previews SET preview_json=?, confirmed_command_id=? WHERE preview_id=?
      `).run(
        canonicalJson({ preview: envelope.preview, action }),
        request.command.commandId,
        request.previewId,
      );
      this.#journal.insertCommand(
        context,
        request.command,
        request.projectId,
        this.#actionSessionId(authenticated, intent),
        requiredOperatorActionForIntent(intent),
        payloadHash,
        current,
        outcome,
        receipt,
        "committed",
      );
      return { kind: "prepared", receipt };
    });
    return prepare.immediate().receipt;
  }

  status(
    operatorId: string,
    commandId: string,
    envelope: StoredPreviewEnvelope,
  ): OperatorActionStatus {
    if (envelope.action === null) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "lifecycle recovery custody runtime is unavailable");
    }
    return lifecycleRecoveryStatusResult(
      this.#custody.lifecycleRecoveryStatus(operatorId, commandId),
      commandId,
      envelope,
    );
  }

  async reconcile(
    context: AuthenticatedOperatorContext,
    request: OperatorActionReconcileRequest,
    stored: Row,
    envelope: StoredPreviewEnvelope,
    authenticated: AuthenticatedOperatorCredential,
    payloadHash: string,
  ): Promise<OperatorActionStatus> {
    const custody = this.#custody;
    if (envelope.action === null || envelope.preview.intent.kind !== "agent-lifecycle-recovery") {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "lifecycle recovery custody runtime is unavailable");
    }
    this.#journal.assertStoredPreviewScope(stored, authenticated, request.projectId, envelope.preview.intent, this.#actionSessionId);
    const replay = this.#journal.reconcileReplay(context.operatorId, request.command.commandId, payloadHash);
    if (replay !== null) return replay;
    if (request.command.commandId === request.targetCommandId) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "reconciliation requires a distinct command ID");
    }
    const currentStatus = this.status(context.operatorId, request.targetCommandId, envelope);
    if (
      currentStatus.status !== request.expectedStatus ||
      (currentStatus.status === "pending" || currentStatus.status === "ambiguous") &&
        currentStatus.attemptGeneration !== request.expectedAttemptGeneration
    ) {
      throw new ProjectFabricCoreError("STALE_REVISION", "lifecycle recovery reconciliation target changed");
    }
    const observed = await custody.reconcileLifecycleRecovery(
      context.operatorId,
      request.targetCommandId,
    );
    const result = lifecycleRecoveryStatusResult(
      observed,
      request.targetCommandId,
      envelope,
      request.expectedAttemptGeneration + 1,
    );
    const baseReceipt = parseStoredReceipt(envelope.action);
    if (result.status === "committed") {
      this.#journal.updateStoredAction(envelope.preview.previewId, envelope.preview, {
        status: "terminal",
        commandId: request.targetCommandId,
        receipt: result.receipt,
      });
      this.#database.prepare(`
        UPDATE operator_commands SET result_json=?, after_json=?
         WHERE operator_id=? AND command_id=?
      `).run(
        canonicalJson(result.receipt),
        canonicalJson(observed),
        context.operatorId,
        request.targetCommandId,
      );
    } else if (result.status === "pending") {
      this.#journal.updateStoredAction(envelope.preview.previewId, envelope.preview, {
        status: "pending",
        commandId: request.targetCommandId,
        phase: result.phase,
        attemptGeneration: result.attemptGeneration,
        receipt: baseReceipt,
      });
    }
    this.#database.transaction(() => {
      this.#journal.insertCommand(
        context,
        request.command,
        request.projectId,
        this.#actionSessionId(authenticated, envelope.preview.intent),
        requiredOperatorActionForIntent(envelope.preview.intent),
        payloadHash,
        currentStatus,
        result,
        result,
        "committed",
      );
    })();
    return result;
  }
}

export function lifecycleRecoveryStatusResult(
  observed: OperatorLifecycleRecoveryCommit,
  commandId: string,
  envelope: StoredPreviewEnvelope,
  attemptGeneration?: number,
): OperatorActionStatus {
  if (envelope.action === null) throw new Error("lifecycle recovery preview has no action state");
  if (observed.status === "committed") {
    const receipt: OperatorActionReceipt = {
      ...parseStoredReceipt(envelope.action),
      afterStateDigest: digestValue(observed, "operatorLifecycleRecoveryStatus.afterStateDigest"),
    };
    return { status: "committed", commandId, receipt: nonLaunchReceipt(receipt) };
  }
  if (observed.status === "no-effect") {
    return {
      status: "rejected",
      commandId,
      intentDigest: envelope.preview.intentDigest,
      code: "state-changed",
      evidenceRefs: [],
    };
  }
  const generation = attemptGeneration ?? (
    envelope.action.status === "pending" || envelope.action.status === "ambiguous"
      ? envelope.action.attemptGeneration
      : 1
  );
  return {
    status: "pending",
    commandId,
    intentDigest: envelope.preview.intentDigest,
    phase: observed.status === "ambiguous" ? "observing" : "prepared",
    attemptGeneration: generation,
  };
}

/**
 * Byte-moved from `OperatorActionStore#validateCurrentState`'s `agent-lifecycle-recovery` arm.
 */
export function validateLifecycleRecoveryCurrentState(
  intent: LifecycleRecoveryIntent,
  current: OperatorActionCurrentState,
): void {
  if (current.kind !== "agent-lifecycle-recovery") {
    throw new TypeError("lifecycle recovery intent received another current-state family");
  }
  const state = current.state;
  if (
    state.revision !== intent.expectedAgentRevision ||
    state.projectSessionId !== intent.projectSessionId ||
    state.coordinationRunId !== intent.coordinationRunId ||
    state.agentId !== intent.agentId ||
    state.sessionRevision !== intent.expectedSessionRevision ||
    state.sessionGeneration !== intent.expectedSessionGeneration ||
    state.runRevision !== intent.expectedRunRevision ||
    state.agentRevision !== intent.expectedAgentRevision ||
    canonicalJson(state.source) !== canonicalJson(intent.source) ||
    state.sourceRevision !== intent.expectedSourceRevision ||
    state.principalGeneration !== intent.expectedPrincipalGeneration ||
    state.providerGeneration !== intent.expectedProviderGeneration ||
    state.bridgeGeneration !== intent.expectedBridgeGeneration ||
    state.contextRevision !== intent.expectedContextRevision ||
    state.bridgeOwnerKind !== intent.bridgeOwnerKind ||
    state.chairLeaseGeneration !== intent.expectedChairLeaseGeneration
  ) {
    throw new ProjectFabricCoreError("STALE_GENERATION", "lifecycle recovery source binding changed");
  }
  if (
    state.gate.gateId !== intent.gateId ||
    state.gate.revision !== intent.expectedGateRevision ||
    state.gate.status !== intent.expectedGateStatus
  ) {
    throw new ProjectFabricCoreError("GATE_BLOCKED", "lifecycle recovery gate binding changed");
  }
  if (intent.path === "fresh-rotate") {
    if (
      state.recoveryCapability === null ||
      state.recoveryCapability.capabilityId !== intent.recoveryCapabilityId ||
      state.recoveryCapability.revision !== intent.expectedRecoveryCapabilityRevision ||
      state.recoveryCapability.capabilityHash !== intent.recoveryCapabilityHash ||
      state.checkpoint === null ||
      canonicalJson(state.checkpoint.ref) !== canonicalJson(intent.checkpointRef) ||
      state.checkpoint.digest !== intent.checkpointDigest ||
      state.checkpoint.validationReceiptDigest !== intent.checkpointValidationReceiptDigest
    ) {
      throw new ProjectFabricCoreError("STALE_REVISION", "lifecycle recovery capability or checkpoint changed");
    }
  }
}

function toTimestamp(milliseconds: number, path: string): Timestamp {
  return parseTimestamp(new Date(milliseconds).toISOString(), path);
}
