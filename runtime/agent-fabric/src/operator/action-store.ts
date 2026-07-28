import type {
  ArtifactRef,
  GitCurrentState,
  JsonValue,
  OperatorAction,
  OperatorActionCommitRequest,
  OperatorActionIntent,
  OperatorActionPreview,
  OperatorActionPreviewRequest,
  OperatorActionReceipt,
  OperatorActionRejectionCode,
  OperatorActionReconcileRequest,
  OperatorActionStatus,
  OperatorActionStatusRequest,
  ProjectSessionLaunchPrepareRequest,
  ProjectSessionLaunchCurrentState,
  LaunchProviderActionJournalRefV1,
  McpSeatProvisioningDescriptorV1,
  RegisteredExternalEffectState,
  ScopedGate,
  Sha256Digest,
  Timestamp,
} from "@local/agent-fabric-protocol";
import {
  FABRIC_OPERATIONS,
  assertGitIntentState,
  assertPromotionIntentGate,
  assertRegisteredExternalEffectContract,
  parseIdentifier,
  parseOperationResult,
  parseTimestamp,
  requiredOperatorActionForIntent,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError, type AuthenticatedOperatorContext, type CoreServiceOptions } from "../project-session/contracts.js";
import type {
  LaunchCustodyIntent,
  LaunchDispatchHandle,
  LaunchInspection,
  ChairRecoveryCurrentState,
  ChairLiveHandoffCurrentState,
} from "../project-session/launch-custody.js";
import { canonicalJson, integer, isRow, row, sha256, text, type Row } from "../persistence/row-codec.js";
import type { AuthenticatedOperatorCredential, OperatorStore } from "./store.js";
import {
  OperatorActionJournal,
  digestValue,
  nonLaunchReceipt,
  parseReceipt,
  parseRejectedStatus,
  parseStoredPreview,
  parseStoredReceipt,
  statusFromAction,
  type StoredPendingAction,
  type StoredPreviewEnvelope,
} from "./action-journal.js";
import {
  OperatorLifecycleActionAdapter,
  type OperatorLifecycleRecoveryCurrentState,
  type OperatorLifecycleRecoveryCustodyPort,
  validateLifecycleRecoveryCurrentState,
} from "../lifecycle/operator-actions.js";
import {
  OperatorChairLiveHandoffActionAdapter,
  type OperatorChairLiveHandoffCustodyPort,
} from "./chair-live-handoff-actions.js";
import {
  OperatorChairRecoveryActionAdapter,
  type OperatorChairRecoveryCustodyPort,
} from "./chair-recovery-actions.js";

export type OperatorActionCurrentState =
  | {
      kind: "control";
      revision: number;
      lifecycleState: string;
      eligibleActions: readonly ("pause" | "resume" | "cancel" | "steer")[];
      binding?: JsonValue;
    }
  | {
      kind: "project-session-launch";
      revision: number;
      state: ProjectSessionLaunchCurrentState;
    }
  | {
      kind: "project-session-lifecycle";
      revision: number;
      sessionGeneration: number;
      globalStateRevision: number;
      lifecycleState: string;
      drainReceiptRef: ArtifactRef | null;
    }
  | {
      kind: "daemon-lifecycle";
      revision: number;
      daemonGeneration: number;
      globalStateRevision: number;
      lifecycleState: string;
      drainReceiptRef: ArtifactRef | null;
    }
  | { kind: "chair-bridge-recovery"; revision: number; state: ChairRecoveryCurrentState }
  | { kind: "chair-live-handoff"; revision: number; state: ChairLiveHandoffCurrentState }
  | { kind: "agent-lifecycle-recovery"; revision: number; state: OperatorLifecycleRecoveryCurrentState }
  | { kind: "git"; revision: number; state: GitCurrentState }
  | { kind: "git-administration"; revision: number; state: JsonValue }
  | { kind: "registered-external-effect"; revision: number; state: RegisteredExternalEffectState }
  | { kind: "promotion"; revision: number; gate: ScopedGate };

export interface OperatorActionStatePort {
  read(intent: OperatorActionIntent): Promise<OperatorActionCurrentState>;
}

export type OperatorEffectOutcome =
  | { status: "committed"; afterState: JsonValue; effectRef?: ArtifactRef }
  | { status: "pending"; phase: "accepted" | "observing" }
  | { status: "ambiguous"; effectRef: ArtifactRef }
  | {
      status: "rejected";
      code: OperatorActionRejectionCode;
      evidenceRefs: readonly ArtifactRef[];
    };

export type OperatorEffectRequest = {
  commandId: string;
  previewId?: string;
  operatorId?: string;
  projectId?: string;
  projectSessionId?: string;
  principalGeneration?: number;
  operation?: string;
  intent: OperatorActionIntent;
  intentDigest: Sha256Digest;
  beforeStateDigest: Sha256Digest;
  attemptGeneration: number;
  operatorInputRecordDigest?: Sha256Digest;
};

export interface OperatorActionEffectPort {
  prepare?(request: OperatorEffectRequest): void;
  dispatch(request: OperatorEffectRequest): Promise<OperatorEffectOutcome>;
  observe(request: OperatorEffectRequest & { effectRef: ArtifactRef | null }): Promise<OperatorEffectOutcome>;
  status?(commandId: string, intentDigest: Sha256Digest): OperatorActionStatus | null;
  reconcileGit?(input: Readonly<{
    request: OperatorActionReconcileRequest;
    targetIntent: Extract<OperatorActionIntent, { kind: "git" }>;
    intentDigest: Sha256Digest;
    nextAttemptGeneration: number;
  }>): Promise<OperatorActionStatus>;
}

export interface OperatorLaunchCustodyPort {
  readCurrentState(intent: LaunchCustodyIntent): Promise<ProjectSessionLaunchCurrentState>;
  inspect(intent: LaunchCustodyIntent): Promise<LaunchInspection>;
  prepareInTransaction(input: Readonly<{
    inspection: LaunchInspection;
    operatorId: string;
    operatorCommandId: string;
    principal: AuthenticatedOperatorContext;
  }>): LaunchDispatchHandle;
  dispatchPrepared(handle: LaunchDispatchHandle): Promise<unknown>;
  launchProviderActionJournalRefForCommand(
    operatorId: string,
    commandId: string,
  ): LaunchProviderActionJournalRefV1;
  seatProvisioningDescriptorForCommand(
    operatorId: string,
    commandId: string,
  ): McpSeatProvisioningDescriptorV1;
}

type LifecycleRecoveryIntent = Extract<OperatorActionIntent, { kind: "agent-lifecycle-recovery" }>;

export type {
  OperatorLifecycleRecoveryCommit,
  OperatorLifecycleRecoveryCurrentState,
  OperatorLifecycleRecoveryCustodyPort,
  OperatorLifecycleRecoveryInspection,
} from "../lifecycle/operator-actions.js";
export type { OperatorChairLiveHandoffCustodyPort } from "./chair-live-handoff-actions.js";
export type { OperatorChairRecoveryCustodyPort } from "./chair-recovery-actions.js";

export type OperatorActionStoreOptions = CoreServiceOptions & {
  operatorStore: OperatorStore;
  statePort: OperatorActionStatePort;
  effectPort: OperatorActionEffectPort;
  launchCustody?: OperatorLaunchCustodyPort;
  chairRecoveryCustody?: OperatorChairRecoveryCustodyPort;
  chairLiveHandoffCustody?: OperatorChairLiveHandoffCustodyPort;
  lifecycleRecoveryCustody?: OperatorLifecycleRecoveryCustodyPort;
  previewTtlMs?: number;
};

export class OperatorActionStore {
  readonly #database: Database.Database;
  readonly #operatorStore: OperatorStore;
  readonly #statePort: OperatorActionStatePort;
  readonly #effectPort: OperatorActionEffectPort;
  readonly #launchCustody: OperatorLaunchCustodyPort | undefined;
  readonly #chairRecoveryAdapter: OperatorChairRecoveryActionAdapter | undefined;
  readonly #chairLiveHandoffAdapter: OperatorChairLiveHandoffActionAdapter | undefined;
  readonly #lifecycleAdapter: OperatorLifecycleActionAdapter | undefined;
  readonly #clock: () => number;
  readonly #previewTtlMs: number;
  readonly #journal: OperatorActionJournal;
  readonly #inFlightCommits = new Map<string, { payloadHash: string; promise: Promise<OperatorActionReceipt> }>();

  constructor(options: OperatorActionStoreOptions) {
    this.#database = options.database;
    this.#operatorStore = options.operatorStore;
    this.#statePort = options.statePort;
    this.#effectPort = options.effectPort;
    this.#launchCustody = options.launchCustody;
    this.#clock = options.clock ?? Date.now;
    this.#previewTtlMs = options.previewTtlMs ?? 5 * 60_000;
    if (!Number.isSafeInteger(this.#previewTtlMs) || this.#previewTtlMs < 1) {
      throw new TypeError("operator preview TTL must be a positive safe integer");
    }
    this.#journal = new OperatorActionJournal(this.#database, this.#clock);
    this.#lifecycleAdapter = options.lifecycleRecoveryCustody === undefined ? undefined : new OperatorLifecycleActionAdapter({
      database: this.#database,
      journal: this.#journal,
      custody: options.lifecycleRecoveryCustody,
      clock: this.#clock,
      actionSessionId,
    });
    this.#chairLiveHandoffAdapter = options.chairLiveHandoffCustody === undefined ? undefined : new OperatorChairLiveHandoffActionAdapter({
      database: this.#database,
      journal: this.#journal,
      custody: options.chairLiveHandoffCustody,
      clock: this.#clock,
      actionSessionId,
    });
    this.#chairRecoveryAdapter = options.chairRecoveryCustody === undefined ? undefined : new OperatorChairRecoveryActionAdapter({
      database: this.#database,
      journal: this.#journal,
      custody: options.chairRecoveryCustody,
      clock: this.#clock,
      actionSessionId,
    });
  }

  replayLaunchPreview(
    context: AuthenticatedOperatorContext,
    request: ProjectSessionLaunchPrepareRequest,
  ): OperatorActionPreview | undefined {
    const previewId = `preview_${sha256(`${context.operatorId}:${request.command.commandId}`).slice(0, 48)}`;
    const stored = this.#database.prepare(`
      SELECT operator_id, project_session_id, operation, payload_digest, preview_json
        FROM operator_previews WHERE preview_id=?
    `).get(previewId);
    if (!isRow(stored)) return undefined;
    const envelope = parseStoredPreview(text(stored, "preview_json"));
    const intent = envelope.preview.intent;
    const exactBinding =
      text(stored, "operator_id") === context.operatorId &&
      text(stored, "project_session_id") === request.projectSessionId &&
      text(stored, "operation") === "launch" &&
      intent.kind === "project-session-launch" &&
      intent.projectId === request.projectId &&
      intent.projectSessionId === request.projectSessionId &&
      intent.expectedSessionRevision === request.command.expectedRevision &&
      intent.expectedSessionGeneration === request.expectedSessionGeneration &&
      canonicalJson(intent.launchPacketRef) === canonicalJson(request.launchPacketRef);
    const payloadDigest = intent.kind === "project-session-launch"
      ? sha256(canonicalJson(sanitisedPreviewRequest({
          command: request.command,
          projectId: request.projectId,
          intent,
        })))
      : "";
    if (!exactBinding || text(stored, "payload_digest") !== payloadDigest) {
      throw new ProjectFabricCoreError(
        "DEDUPE_CONFLICT",
        "launch preparation command identity was reused with changed input",
      );
    }
    return envelope.preview;
  }

  async preview(
    context: AuthenticatedOperatorContext,
    request: OperatorActionPreviewRequest,
    options?: { allowLaunchIntent?: boolean },
  ): Promise<OperatorActionPreview> {
    if (request.intent.kind === "project-session-launch" && options?.allowLaunchIntent !== true) {
      throw new ProjectFabricCoreError(
        "CAPABILITY_FORBIDDEN",
        "project-session-launch previews must be created via projectSessionLaunchPrepare",
      );
    }
    const authenticated = this.#authenticateCommand(context, request.command, request.projectId, request.intent);
    const current = await this.#readCurrentState(request.intent);
    validateCurrentState(request.intent, request.command.expectedRevision, current);
    const beforeStateDigest = digestValue(current, "operatorActionPreview.beforeStateDigest");
    const intentDigest = digestValue(request.intent, "operatorActionPreview.intentDigest");
    const payloadDigest = sha256(canonicalJson(sanitisedPreviewRequest(request)));
    const previewId = `preview_${sha256(`${context.operatorId}:${request.command.commandId}`).slice(0, 48)}`;
    const expiresAt = toTimestamp(this.#clock() + this.#previewTtlMs, "operatorActionPreview.expiresAt");
    const previewBase = {
      previewId,
      previewRevision: 1,
      intent: request.intent,
      intentDigest,
      beforeStateDigest,
      consequenceClass: consequenceClass(request.intent),
      evidenceRefs: combinedEvidence(request),
      gateIds: request.intent.kind === "promotion"
        ? [request.intent.gateId]
        : request.intent.kind === "agent-lifecycle-recovery"
          ? [parseIdentifier<"GateId">(request.intent.gateId, "operatorActionPreview.gateId")]
        : [],
      confirmationMode: "explicit" as const,
      expiresAt,
    };
    const preview: OperatorActionPreview = {
      ...previewBase,
      previewDigest: digestValue(previewBase, "operatorActionPreview.previewDigest"),
    };
    const transaction = this.#database.transaction((): OperatorActionPreview => {
      const existing = this.#database.prepare(`
        SELECT payload_digest, preview_json FROM operator_previews WHERE preview_id=?
      `).get(previewId);
      if (isRow(existing)) {
        if (text(existing, "payload_digest") !== payloadDigest) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "preview command identity was reused with changed input");
        }
        return parseStoredPreview(text(existing, "preview_json")).preview;
      }
      const projectSessionId = actionSessionId(authenticated, request.intent);
      this.#database.prepare(`
        INSERT INTO operator_previews(
          preview_id, operator_id, project_session_id, operation, payload_digest,
          preview_json, revision, expires_at, confirmed_command_id, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, 1, ?, NULL, ?)
      `).run(
        previewId,
        context.operatorId,
        projectSessionId,
        requiredOperatorActionForIntent(request.intent),
        payloadDigest,
        canonicalJson({ preview, action: null }),
        this.#clock() + this.#previewTtlMs,
        this.#clock(),
      );
      return preview;
    });
    return transaction();
  }

  async commit(
    context: AuthenticatedOperatorContext,
    request: OperatorActionCommitRequest,
  ): Promise<OperatorActionReceipt> {
    const stored = this.#journal.previewRow(request.previewId);
    if (text(stored, "operator_id") !== context.operatorId) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator preview belongs to another operator");
    }
    const envelope = parseStoredPreview(text(stored, "preview_json"));
    const authenticated = this.#authenticateCommand(context, request.command, request.projectId, envelope.preview.intent);
    this.#journal.assertStoredPreviewScope(stored, authenticated, request.projectId, envelope.preview.intent, actionSessionId);
    const payloadHash = sha256(canonicalJson(sanitisedCommitRequest(request)));
    const inFlightKey = `${context.operatorId}:${request.command.commandId}`;
    const inFlight = this.#inFlightCommits.get(inFlightKey);
    if (inFlight !== undefined) {
      if (inFlight.payloadHash !== payloadHash) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "operator command ID was reused with changed input");
      }
      return inFlight.promise;
    }
    const promise = this.#commitAuthenticated(context, request, stored, envelope, authenticated, payloadHash);
    this.#inFlightCommits.set(inFlightKey, { payloadHash, promise });
    const clear = (): void => {
      if (this.#inFlightCommits.get(inFlightKey)?.promise === promise) this.#inFlightCommits.delete(inFlightKey);
    };
    void promise.then(clear, clear);
    return promise;
  }

  async #commitAuthenticated(
    context: AuthenticatedOperatorContext,
    request: OperatorActionCommitRequest,
    stored: Row,
    envelope: StoredPreviewEnvelope,
    authenticated: AuthenticatedOperatorCredential,
    payloadHash: string,
  ): Promise<OperatorActionReceipt> {
    const replay = this.#journal.commandReplay(context.operatorId, request.command.commandId, payloadHash);
    if (replay !== null) return replay;
    this.#journal.assertPreviewClaim(stored, request.command.commandId);
    try {
      assertCommitMatchesPreview(request, envelope.preview, integer(stored, "revision"), this.#clock());
    } catch (error: unknown) {
      const code = Date.parse(envelope.preview.expiresAt) <= this.#clock()
        ? "preview-expired"
        : "preview-stale";
      if (code === "preview-expired" && envelope.preview.intent.kind === "project-session-launch") {
        this.#journal.recordRetryableLaunchExpiry(request, envelope);
      } else {
        this.#journal.recordRejected(context, request, stored, envelope, payloadHash, code, this.#journal.commandReplay.bind(this.#journal));
      }
      throw error;
    }
    const current = await this.#readCurrentState(envelope.preview.intent);
    try {
      validateCurrentState(envelope.preview.intent, request.command.expectedRevision, current);
    } catch (error: unknown) {
      this.#journal.recordRejected(
        context,
        request,
        stored,
        envelope,
        payloadHash,
        rejectionForIntent(envelope.preview.intent),
        this.#journal.commandReplay.bind(this.#journal),
      );
      throw error;
    }
    const currentDigest = digestValue(current, "operatorActionCommit.currentStateDigest");
    if (currentDigest !== envelope.preview.beforeStateDigest) {
      this.#journal.recordRejected(
        context,
        request,
        stored,
        envelope,
        payloadHash,
        "state-changed",
        this.#journal.commandReplay.bind(this.#journal),
      );
      throw new ProjectFabricCoreError("STALE_REVISION", "operator action state changed after preview");
    }
    if (envelope.preview.intent.kind === "project-session-launch") {
      return await this.#commitLaunch(
        context,
        request,
        envelope,
        envelope.preview.intent,
        authenticated,
        payloadHash,
        current,
      );
    }
    if (envelope.preview.intent.kind === "chair-bridge-recovery") {
      return await this.#commitChairRecovery(
        context,
        request,
        envelope,
        envelope.preview.intent,
        authenticated,
        payloadHash,
        current,
      );
    }
    if (envelope.preview.intent.kind === "chair-live-handoff") {
      return await this.#commitChairLiveHandoff(
        context,
        request,
        envelope,
        envelope.preview.intent,
        authenticated,
        payloadHash,
        current,
      );
    }
    if (envelope.preview.intent.kind === "agent-lifecycle-recovery") {
      return await this.#commitLifecycleRecovery(
        context,
        request,
        envelope,
        envelope.preview.intent,
        authenticated,
        payloadHash,
        current,
      );
    }
    const preparedState = {
      status: "pending" as const,
      commandId: request.command.commandId,
      phase: "prepared" as const,
      attemptGeneration: 1,
      operatorInputRecordDigest: digestValue({
        actor: request.command.actor,
        provenance: request.command.provenance,
        evidenceRefs: request.command.evidenceRefs,
        confirmation: request.confirmation,
      }, "operatorActionCommit.operatorInputRecordDigest"),
    };
    const preparedReceipt: OperatorActionReceipt = {
      commandId: request.command.commandId,
      previewId: envelope.preview.previewId,
      previewRevision: envelope.preview.previewRevision,
      intentDigest: envelope.preview.intentDigest,
      beforeStateDigest: envelope.preview.beforeStateDigest,
      afterStateDigest: digestValue(preparedState, "operatorActionCommit.preparedStateDigest"),
      evidenceRefs: envelope.preview.evidenceRefs,
      committedAt: toTimestamp(this.#clock(), "operatorActionCommit.committedAt"),
    };
    const effectRequest: OperatorEffectRequest = {
      commandId: request.command.commandId,
      previewId: envelope.preview.previewId,
      operatorId: context.operatorId,
      projectId: request.projectId,
      projectSessionId: actionSessionId(authenticated, envelope.preview.intent),
      principalGeneration: context.principalGeneration,
      operation: requiredOperatorActionForIntent(envelope.preview.intent),
      intent: envelope.preview.intent,
      intentDigest: envelope.preview.intentDigest,
      beforeStateDigest: envelope.preview.beforeStateDigest,
      attemptGeneration: 1,
      operatorInputRecordDigest: preparedState.operatorInputRecordDigest,
    };
    const prepare = this.#database.transaction((): OperatorActionReceipt | null => {
      const concurrentReplay = this.#journal.commandReplay(
        context.operatorId,
        request.command.commandId,
        payloadHash,
      );
      if (concurrentReplay !== null) return concurrentReplay;
      const latest = this.#journal.previewRow(request.previewId);
      this.#journal.assertPreviewClaim(latest, request.command.commandId);
      this.#database.prepare(`
        UPDATE operator_previews SET preview_json=?, confirmed_command_id=? WHERE preview_id=?
      `).run(
        canonicalJson({ preview: envelope.preview, action: { ...preparedState, receipt: preparedReceipt } }),
        request.command.commandId,
        request.previewId,
      );
      this.#journal.insertCommand(
        context,
        request.command,
        request.projectId,
        actionSessionId(authenticated, envelope.preview.intent),
        requiredOperatorActionForIntent(envelope.preview.intent),
        payloadHash,
        current,
        preparedState,
        preparedReceipt,
        "committed",
      );
      this.#effectPort.prepare?.(effectRequest);
      return null;
    });
    const concurrentReplay = prepare();
    if (concurrentReplay !== null) return concurrentReplay;

    let outcome: OperatorEffectOutcome;
    try {
      outcome = await this.#effectPort.dispatch(effectRequest);
    } catch {
      return preparedReceipt;
    }
    return this.#journal.applyEffectOutcome(
      context.operatorId,
      request.command.commandId,
      envelope.preview,
      preparedReceipt,
      outcome,
      1,
    );
  }

  async #commitLaunch(
    context: AuthenticatedOperatorContext,
    request: OperatorActionCommitRequest,
    envelope: StoredPreviewEnvelope,
    intent: Extract<OperatorActionIntent, { kind: "project-session-launch" }>,
    authenticated: AuthenticatedOperatorCredential,
    payloadHash: string,
    current: OperatorActionCurrentState,
  ): Promise<OperatorActionReceipt> {
    const launchCustody = this.#launchCustody;
    if (launchCustody === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "launch custody runtime is unavailable");
    }
    const inspection = await launchCustody.inspect(intent);
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
      afterStateDigest: digestValue(provisionalState, "operatorLaunchCommit.provisionalStateDigest"),
      evidenceRefs: envelope.preview.evidenceRefs,
      committedAt: toTimestamp(this.#clock(), "operatorLaunchCommit.committedAt"),
    };
    const prepare = this.#database.transaction(():
      | { kind: "replay"; receipt: OperatorActionReceipt }
      | { kind: "prepared"; receipt: OperatorActionReceipt; handle: LaunchDispatchHandle } => {
      const concurrentReplay = this.#journal.commandReplay(context.operatorId, request.command.commandId, payloadHash);
      if (concurrentReplay !== null) return { kind: "replay", receipt: concurrentReplay };
      const latest = this.#journal.previewRow(request.previewId);
      this.#journal.assertPreviewClaim(latest, request.command.commandId);
      this.#database.prepare(`
        UPDATE operator_previews SET preview_json=?, confirmed_command_id=? WHERE preview_id=?
      `).run(
        canonicalJson({ preview: envelope.preview, action: { ...provisionalState, receipt: provisionalReceipt } }),
        request.command.commandId,
        request.previewId,
      );
      this.#journal.insertCommand(
        context,
        request.command,
        request.projectId,
        actionSessionId(authenticated, intent),
        "launch",
        payloadHash,
        current,
        provisionalState,
        provisionalReceipt,
        "committed",
      );
      const handle = launchCustody.prepareInTransaction({
        inspection,
        operatorId: context.operatorId,
        operatorCommandId: request.command.commandId,
        principal: context,
      });
      const launchProviderActionJournalRef = launchCustody.launchProviderActionJournalRefForCommand(
        context.operatorId,
        request.command.commandId,
      );
      const preparedState = {
        ...provisionalState,
        attemptGeneration: launchProviderActionJournalRef.custodyAttemptGeneration,
      };
      const receipt: OperatorActionReceipt = {
        ...provisionalReceipt,
        afterStateDigest: digestValue(preparedState, "operatorLaunchCommit.preparedStateDigest"),
        launchProviderActionJournalRef,
      };
      this.#database.prepare(`
        UPDATE operator_previews SET preview_json=? WHERE preview_id=?
      `).run(
        canonicalJson({ preview: envelope.preview, action: { ...preparedState, receipt } }),
        request.previewId,
      );
      this.#database.prepare(`
        UPDATE operator_commands SET after_json=?, result_json=?
         WHERE operator_id=? AND command_id=?
      `).run(
        canonicalJson(preparedState),
        canonicalJson(receipt),
        context.operatorId,
        request.command.commandId,
      );
      return { kind: "prepared", receipt, handle };
    });
    const prepared = prepare.immediate();
    if (prepared.kind === "replay") return prepared.receipt;
    await launchCustody.dispatchPrepared(prepared.handle);
    return prepared.receipt;
  }

  async #commitChairRecovery(
    context: AuthenticatedOperatorContext,
    request: OperatorActionCommitRequest,
    envelope: StoredPreviewEnvelope,
    intent: Extract<OperatorActionIntent, { kind: "chair-bridge-recovery" }>,
    authenticated: AuthenticatedOperatorCredential,
    payloadHash: string,
    current: OperatorActionCurrentState,
  ): Promise<OperatorActionReceipt> {
    if (this.#chairRecoveryAdapter === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair recovery custody runtime is unavailable");
    }
    return this.#chairRecoveryAdapter.commit(context, request, envelope, intent, authenticated, payloadHash, current);
  }

  async #commitChairLiveHandoff(
    context: AuthenticatedOperatorContext,
    request: OperatorActionCommitRequest,
    envelope: StoredPreviewEnvelope,
    intent: Extract<OperatorActionIntent, { kind: "chair-live-handoff" }>,
    authenticated: AuthenticatedOperatorCredential,
    payloadHash: string,
    current: OperatorActionCurrentState,
  ): Promise<OperatorActionReceipt> {
    if (this.#chairLiveHandoffAdapter === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair live handoff custody runtime is unavailable");
    }
    return this.#chairLiveHandoffAdapter.commit(context, request, envelope, intent, authenticated, payloadHash, current);
  }

  async #commitLifecycleRecovery(
    context: AuthenticatedOperatorContext,
    request: OperatorActionCommitRequest,
    envelope: StoredPreviewEnvelope,
    intent: LifecycleRecoveryIntent,
    authenticated: AuthenticatedOperatorCredential,
    payloadHash: string,
    current: OperatorActionCurrentState,
  ): Promise<OperatorActionReceipt> {
    if (this.#lifecycleAdapter === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "lifecycle recovery custody runtime is unavailable");
    }
    return this.#lifecycleAdapter.commit(context, request, envelope, intent, authenticated, payloadHash, current);
  }

  status(request: OperatorActionStatusRequest): OperatorActionStatus {
    const authenticated = this.#operatorStore.authenticateCredential(request.credential.token);
    if (
      authenticated.capabilityId !== request.credential.capabilityId ||
      authenticated.context.projectId !== request.projectId
    ) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator status credential is not authorised");
    }
    const command = this.#database.prepare(`
      SELECT status, result_json, operation FROM operator_commands
       WHERE operator_id=? AND command_id=? AND project_id=?
         AND (? IS NULL OR project_session_id=?)
    `).get(
      authenticated.context.operatorId,
      request.commandId,
      request.projectId,
      authenticated.projectSessionId ?? null,
      authenticated.projectSessionId ?? null,
    );
    if (!isRow(command)) return { status: "not-found", commandId: request.commandId };
    if (
      !authenticated.actions.includes("read") &&
      !authenticated.actions.includes(text(command, "operation") as OperatorAction)
    ) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator status credential is not authorised");
    }
    if (text(command, "status") === "rejected") {
      return parseRejectedStatus(text(command, "result_json"), request.commandId);
    }
    const previewValue = this.#database.prepare(`
      SELECT preview_json FROM operator_previews WHERE confirmed_command_id=? AND operator_id=?
    `).get(request.commandId, authenticated.context.operatorId);
    if (!isRow(previewValue)) {
      if (text(command, "operation") === "git-custody-resolve") {
        return parseOperationResult(
          FABRIC_OPERATIONS.operatorActionReconcile,
          JSON.parse(text(command, "result_json")),
        );
      }
      return {
        status: "committed",
        commandId: request.commandId,
        receipt: nonLaunchReceipt(parseReceipt(text(command, "result_json"))),
      };
    }
    const envelope = parseStoredPreview(text(previewValue, "preview_json"));
    if (envelope.action === null) throw new Error("confirmed operator preview has no action state");
    if (envelope.preview.intent.kind === "project-session-launch") {
      return this.#launchStatus(
        authenticated.context.operatorId,
        request.commandId,
        envelope,
      );
    }
    if (envelope.preview.intent.kind === "chair-bridge-recovery") {
      return this.#chairRecoveryStatus(
        authenticated.context.operatorId,
        request.commandId,
        envelope,
      );
    }
    if (envelope.preview.intent.kind === "chair-live-handoff") {
      return this.#chairLiveHandoffStatus(
        authenticated.context.operatorId,
        request.commandId,
        envelope,
      );
    }
    if (envelope.preview.intent.kind === "agent-lifecycle-recovery") {
      return this.#lifecycleRecoveryStatus(
        authenticated.context.operatorId,
        request.commandId,
        envelope,
      );
    }
    if (envelope.preview.intent.kind === "git") {
      const gitStatus = this.#effectPort.status?.(request.commandId, envelope.preview.intentDigest);
      if (gitStatus !== undefined && gitStatus !== null) return gitStatus;
    }
    return statusFromAction(envelope.action, envelope.preview.intentDigest);
  }

  #chairRecoveryStatus(
    operatorId: string,
    commandId: string,
    envelope: StoredPreviewEnvelope,
  ): OperatorActionStatus {
    if (this.#chairRecoveryAdapter === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair recovery custody runtime is unavailable");
    }
    return this.#chairRecoveryAdapter.status(operatorId, commandId, envelope);
  }

  #chairLiveHandoffStatus(
    operatorId: string,
    commandId: string,
    envelope: StoredPreviewEnvelope,
  ): OperatorActionStatus {
    if (this.#chairLiveHandoffAdapter === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair live handoff custody runtime is unavailable");
    }
    return this.#chairLiveHandoffAdapter.status(operatorId, commandId, envelope);
  }

  #lifecycleRecoveryStatus(
    operatorId: string,
    commandId: string,
    envelope: StoredPreviewEnvelope,
  ): OperatorActionStatus {
    if (this.#lifecycleAdapter === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "lifecycle recovery custody runtime is unavailable");
    }
    return this.#lifecycleAdapter.status(operatorId, commandId, envelope);
  }

  #launchStatus(
    operatorId: string,
    commandId: string,
    envelope: StoredPreviewEnvelope,
  ): OperatorActionStatus {
    const launchCustody = this.#launchCustody;
    if (launchCustody === undefined || envelope.action === null) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "launch custody runtime is unavailable");
    }
    const launchProviderActionJournalRef = launchCustody.launchProviderActionJournalRefForCommand(
      operatorId,
      commandId,
    );
    const receipt = parseStoredReceipt(envelope.action);
    if (receipt.launchProviderActionJournalRef === undefined) {
      throw new Error("stored launch action has no launch receipt");
    }
    if (launchProviderActionJournalRef.journalState === "terminal") {
      if (launchProviderActionJournalRef.outcomeKind === "terminal-success") {
        return {
          status: "committed",
          commandId,
          receipt,
          launchProviderActionJournalRef,
          seatProvisioning: launchCustody.seatProvisioningDescriptorForCommand(operatorId, commandId),
        };
      }
      return {
        status: "committed",
        commandId,
        receipt,
        launchProviderActionJournalRef,
      };
    }
    if (launchProviderActionJournalRef.journalState === "ambiguous") {
      return {
        status: "ambiguous",
        commandId,
        intentDigest: envelope.preview.intentDigest,
        attemptGeneration: launchProviderActionJournalRef.custodyAttemptGeneration,
        launchProviderActionJournalRef,
      };
    }
    return {
      status: "pending",
      commandId,
      intentDigest: envelope.preview.intentDigest,
      phase: launchProviderActionJournalRef.journalState,
      attemptGeneration: launchProviderActionJournalRef.custodyAttemptGeneration,
      launchProviderActionJournalRef,
    };
  }

  async reconcile(
    context: AuthenticatedOperatorContext,
    request: OperatorActionReconcileRequest,
  ): Promise<OperatorActionStatus> {
    if (request.mode !== "observe-only") {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator action reconciliation is observe-only");
    }
    const stored = row(this.#database.prepare(`
      SELECT * FROM operator_previews WHERE confirmed_command_id=? AND operator_id=?
    `).get(request.targetCommandId, context.operatorId), "operator action target");
    const envelope = parseStoredPreview(text(stored, "preview_json"));
    if (envelope.preview.intent.kind === "project-session-launch") {
      throw new ProjectFabricCoreError(
        "CAPABILITY_FORBIDDEN",
        "public operator reconciliation cannot own a launch provider action",
      );
    }
    if (envelope.preview.intent.kind === "chair-bridge-recovery") {
      return await this.#reconcileChairRecovery(context, request, stored, envelope);
    }
    if (envelope.preview.intent.kind === "chair-live-handoff") {
      return await this.#reconcileChairLiveHandoff(context, request, stored, envelope);
    }
    if (envelope.preview.intent.kind === "agent-lifecycle-recovery") {
      return await this.#reconcileLifecycleRecovery(context, request, stored, envelope);
    }
    if (request.gitConflict !== undefined) {
      return await this.#reconcileGitConflict(context, request, stored, envelope);
    }
    const authenticated = this.#authenticateCommand(context, request.command, request.projectId, envelope.preview.intent);
    this.#journal.assertStoredPreviewScope(stored, authenticated, request.projectId, envelope.preview.intent, actionSessionId);
    const payloadHash = sha256(canonicalJson(sanitisedReconcileRequest(request)));
    const replay = this.#journal.reconcileReplay(context.operatorId, request.command.commandId, payloadHash);
    if (replay !== null) return replay;
    if (request.command.commandId === request.targetCommandId) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "reconciliation requires a distinct command ID");
    }
    const action = envelope.action;
    if (action === null || action.status === "terminal" || action.status === "rejected") {
      throw new ProjectFabricCoreError("CONFLICT", "operator action is not reconcilable");
    }
    if (action.status !== request.expectedStatus || action.attemptGeneration !== request.expectedAttemptGeneration) {
      throw new ProjectFabricCoreError("STALE_REVISION", "operator action reconciliation target changed");
    }
    const targetCommand = row(this.#database.prepare(`
      SELECT expected_revision FROM operator_commands WHERE operator_id=? AND command_id=?
    `).get(context.operatorId, request.targetCommandId), "operator action command");
    if (request.command.expectedRevision !== integer(targetCommand, "expected_revision")) {
      throw new ProjectFabricCoreError("STALE_REVISION", "reconciliation target revision changed");
    }
    const nextGeneration = action.attemptGeneration + 1;
    const effectRef = action.status === "ambiguous" ? action.effectRef : null;
    const observing: StoredPendingAction = {
      status: "pending",
      commandId: request.targetCommandId,
      phase: "observing",
      attemptGeneration: nextGeneration,
      receipt: action.receipt,
    };
    const observingStatus = statusFromAction(observing, envelope.preview.intentDigest);
    const prepare = this.#database.transaction((): OperatorActionStatus | null => {
      const concurrentReplay = this.#journal.reconcileReplay(
        context.operatorId,
        request.command.commandId,
        payloadHash,
      );
      if (concurrentReplay !== null) return concurrentReplay;
      const latestEnvelope = parseStoredPreview(text(this.#journal.previewRow(envelope.preview.previewId), "preview_json"));
      const latestAction = latestEnvelope.action;
      if (
        latestAction === null ||
        latestAction.status !== request.expectedStatus ||
        latestAction.attemptGeneration !== request.expectedAttemptGeneration
      ) {
        throw new ProjectFabricCoreError("STALE_REVISION", "operator action reconciliation target changed");
      }
      this.#journal.updateStoredAction(envelope.preview.previewId, envelope.preview, observing);
      this.#journal.insertCommand(
        context,
        request.command,
        request.projectId,
        actionSessionId(authenticated, envelope.preview.intent),
        requiredOperatorActionForIntent(envelope.preview.intent),
        payloadHash,
        action,
        observing,
        observingStatus,
        "committed",
      );
      return null;
    });
    const concurrentReplay = prepare();
    if (concurrentReplay !== null) return concurrentReplay;

    let outcome: OperatorEffectOutcome;
    try {
      outcome = await this.#effectPort.observe({
        commandId: request.targetCommandId,
        previewId: envelope.preview.previewId,
        operatorId: context.operatorId,
        projectId: request.projectId,
        projectSessionId: actionSessionId(authenticated, envelope.preview.intent),
        principalGeneration: context.principalGeneration,
        operation: requiredOperatorActionForIntent(envelope.preview.intent),
        intent: envelope.preview.intent,
        intentDigest: envelope.preview.intentDigest,
        beforeStateDigest: envelope.preview.beforeStateDigest,
        attemptGeneration: nextGeneration,
        effectRef,
      });
    } catch {
      return observingStatus;
    }
    try {
      this.#journal.applyEffectOutcome(
        context.operatorId,
        request.targetCommandId,
        envelope.preview,
        action.receipt,
        outcome,
        nextGeneration,
      );
    } catch (error: unknown) {
      const rejected = this.status({
        credential: request.command.credential,
        projectId: request.projectId,
        commandId: request.targetCommandId,
      });
      this.#journal.updateReconcileCommand(context.operatorId, request.command.commandId, rejected);
      throw error;
    }
    const result = this.status({
      credential: request.command.credential,
      projectId: request.projectId,
      commandId: request.targetCommandId,
    });
    this.#journal.updateReconcileCommand(context.operatorId, request.command.commandId, result);
    return result;
  }

  async #reconcileGitConflict(
    context: AuthenticatedOperatorContext,
    request: OperatorActionReconcileRequest,
    stored: Row,
    envelope: StoredPreviewEnvelope,
  ): Promise<OperatorActionStatus> {
    if (envelope.preview.intent.kind !== "git" || request.gitConflict === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "Git conflict reconciliation requires typed Git custody");
    }
    const reconcileGit = this.#effectPort.reconcileGit;
    const status = this.#effectPort.status?.(request.targetCommandId, envelope.preview.intentDigest);
    if (reconcileGit === undefined || status === undefined || status === null) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git custody reconciliation is unavailable");
    }
    const authenticated = this.#authenticateCommand(
      context,
      request.command,
      request.projectId,
      envelope.preview.intent,
      "git-custody-resolve",
    );
    this.#journal.assertStoredPreviewScope(stored, authenticated, request.projectId, envelope.preview.intent, actionSessionId);
    const payloadHash = sha256(canonicalJson(sanitisedReconcileRequest(request)));
    const replay = this.#journal.reconcileReplay(context.operatorId, request.command.commandId, payloadHash);
    if (replay !== null) return replay;
    if (
      status.status !== request.expectedStatus ||
      !("attemptGeneration" in status) ||
      status.attemptGeneration !== request.expectedAttemptGeneration
    ) throw new ProjectFabricCoreError("STALE_REVISION", "typed Git reconciliation target changed");
    if (request.command.commandId === request.targetCommandId) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "reconciliation requires a distinct command ID");
    }
    const targetCommand = row(this.#database.prepare(`
      SELECT expected_revision FROM operator_commands WHERE operator_id=? AND command_id=?
    `).get(context.operatorId, request.targetCommandId), "typed Git target command");
    if (request.command.expectedRevision !== integer(targetCommand, "expected_revision")) {
      throw new ProjectFabricCoreError("STALE_REVISION", "typed Git reconciliation target revision changed");
    }
    const action = envelope.action;
    if (action === null || action.status === "terminal" || action.status === "rejected") {
      throw new ProjectFabricCoreError("CONFLICT", "typed Git target is not reconcilable");
    }
    const nextAttemptGeneration = status.attemptGeneration + 1;
    const observing: StoredPendingAction = {
      status: "pending",
      commandId: request.targetCommandId,
      phase: "observing",
      attemptGeneration: nextAttemptGeneration,
      receipt: action.receipt,
    };
    const observingStatus = statusFromAction(observing, envelope.preview.intentDigest);
    this.#database.transaction(() => {
      const concurrentReplay = this.#journal.reconcileReplay(context.operatorId, request.command.commandId, payloadHash);
      if (concurrentReplay !== null) return;
      const latest = this.#effectPort.status?.(request.targetCommandId, envelope.preview.intentDigest);
      if (
        latest === undefined || latest === null || latest.status !== request.expectedStatus ||
        !("attemptGeneration" in latest) || latest.attemptGeneration !== request.expectedAttemptGeneration
      ) throw new ProjectFabricCoreError("STALE_REVISION", "typed Git reconciliation target changed");
      this.#journal.updateStoredAction(envelope.preview.previewId, envelope.preview, observing);
      this.#journal.insertCommand(
        context,
        request.command,
        request.projectId,
        actionSessionId(authenticated, envelope.preview.intent),
        "git-custody-resolve",
        payloadHash,
        status,
        observing,
        observingStatus,
        "committed",
      );
    })();
    let result: OperatorActionStatus;
    try {
      result = await reconcileGit({
        request,
        targetIntent: envelope.preview.intent,
        intentDigest: envelope.preview.intentDigest,
        nextAttemptGeneration,
      });
    } catch (error: unknown) {
      if (error instanceof ProjectFabricCoreError) {
        const rejected: OperatorActionStatus = {
          status: "rejected",
          commandId: request.command.commandId,
          intentDigest: envelope.preview.intentDigest,
          code: error.code === "STALE_GENERATION" ? "generation-stale" : "state-changed",
          evidenceRefs: envelope.preview.evidenceRefs,
        };
        this.#journal.updateReconcileCommand(context.operatorId, request.command.commandId, rejected);
        return rejected;
      }
      return observingStatus;
    }
    this.#journal.updateReconcileCommand(context.operatorId, request.command.commandId, result);
    return result;
  }

  async #reconcileLifecycleRecovery(
    context: AuthenticatedOperatorContext,
    request: OperatorActionReconcileRequest,
    stored: Row,
    envelope: StoredPreviewEnvelope,
  ): Promise<OperatorActionStatus> {
    if (
      this.#lifecycleAdapter === undefined ||
      envelope.action === null ||
      envelope.preview.intent.kind !== "agent-lifecycle-recovery"
    ) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "lifecycle recovery custody runtime is unavailable");
    }
    const authenticated = this.#authenticateCommand(
      context,
      request.command,
      request.projectId,
      envelope.preview.intent,
    );
    const payloadHash = sha256(canonicalJson(sanitisedReconcileRequest(request)));
    return this.#lifecycleAdapter.reconcile(context, request, stored, envelope, authenticated, payloadHash);
  }

  async #reconcileChairRecovery(
    context: AuthenticatedOperatorContext,
    request: OperatorActionReconcileRequest,
    stored: Row,
    envelope: StoredPreviewEnvelope,
  ): Promise<OperatorActionStatus> {
    if (this.#chairRecoveryAdapter === undefined || envelope.action === null) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair recovery custody runtime is unavailable");
    }
    const authenticated = this.#authenticateCommand(context, request.command, request.projectId, envelope.preview.intent);
    const payloadHash = sha256(canonicalJson(sanitisedReconcileRequest(request)));
    return this.#chairRecoveryAdapter.reconcile(context, request, stored, envelope, authenticated, payloadHash);
  }

  async #reconcileChairLiveHandoff(
    context: AuthenticatedOperatorContext,
    request: OperatorActionReconcileRequest,
    stored: Row,
    envelope: StoredPreviewEnvelope,
  ): Promise<OperatorActionStatus> {
    if (
      this.#chairLiveHandoffAdapter === undefined ||
      envelope.action === null ||
      envelope.preview.intent.kind !== "chair-live-handoff"
    ) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair live handoff custody runtime is unavailable");
    }
    const authenticated = this.#authenticateCommand(
      context,
      request.command,
      request.projectId,
      envelope.preview.intent,
    );
    const payloadHash = sha256(canonicalJson(sanitisedReconcileRequest(request)));
    return this.#chairLiveHandoffAdapter.reconcile(context, request, stored, envelope, authenticated, payloadHash);
  }

  async #readCurrentState(intent: OperatorActionIntent): Promise<OperatorActionCurrentState> {
    if (intent.kind === "agent-lifecycle-recovery") {
      if (this.#lifecycleAdapter === undefined) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "lifecycle recovery custody runtime is unavailable");
      }
      return await this.#lifecycleAdapter.readCurrentState(intent);
    }
    if (intent.kind === "chair-bridge-recovery") {
      if (this.#chairRecoveryAdapter === undefined) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair recovery custody runtime is unavailable");
      }
      return await this.#chairRecoveryAdapter.readCurrentState(intent);
    }
    if (intent.kind === "chair-live-handoff") {
      if (this.#chairLiveHandoffAdapter === undefined) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair live handoff custody runtime is unavailable");
      }
      return await this.#chairLiveHandoffAdapter.readCurrentState(intent);
    }
    if (intent.kind !== "project-session-launch") return await this.#statePort.read(intent);
    if (this.#launchCustody === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "launch custody runtime is unavailable");
    }
    const state = await this.#launchCustody.readCurrentState(intent);
    return { kind: "project-session-launch", revision: state.sessionRevision, state };
  }

  #authenticateCommand(
    context: AuthenticatedOperatorContext,
    command: OperatorActionPreviewRequest["command"],
    projectId: OperatorActionPreviewRequest["projectId"],
    intent: OperatorActionIntent,
    requiredActionOverride?: "git-custody-resolve",
  ): AuthenticatedOperatorCredential {
    const authenticated = this.#operatorStore.authenticateCredential(command.credential.token);
    if (
      authenticated.capabilityId !== command.credential.capabilityId ||
      authenticated.context.operatorId !== context.operatorId ||
      authenticated.context.projectId !== context.projectId ||
      authenticated.context.projectId !== projectId ||
      authenticated.context.projectAuthorityGeneration !== context.projectAuthorityGeneration ||
      authenticated.context.principalGeneration !== context.principalGeneration ||
      command.actor !== context.operatorId
    ) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "operator command does not match its authenticated context");
    }
    const requiredAction = requiredActionOverride ?? requiredOperatorActionForIntent(intent);
    if (!authenticated.actions.includes(requiredAction)) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", `operator capability lacks ${requiredAction}`);
    }
    const intentSessionId = sessionIdForIntent(intent);
    if (intent.kind === "project-session-launch") {
      if (
        authenticated.kind !== "session" ||
        authenticated.projectSessionId !== intent.projectSessionId ||
        intent.projectId !== projectId
      ) {
        throw new ProjectFabricCoreError(
          "CAPABILITY_FORBIDDEN",
          "project-session launch requires exact session-bound launch authority",
        );
      }
      if (!isRow(this.#database.prepare(`
        SELECT project_session_id FROM project_sessions WHERE project_session_id=? AND project_id=?
      `).get(intent.projectSessionId, projectId))) {
        throw new ProjectFabricCoreError("WRONG_PROJECT", "project launch session belongs to another project");
      }
    } else if (intent.kind === "agent-lifecycle-recovery") {
      if (
        authenticated.kind !== "session" ||
        authenticated.projectSessionId !== intent.projectSessionId ||
        authenticated.sessionGeneration !== intent.expectedSessionGeneration
      ) {
        throw new ProjectFabricCoreError(
          "CAPABILITY_FORBIDDEN",
          "lifecycle recovery requires exact session-bound operator authority",
        );
      }
    } else if (intent.kind === "chair-bridge-recovery") {
      const binding = row(this.#database.prepare(`
        SELECT kind, handoff_digest, old_chair_generation, expected_run_id,
               expected_run_revision, expected_session_revision, cas_target_revision
          FROM operator_capabilities WHERE capability_id=?
      `).get(authenticated.capabilityId), "chair recovery operator capability");
      if (
        authenticated.kind !== "takeover" ||
        authenticated.projectSessionId !== intent.projectSessionId ||
        authenticated.sessionGeneration !== intent.expectedSessionGeneration ||
        text(binding, "kind") !== "takeover" ||
        text(binding, "handoff_digest") !== intent.recoveryManifestDigest ||
        integer(binding, "old_chair_generation") !== intent.expectedChairGeneration ||
        text(binding, "expected_run_id") !== intent.coordinationRunId ||
        integer(binding, "expected_run_revision") !== intent.expectedRunRevision ||
        integer(binding, "expected_session_revision") !== intent.expectedSessionRevision ||
        integer(binding, "cas_target_revision") !== intent.expectedBridgeRevision
      ) {
        throw new ProjectFabricCoreError(
          "CAPABILITY_FORBIDDEN",
          "chair recovery requires exact loss-manifest takeover authority",
        );
      }
    } else if (intent.kind === "chair-live-handoff") {
      const binding = row(this.#database.prepare(`
        SELECT kind, handoff_digest, old_chair_generation, expected_run_id,
               expected_run_revision, expected_session_revision, cas_target_revision
          FROM operator_capabilities WHERE capability_id=?
      `).get(authenticated.capabilityId), "chair live handoff operator capability");
      if (
        authenticated.kind !== "takeover" ||
        authenticated.projectSessionId !== intent.projectSessionId ||
        authenticated.sessionGeneration !== intent.expectedSessionGeneration ||
        text(binding, "kind") !== "takeover" ||
        text(binding, "handoff_digest") !== intent.handoffRef.digest ||
        integer(binding, "old_chair_generation") !== intent.expectedChairGeneration ||
        text(binding, "expected_run_id") !== intent.coordinationRunId ||
        integer(binding, "expected_run_revision") !== intent.expectedRunRevision ||
        integer(binding, "expected_session_revision") !== intent.expectedSessionRevision ||
        integer(binding, "cas_target_revision") !== intent.expectedBridgeRevision
      ) {
        throw new ProjectFabricCoreError(
          "CAPABILITY_FORBIDDEN",
          "chair live handoff requires exact handoff-bound takeover authority",
        );
      }
    } else if (intentSessionId !== undefined && authenticated.projectSessionId !== intentSessionId) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator capability is bound to another session");
    }
    return authenticated;
  }

}

function validateCurrentState(
  intent: OperatorActionIntent,
  commandExpectedRevision: number,
  current: OperatorActionCurrentState,
): void {
  if (current.revision !== commandExpectedRevision) {
    throw new ProjectFabricCoreError("STALE_REVISION", "operator command revision changed", {
      expected: commandExpectedRevision,
      actual: current.revision,
    });
  }
  if (intent.kind === "project-session-launch") {
    if (current.kind !== "project-session-launch") {
      throw new TypeError("project-session launch intent received another current-state family");
    }
    const launch = current.state;
    if (
      launch.projectId !== intent.projectId ||
      launch.projectRevision !== intent.expectedProjectRevision ||
      launch.projectSessionId !== intent.projectSessionId ||
      launch.sessionRevision !== intent.expectedSessionRevision ||
      launch.sessionRevision !== current.revision ||
      launch.sessionGeneration !== intent.expectedSessionGeneration ||
      launch.trustRecordDigest !== intent.trustRecordDigest ||
      launch.providerAdapterId !== intent.providerAdapterId ||
      launch.providerContractDigest !== intent.providerContractDigest ||
      launch.resourceStateDigest !== intent.resourceStateDigest
    ) {
      throw new ProjectFabricCoreError("STALE_REVISION", "project-session launch custody binding changed");
    }
    if (launch.sessionState === "awaiting_launch") {
      if (intent.retryOf !== undefined || launch.provedFailedAttempt !== null || !sameArtifact(intent.launchPacketRef, launch.currentLaunchPacketRef)) {
        throw new ProjectFabricCoreError("STALE_REVISION", "initial launch packet or retry binding changed");
      }
      return;
    }
    if (
      intent.retryOf === undefined ||
      launch.provedFailedAttempt === null ||
      intent.retryOf.providerAdapterId !== launch.provedFailedAttempt.providerAdapterId ||
      intent.retryOf.providerActionId !== launch.provedFailedAttempt.providerActionId
    ) {
      throw new ProjectFabricCoreError(
        "STALE_REVISION",
        "proved launch failure or retry identity changed",
      );
    }
    return;
  }
  if (intent.kind === "control") {
    if (current.kind !== "control") throw new TypeError("control intent received another current-state family");
    if (intent.target.expectedRevision !== current.revision) {
      throw new ProjectFabricCoreError("STALE_REVISION", "operator control target revision changed");
    }
    if (!current.eligibleActions.includes(intent.action)) {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "operator control action is not eligible now");
    }
    return;
  }
  if (intent.kind === "project-session-drain" || intent.kind === "project-session-stop") {
    if (current.kind !== "project-session-lifecycle") {
      throw new TypeError("project-session lifecycle intent received another current-state family");
    }
    if (
      intent.expectedSessionRevision !== current.revision ||
      intent.expectedSessionGeneration !== current.sessionGeneration ||
      intent.expectedGlobalStateRevision !== current.globalStateRevision
    ) {
      throw new ProjectFabricCoreError("STALE_GENERATION", "project-session lifecycle binding changed");
    }
    if (intent.kind === "project-session-stop" && !sameArtifact(intent.drainReceiptRef, current.drainReceiptRef)) {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "project-session stop drain receipt changed");
    }
    return;
  }
  if (intent.kind === "daemon-drain" || intent.kind === "daemon-stop") {
    if (current.kind !== "daemon-lifecycle") {
      throw new TypeError("daemon lifecycle intent received another current-state family");
    }
    if (
      intent.expectedDaemonGeneration !== current.daemonGeneration ||
      intent.expectedGlobalStateRevision !== current.globalStateRevision ||
      intent.expectedGlobalStateRevision !== current.revision
    ) {
      throw new ProjectFabricCoreError("STALE_GENERATION", "daemon lifecycle binding changed");
    }
    if (intent.kind === "daemon-stop" && !sameArtifact(intent.drainReceiptRef, current.drainReceiptRef)) {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "daemon stop drain receipt changed");
    }
    return;
  }
  if (intent.kind === "git") {
    if (current.kind !== "git") throw new TypeError("Git intent received another current-state family");
    try {
      assertGitIntentState(intent, current.state);
    } catch (error: unknown) {
      throw new ProjectFabricCoreError("STALE_REVISION", "operator Git state changed", {
        reason: error instanceof Error ? error.message : String(error),
      });
    }
    return;
  }
  if (
    intent.kind === "git-authorise" ||
    intent.kind === "git-operation-draft" ||
    intent.kind === "git-custody-resolve"
  ) {
    if (current.kind !== "git-administration") {
      throw new TypeError("Git administration intent received another current-state family");
    }
    return;
  }
  if (intent.kind === "registered-external-effect") {
    if (current.kind !== "registered-external-effect") {
      throw new TypeError("external-effect intent received another current-state family");
    }
    try {
      assertRegisteredExternalEffectContract(intent, current.state);
    } catch (error: unknown) {
      throw new ProjectFabricCoreError("STALE_REVISION", "registered external-effect state changed", {
        reason: error instanceof Error ? error.message : String(error),
      });
    }
    return;
  }
  if (intent.kind === "chair-bridge-recovery") {
    if (current.kind !== "chair-bridge-recovery") {
      throw new TypeError("chair bridge recovery intent received another current-state family");
    }
    if (intent.expectedBridgeRevision !== current.revision) {
      throw new ProjectFabricCoreError("STALE_REVISION", "chair bridge recovery target revision changed");
    }
    return;
  }
  if (intent.kind === "chair-live-handoff") {
    if (current.kind !== "chair-live-handoff") {
      throw new TypeError("chair live handoff intent received another current-state family");
    }
    if (intent.expectedBridgeRevision !== current.revision) {
      throw new ProjectFabricCoreError("STALE_REVISION", "chair live handoff bridge revision changed");
    }
    return;
  }
  if (intent.kind === "agent-lifecycle-recovery") {
    validateLifecycleRecoveryCurrentState(intent, current);
    return;
  }
  if (intent.kind === "provider-route-integrity-retire") {
    throw new ProjectFabricCoreError(
      "CAPABILITY_FORBIDDEN",
      `${intent.kind} current-state validation is unavailable until its daemon custody owner is composed`,
    );
  }
  if (current.kind !== "promotion") throw new TypeError("promotion intent received another current-state family");
  try {
    assertPromotionIntentGate(intent, current.gate);
  } catch (error: unknown) {
    throw new ProjectFabricCoreError("GATE_BLOCKED", "promotion release binding is not current", {
      reason: error instanceof Error ? error.message : String(error),
    });
  }
}

function sanitisedPreviewRequest(request: OperatorActionPreviewRequest): unknown {
  return {
    command: sanitisedCommand(request.command),
    projectId: request.projectId,
    intent: request.intent,
  };
}

function sanitisedCommitRequest(request: OperatorActionCommitRequest): unknown {
  return {
    command: sanitisedCommand(request.command),
    projectId: request.projectId,
    previewId: request.previewId,
    expectedPreviewRevision: request.expectedPreviewRevision,
    previewDigest: request.previewDigest,
    expectedIntentDigest: request.expectedIntentDigest,
    confirmation: request.confirmation,
  };
}

function sanitisedReconcileRequest(request: OperatorActionReconcileRequest): unknown {
  return {
    command: sanitisedCommand(request.command),
    projectId: request.projectId,
    targetCommandId: request.targetCommandId,
    expectedStatus: request.expectedStatus,
    expectedAttemptGeneration: request.expectedAttemptGeneration,
    mode: request.mode,
    ...(request.gitConflict === undefined ? {} : { gitConflict: request.gitConflict }),
  };
}

function sanitisedCommand(command: OperatorActionPreviewRequest["command"]): unknown {
  return {
    credential: { capabilityId: command.credential.capabilityId },
    commandId: command.commandId,
    expectedRevision: command.expectedRevision,
    actor: command.actor,
    provenance: command.provenance,
    evidenceRefs: command.evidenceRefs,
  };
}

function assertCommitMatchesPreview(
  request: OperatorActionCommitRequest,
  preview: OperatorActionPreview,
  storedRevision: number,
  nowMilliseconds: number,
): void {
  if (request.expectedPreviewRevision !== storedRevision || request.expectedPreviewRevision !== preview.previewRevision) {
    throw new ProjectFabricCoreError("STALE_REVISION", "operator action preview revision is stale");
  }
  if (request.previewDigest !== preview.previewDigest || request.expectedIntentDigest !== preview.intentDigest) {
    throw new ProjectFabricCoreError("STALE_REVISION", "operator action preview digest is stale");
  }
  if (Date.parse(preview.expiresAt) <= nowMilliseconds) {
    throw new ProjectFabricCoreError("CAPABILITY_EXPIRED", "operator action preview expired");
  }
  if (preview.confirmationMode === "explicit" && request.confirmation.kind !== "explicit") {
    throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator action requires explicit confirmation");
  }
  if (
    request.confirmation.kind === "echo" &&
    request.confirmation.echoedPreviewDigest !== preview.previewDigest
  ) {
    throw new ProjectFabricCoreError("STALE_REVISION", "operator action confirmation digest is stale");
  }
}

function consequenceClass(intent: OperatorActionIntent): OperatorActionPreview["consequenceClass"] {
  if (intent.kind === "promotion") return "promotion";
  if (intent.kind === "registered-external-effect") return "external";
  if (
    (intent.kind === "chair-bridge-recovery" && intent.path === "abandon") ||
    (intent.kind === "agent-lifecycle-recovery" && intent.path === "abandon") ||
    intent.kind === "project-session-stop" ||
    intent.kind === "daemon-stop" ||
    (intent.kind === "control" && intent.action === "cancel") ||
    intent.kind === "git-custody-resolve" ||
    (intent.kind === "git" && [
      "push-force-with-lease",
      "branch-delete-force",
      "worktree-remove-force",
      "merge-abort",
      "rebase-abort",
    ].includes(intent.operation.variant))
  ) return "destructive";
  if (intent.kind === "control" && (intent.action === "pause" || intent.action === "resume" || intent.action === "steer")) {
    return "routine";
  }
  return "consequential";
}

function combinedEvidence(request: OperatorActionPreviewRequest): ArtifactRef[] {
  const evidence = [...request.command.evidenceRefs];
  if (request.intent.kind === "project-session-launch") {
    evidence.push(request.intent.launchPacketRef, request.intent.resourcePlanRef);
  } else if (request.intent.kind === "chair-live-handoff") {
    evidence.push(request.intent.handoffRef);
  } else if (request.intent.kind === "control" && request.intent.action === "steer") {
    evidence.push(...request.intent.evidenceRefs);
  } else if (request.intent.kind === "registered-external-effect") {
    evidence.push(request.intent.requestArtifactRef);
  } else if (request.intent.kind === "promotion") {
    evidence.push(request.intent.releaseBinding.acceptedDeliveryReceiptRef);
  } else if (request.intent.kind === "project-session-stop" || request.intent.kind === "daemon-stop") {
    evidence.push(request.intent.drainReceiptRef);
  }
  const unique = new Map<string, ArtifactRef>();
  for (const item of evidence) unique.set(`${item.path}:${item.digest}`, item);
  return [...unique.values()];
}

function sessionIdForIntent(intent: OperatorActionIntent): string | undefined {
  if (intent.kind === "project-session-launch") return intent.projectSessionId;
  if (intent.kind === "agent-lifecycle-recovery") return intent.projectSessionId;
  if (intent.kind === "chair-bridge-recovery") return intent.projectSessionId;
  if (intent.kind === "chair-live-handoff") return intent.projectSessionId;
  if (intent.kind === "control") return intent.target.projectSessionId;
  if (intent.kind === "project-session-drain" || intent.kind === "project-session-stop" || intent.kind === "promotion") {
    return intent.projectSessionId;
  }
  if (intent.kind === "git") return intent.authorisation.projectSessionId;
  if (intent.kind === "git-authorise" || intent.kind === "git-custody-resolve") return intent.projectSessionId;
  if (intent.kind === "git-operation-draft") {
    return intent.action === "cancel"
      ? intent.projectSessionId
      : intent.binding.kind === "mutation"
        ? intent.binding.authorisation.projectSessionId
        : intent.binding.projectSessionId;
  }
  return undefined;
}

function actionSessionId(authenticated: AuthenticatedOperatorCredential, intent: OperatorActionIntent): string {
  if (intent.kind === "project-session-launch") return intent.projectSessionId;
  if (intent.kind === "agent-lifecycle-recovery") return intent.projectSessionId;
  if (intent.kind === "chair-bridge-recovery") return intent.projectSessionId;
  if (intent.kind === "chair-live-handoff") return intent.projectSessionId;
  return requiredSession(authenticated);
}

function requiredSession(authenticated: AuthenticatedOperatorCredential): string {
  if (authenticated.projectSessionId === undefined) {
    throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator action requires a session-bound capability");
  }
  return authenticated.projectSessionId;
}

function toTimestamp(milliseconds: number, path: string): Timestamp {
  return parseTimestamp(new Date(milliseconds).toISOString(), path);
}

function sameArtifact(left: ArtifactRef, right: ArtifactRef | null): boolean {
  return right !== null && left.path === right.path && left.digest === right.digest;
}

function rejectionForIntent(intent: OperatorActionIntent): OperatorActionRejectionCode {
  if (intent.kind === "git") return "git-state-changed";
  if (intent.kind === "registered-external-effect") return "external-contract-stale";
  if (intent.kind === "promotion") return "release-binding-mismatch";
  if (
    intent.kind === "project-session-launch" ||
    intent.kind === "agent-lifecycle-recovery" ||
    intent.kind === "chair-bridge-recovery" ||
    intent.kind === "chair-live-handoff" ||
    intent.kind === "project-session-drain" ||
    intent.kind === "project-session-stop" ||
    intent.kind === "daemon-drain" ||
    intent.kind === "daemon-stop"
  ) return "generation-stale";
  return "state-changed";
}
