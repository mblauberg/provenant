import type {
  OperatorActionCommitRequest,
  OperatorActionIntent,
  OperatorActionReceipt,
  OperatorActionReconcileRequest,
  OperatorActionStatus,
  Timestamp,
} from "@local/agent-fabric-protocol";
import { parseTimestamp } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError, type AuthenticatedOperatorContext } from "../project-session/contracts.js";
import { canonicalJson, type Row } from "../persistence/row-codec.js";
import type {
  ChairLiveHandoffCommit,
  ChairLiveHandoffCurrentState,
  ChairLiveHandoffDispatchHandle,
  ChairLiveHandoffInspection,
} from "../project-session/launch-custody.js";
import type { AuthenticatedOperatorCredential } from "./store.js";
import type { ProviderActionTicket } from "../application/provider-action-admission.js";
import {
  OperatorActionJournal,
  digestValue,
  nonLaunchReceipt,
  parseStoredReceipt,
  prepareProviderActionAfterPreflight,
  statusFromAction,
  type StoredPendingAction,
  type StoredPreviewEnvelope,
  type StoredTerminalAction,
} from "./action-journal.js";
import type { OperatorActionCurrentState } from "./action-store.js";

export interface OperatorChairLiveHandoffCustodyPort {
  readChairLiveHandoffCurrentState(intent: Extract<OperatorActionIntent, { kind: "chair-live-handoff" }>): Promise<ChairLiveHandoffCurrentState>;
  inspectChairLiveHandoff(intent: Extract<OperatorActionIntent, { kind: "chair-live-handoff" }>): Promise<ChairLiveHandoffInspection>;
  preflightChairLiveHandoff(input: Readonly<{
    inspection: ChairLiveHandoffInspection;
    operatorCommandId: string;
    principal: AuthenticatedOperatorContext;
  }>): ProviderActionTicket;
  prepareChairLiveHandoffInTransaction(input: Readonly<{
    inspection: ChairLiveHandoffInspection;
    operatorId: string;
    operatorCommandId: string;
    providerActionTicket: ProviderActionTicket;
  }>): ChairLiveHandoffDispatchHandle;
  dispatchPreparedChairLiveHandoff(handle: ChairLiveHandoffDispatchHandle): Promise<ChairLiveHandoffCommit>;
  chairLiveHandoffStatus(operatorId: string, operatorCommandId: string): ChairLiveHandoffCommit;
  reconcileChairLiveHandoff(operatorId: string, operatorCommandId: string): Promise<ChairLiveHandoffCommit>;
  releaseProviderActionPreflightAfterRollback?(ticket: ProviderActionTicket, failure: unknown): void;
}

/**
 * Byte-moved from `OperatorActionStore#commitChairLiveHandoff`, `#chairLiveHandoffStatus`, and
 * `#reconcileChairLiveHandoff` (S4c). Preserves: custody + provider action + predecessor/run/
 * dual-delivery fences inside the caller's transaction, `dispatched` persisted before promotion
 * I/O, both parties fenced on ambiguity, no-effect restored atomically, and the bridge/capability/
 * lease/membership/chair swap committing before volatile predecessor retirement — all of which
 * live in `ChairLiveHandoffCustodyAdapter` (`project-session/chair-live-handoff-custody.ts`), the
 * `custody` port this class delegates to. Auth (`#authenticateCommand`) stays in the facade, which
 * keeps ownership of authentication per the S4 plan.
 */
export class OperatorChairLiveHandoffActionAdapter {
  readonly #database: Database.Database;
  readonly #journal: OperatorActionJournal;
  readonly #custody: OperatorChairLiveHandoffCustodyPort;
  readonly #clock: () => number;
  readonly #actionSessionId: (authenticated: AuthenticatedOperatorCredential, intent: OperatorActionIntent) => string;

  constructor(options: Readonly<{
    database: Database.Database;
    journal: OperatorActionJournal;
    custody: OperatorChairLiveHandoffCustodyPort;
    clock: () => number;
    actionSessionId: (authenticated: AuthenticatedOperatorCredential, intent: OperatorActionIntent) => string;
  }>) {
    this.#database = options.database;
    this.#journal = options.journal;
    this.#custody = options.custody;
    this.#clock = options.clock;
    this.#actionSessionId = options.actionSessionId;
  }

  async readCurrentState(
    intent: Extract<OperatorActionIntent, { kind: "chair-live-handoff" }>,
  ): Promise<Extract<OperatorActionCurrentState, { kind: "chair-live-handoff" }>> {
    const state = await this.#custody.readChairLiveHandoffCurrentState(intent);
    return { kind: "chair-live-handoff", revision: state.revision, state };
  }

  async commit(
    context: AuthenticatedOperatorContext,
    request: OperatorActionCommitRequest,
    envelope: StoredPreviewEnvelope,
    intent: Extract<OperatorActionIntent, { kind: "chair-live-handoff" }>,
    authenticated: AuthenticatedOperatorCredential,
    payloadHash: string,
    current: OperatorActionCurrentState,
  ): Promise<OperatorActionReceipt> {
    const custody = this.#custody;
    const inspection = await custody.inspectChairLiveHandoff(intent);
    const providerActionTicket = custody.preflightChairLiveHandoff({
      inspection,
      operatorCommandId: request.command.commandId,
      principal: context,
    });
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
      afterStateDigest: digestValue(provisionalState, "operatorChairLiveHandoffCommit.provisionalStateDigest"),
      evidenceRefs: envelope.preview.evidenceRefs,
      committedAt: toTimestamp(this.#clock(), "operatorChairLiveHandoffCommit.committedAt"),
    };
    const prepare = this.#database.transaction(():
      | { kind: "replay"; receipt: OperatorActionReceipt }
      | { kind: "prepared"; handle: ChairLiveHandoffDispatchHandle } => {
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
        this.#actionSessionId(authenticated, intent),
        "takeover",
        payloadHash,
        current,
        provisionalState,
        provisionalReceipt,
        "committed",
      );
      return {
        kind: "prepared",
        handle: custody.prepareChairLiveHandoffInTransaction({
          inspection,
          operatorId: context.operatorId,
          operatorCommandId: request.command.commandId,
          providerActionTicket,
        }),
      };
    });
    const prepared = prepareProviderActionAfterPreflight(() => prepare.immediate(), custody, providerActionTicket);
    if (prepared.kind === "replay") return prepared.receipt;
    let outcome: ChairLiveHandoffCommit;
    try {
      outcome = await custody.dispatchPreparedChairLiveHandoff(prepared.handle);
    } catch {
      return provisionalReceipt;
    }
    if (outcome.status !== "committed") return provisionalReceipt;
    const receipt: OperatorActionReceipt = {
      ...provisionalReceipt,
      afterStateDigest: digestValue(outcome, "operatorChairLiveHandoffCommit.afterStateDigest"),
    };
    const terminal: StoredTerminalAction = {
      status: "terminal",
      commandId: request.command.commandId,
      receipt,
    };
    this.#database.transaction(() => {
      this.#journal.updateStoredAction(envelope.preview.previewId, envelope.preview, terminal);
      this.#database.prepare(`
        UPDATE operator_commands SET result_json=?, after_json=?
         WHERE operator_id=? AND command_id=? AND status='committed'
      `).run(
        canonicalJson(receipt),
        canonicalJson(outcome),
        context.operatorId,
        request.command.commandId,
      );
    })();
    return receipt;
  }

  status(
    operatorId: string,
    commandId: string,
    envelope: StoredPreviewEnvelope,
  ): OperatorActionStatus {
    const custody = this.#custody;
    if (envelope.action === null) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair live handoff custody runtime is unavailable");
    }
    const observed = custody.chairLiveHandoffStatus(operatorId, commandId);
    if (observed.status === "committed") {
      const receipt: OperatorActionReceipt = {
        ...parseStoredReceipt(envelope.action),
        afterStateDigest: digestValue(observed, "operatorChairLiveHandoffStatus.afterStateDigest"),
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
    const attemptGeneration = envelope.action.status === "pending" || envelope.action.status === "ambiguous"
      ? envelope.action.attemptGeneration
      : 1;
    if (observed.status === "ambiguous") {
      if (envelope.preview.intent.kind !== "chair-live-handoff") throw new Error("chair live handoff preview changed");
      return {
        status: "ambiguous",
        commandId,
        intentDigest: envelope.preview.intentDigest,
        attemptGeneration,
        effectRef: envelope.preview.intent.handoffRef,
      };
    }
    return {
      status: "pending",
      commandId,
      intentDigest: envelope.preview.intentDigest,
      phase: "observing",
      attemptGeneration,
    };
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
    if (envelope.action === null || envelope.preview.intent.kind !== "chair-live-handoff") {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair live handoff custody runtime is unavailable");
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
    ) throw new ProjectFabricCoreError("STALE_REVISION", "chair live handoff reconciliation target changed");
    const observed = await custody.reconcileChairLiveHandoff(context.operatorId, request.targetCommandId);
    const baseReceipt = parseStoredReceipt(envelope.action);
    let result: OperatorActionStatus;
    if (observed.status === "committed") {
      const receipt: OperatorActionReceipt = {
        ...baseReceipt,
        afterStateDigest: digestValue(observed, "operatorChairLiveHandoffReconcile.afterStateDigest"),
      };
      this.#journal.updateStoredAction(envelope.preview.previewId, envelope.preview, {
        status: "terminal",
        commandId: request.targetCommandId,
        receipt,
      });
      this.#database.prepare(`
        UPDATE operator_commands SET result_json=?, after_json=?
         WHERE operator_id=? AND command_id=?
      `).run(canonicalJson(receipt), canonicalJson(observed), context.operatorId, request.targetCommandId);
      result = {
        status: "committed",
        commandId: request.targetCommandId,
        receipt: nonLaunchReceipt(receipt),
      };
    } else if (observed.status === "no-effect") {
      result = {
        status: "rejected",
        commandId: request.targetCommandId,
        intentDigest: envelope.preview.intentDigest,
        code: "state-changed",
        evidenceRefs: [],
      };
    } else {
      const pending: StoredPendingAction = {
        status: "pending",
        commandId: request.targetCommandId,
        phase: "observing",
        attemptGeneration: request.expectedAttemptGeneration + 1,
        receipt: baseReceipt,
      };
      this.#journal.updateStoredAction(envelope.preview.previewId, envelope.preview, pending);
      result = statusFromAction(pending, envelope.preview.intentDigest);
    }
    this.#database.transaction(() => {
      this.#journal.insertCommand(
        context,
        request.command,
        request.projectId,
        this.#actionSessionId(authenticated, envelope.preview.intent),
        "takeover",
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

function toTimestamp(milliseconds: number, path: string): Timestamp {
  return parseTimestamp(new Date(milliseconds).toISOString(), path);
}
