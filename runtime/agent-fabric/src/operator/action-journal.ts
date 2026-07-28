import type {
  ArtifactRef,
  OperatorActionCommitRequest,
  OperatorActionIntent,
  OperatorActionPreview,
  OperatorActionReceipt,
  OperatorActionRejectionCode,
  OperatorActionStatus,
  Sha256Digest,
} from "@local/agent-fabric-protocol";
import {
  FABRIC_OPERATIONS,
  parseArtifactRef,
  parseOperationResult,
  parseSha256Digest,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError, type AuthenticatedOperatorContext } from "../project-session/contracts.js";
import { canonicalJson, integer, isRow, nullableText, row, sha256, text, type Row } from "../persistence/row-codec.js";
import type { AuthenticatedOperatorCredential } from "./store.js";
import type { ProviderActionTicket } from "../application/provider-action-admission.js";

export type StoredPendingAction = {
  status: "pending";
  commandId: string;
  phase: "prepared" | "dispatched" | "accepted" | "observing";
  attemptGeneration: number;
  receipt: OperatorActionReceipt;
};

export type StoredAmbiguousAction = {
  status: "ambiguous";
  commandId: string;
  attemptGeneration: number;
  effectRef: ArtifactRef;
  receipt: OperatorActionReceipt;
};

export type StoredTerminalAction = {
  status: "terminal";
  commandId: string;
  receipt: OperatorActionReceipt;
};

export type StoredRejectedAction = {
  status: "rejected";
  commandId: string;
  code: OperatorActionRejectionCode;
  evidenceRefs: readonly ArtifactRef[];
};

export type StoredAction = StoredPendingAction | StoredAmbiguousAction | StoredTerminalAction | StoredRejectedAction;
export type StoredPreviewEnvelope = { preview: OperatorActionPreview; action: StoredAction | null };

export function parseStoredPreview(serialized: string): StoredPreviewEnvelope {
  const value: unknown = JSON.parse(serialized);
  const envelope = row(value, "stored operator preview");
  const preview = parseOperationResult(FABRIC_OPERATIONS.operatorActionPreview, envelope.preview);
  return { preview, action: envelope.action === null ? null : parseStoredAction(envelope.action) };
}

export function parseStoredAction(value: unknown): StoredAction {
  const action = row(value, "stored operator action");
  const status = text(action, "status");
  const commandId = text(action, "commandId");
  if (status === "terminal") return { status, commandId, receipt: parseStoredReceipt(action) };
  if (status === "pending") {
    const phase = text(action, "phase");
    if (phase !== "prepared" && phase !== "dispatched" && phase !== "accepted" && phase !== "observing") {
      throw new Error("stored operator action phase is invalid");
    }
    return {
      status,
      commandId,
      phase,
      attemptGeneration: integer(action, "attemptGeneration"),
      receipt: parseStoredReceipt(action),
    };
  }
  if (status === "ambiguous") {
    return {
      status,
      commandId,
      attemptGeneration: integer(action, "attemptGeneration"),
      effectRef: parseArtifactRef(action.effectRef, "storedOperatorAction.effectRef"),
      receipt: parseStoredReceipt(action),
    };
  }
  if (status === "rejected") {
    const code = rejectionCode(action.code);
    const evidence = action.evidenceRefs;
    if (!Array.isArray(evidence)) throw new Error("stored rejected action evidence is invalid");
    return {
      status,
      commandId,
      code,
      evidenceRefs: evidence.map((item, index) => parseArtifactRef(item, `storedOperatorAction.evidenceRefs[${String(index)}]`)),
    };
  }
  throw new Error("stored operator action status is invalid");
}

export function parseStoredReceipt(action: Row): OperatorActionReceipt {
  return parseOperationResult(FABRIC_OPERATIONS.operatorActionCommit, action.receipt);
}

export function parseReceipt(serialized: string): OperatorActionReceipt {
  return parseOperationResult(FABRIC_OPERATIONS.operatorActionCommit, JSON.parse(serialized));
}

type NonLaunchReceipt = Exclude<OperatorActionReceipt, { launchProviderActionJournalRef: unknown }>;

export function nonLaunchReceipt(receipt: OperatorActionReceipt): NonLaunchReceipt {
  if (receipt.launchProviderActionJournalRef !== undefined) {
    throw new Error("launch receipt requires launch custody settlement");
  }
  return receipt;
}

export function statusFromAction(action: StoredAction, intentDigest: Sha256Digest): OperatorActionStatus {
  if (action.status === "terminal") {
    return {
      status: "committed",
      commandId: action.commandId,
      receipt: nonLaunchReceipt(action.receipt),
    };
  }
  if (action.status === "pending") {
    return {
      status: "pending",
      commandId: action.commandId,
      intentDigest,
      phase: action.phase,
      attemptGeneration: action.attemptGeneration,
    };
  }
  if (action.status === "ambiguous") {
    return {
      status: "ambiguous",
      commandId: action.commandId,
      intentDigest,
      attemptGeneration: action.attemptGeneration,
      effectRef: action.effectRef,
    };
  }
  return {
    status: "rejected",
    commandId: action.commandId,
    intentDigest,
    code: action.code,
    evidenceRefs: action.evidenceRefs,
  };
}

export function parseRejectedStatus(serialized: string, commandId: string): Extract<OperatorActionStatus, { status: "rejected" }> {
  const value: unknown = JSON.parse(serialized);
  const stored = row(value, "stored rejected operator action");
  if (text(stored, "status") !== "rejected" || text(stored, "commandId") !== commandId) {
    throw new Error("stored rejected operator action identity is invalid");
  }
  const evidence = stored.evidenceRefs;
  if (!Array.isArray(evidence)) throw new Error("stored rejected operator action evidence is invalid");
  return {
    status: "rejected",
    commandId,
    intentDigest: parseSha256Digest(stored.intentDigest, "storedRejectedAction.intentDigest"),
    code: rejectionCode(stored.code),
    evidenceRefs: evidence.map((item, index) => parseArtifactRef(item, `storedRejectedAction.evidenceRefs[${String(index)}]`)),
  };
}

export function digestValue(value: unknown, path: string): Sha256Digest {
  return parseSha256Digest(`sha256:${sha256(canonicalJson(value))}`, path);
}

export function rejectionCode(value: unknown): OperatorActionRejectionCode {
  if (
    value === "authority-insufficient" ||
    value === "preview-expired" ||
    value === "preview-stale" ||
    value === "state-changed" ||
    value === "generation-stale" ||
    value === "git-state-changed" ||
    value === "external-contract-unknown" ||
    value === "external-contract-stale" ||
    value === "release-binding-mismatch" ||
    value === "dedupe-conflict"
  ) return value;
  throw new Error("stored operator action rejection code is invalid");
}

export function protocolCodeForRejection(code: OperatorActionRejectionCode): ConstructorParameters<typeof ProjectFabricCoreError>[0] {
  if (code === "authority-insufficient") return "CAPABILITY_FORBIDDEN";
  if (code === "preview-expired") return "CAPABILITY_EXPIRED";
  if (code === "release-binding-mismatch") return "GATE_BLOCKED";
  if (code === "dedupe-conflict") return "DEDUPE_CONFLICT";
  return "STALE_REVISION";
}

/**
 * Shared with `action-store.ts`'s chair-recovery and chair-live-handoff commit paths (S4c):
 * releases a provider-action preflight ticket if in-transaction preparation throws, then
 * rethrows the original error so it stays authoritative.
 */
export function prepareProviderActionAfterPreflight<T>(
  prepare: () => T,
  custody: Readonly<{
    releaseProviderActionPreflightAfterRollback?(ticket: ProviderActionTicket, failure: unknown): void;
  }>,
  ticket: ProviderActionTicket,
): T {
  try {
    return prepare();
  } catch (error: unknown) {
    try {
      custody.releaseProviderActionPreflightAfterRollback?.(ticket, error);
    } catch {
      // Preserve the original preparation error if release races or fails.
    }
    throw error;
  }
}

/**
 * Narrow stored-action/journal substrate shared by every operator-action custody family:
 * preview claim/scope assertions, command replay lookups, command-table persistence, and
 * effect-outcome application. Extracted byte-for-byte from `OperatorActionStore` (S4a);
 * auth (`#authenticateCommand`) stays in the facade.
 */
export class OperatorActionJournal {
  readonly #database: Database.Database;
  readonly #clock: () => number;

  constructor(database: Database.Database, clock: () => number) {
    this.#database = database;
    this.#clock = clock;
  }

  previewRow(previewId: string): Row {
    return row(this.#database.prepare(`
      SELECT * FROM operator_previews WHERE preview_id=?
    `).get(previewId), "operator action preview");
  }

  assertStoredPreviewScope(
    stored: Row,
    authenticated: AuthenticatedOperatorCredential,
    projectId: string,
    intent: OperatorActionIntent,
    actionSessionId: (authenticated: AuthenticatedOperatorCredential, intent: OperatorActionIntent) => string,
  ): void {
    const storedSessionId = text(stored, "project_session_id");
    if (actionSessionId(authenticated, intent) !== storedSessionId) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator preview belongs to another session");
    }
    if (!isRow(this.#database.prepare(`
      SELECT project_session_id FROM project_sessions WHERE project_session_id=? AND project_id=?
    `).get(storedSessionId, projectId))) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "operator preview belongs to another project");
    }
  }

  assertPreviewClaim(stored: Row, commandId: string): void {
    const confirmedCommandId = nullableText(stored, "confirmed_command_id");
    if (confirmedCommandId !== null && confirmedCommandId !== commandId) {
      throw new ProjectFabricCoreError("CONFLICT", "operator preview already has another confirmation command");
    }
  }

  commandReplay(operatorId: string, commandId: string, payloadHash: string): OperatorActionReceipt | null {
    const existing = this.#database.prepare(`
      SELECT payload_hash, result_json, status FROM operator_commands
       WHERE operator_id=? AND command_id=?
    `).get(operatorId, commandId);
    if (!isRow(existing)) return null;
    if (text(existing, "payload_hash") !== payloadHash) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "operator command ID was reused with changed input");
    }
    if (text(existing, "status") === "rejected") {
      const status = parseRejectedStatus(text(existing, "result_json"), commandId);
      throw new ProjectFabricCoreError(protocolCodeForRejection(status.code), `operator action was rejected: ${status.code}`);
    }
    return parseReceipt(text(existing, "result_json"));
  }

  reconcileReplay(operatorId: string, commandId: string, payloadHash: string): OperatorActionStatus | null {
    const existing = this.#database.prepare(`
      SELECT payload_hash, result_json FROM operator_commands
       WHERE operator_id=? AND command_id=?
    `).get(operatorId, commandId);
    if (!isRow(existing)) return null;
    if (text(existing, "payload_hash") !== payloadHash) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "reconciliation command ID was reused with changed input");
    }
    return parseOperationResult(FABRIC_OPERATIONS.operatorActionReconcile, JSON.parse(text(existing, "result_json")));
  }

  insertCommand(
    context: AuthenticatedOperatorContext,
    command: OperatorActionCommitRequest["command"],
    projectId: string,
    projectSessionId: string,
    operation: string,
    payloadHash: string,
    before: unknown,
    after: unknown,
    result: unknown,
    status: "committed" | "rejected",
  ): void {
    this.#database.prepare(`
      INSERT INTO operator_commands(
        operator_id, command_id, capability_id, project_id, project_session_id,
        operation, expected_revision, payload_hash, provenance_json, before_json,
        after_json, evidence_json, result_json, status, created_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      context.operatorId,
      command.commandId,
      command.credential.capabilityId,
      projectId,
      projectSessionId,
      operation,
      command.expectedRevision,
      payloadHash,
      canonicalJson(command.provenance),
      canonicalJson(before),
      canonicalJson(after),
      canonicalJson(command.evidenceRefs),
      canonicalJson(result),
      status,
      this.#clock(),
    );
  }

  recordRejected(
    context: AuthenticatedOperatorContext,
    request: OperatorActionCommitRequest,
    stored: Row,
    envelope: StoredPreviewEnvelope,
    payloadHash: string,
    code: OperatorActionRejectionCode,
    commandReplay: (operatorId: string, commandId: string, payloadHash: string) => OperatorActionReceipt | null,
  ): void {
    const rejected: StoredRejectedAction = {
      status: "rejected",
      commandId: request.command.commandId,
      code,
      evidenceRefs: envelope.preview.evidenceRefs,
    };
    const transaction = this.#database.transaction((): void => {
      commandReplay(context.operatorId, request.command.commandId, payloadHash);
      const latest = this.previewRow(request.previewId);
      this.assertPreviewClaim(latest, request.command.commandId);
      this.#database.prepare(`
        UPDATE operator_previews SET preview_json=?, confirmed_command_id=? WHERE preview_id=?
      `).run(
        canonicalJson({ preview: envelope.preview, action: rejected }),
        request.command.commandId,
        request.previewId,
      );
      this.insertCommand(
        context,
        request.command,
        request.projectId,
        text(stored, "project_session_id"),
        text(stored, "operation"),
        payloadHash,
        { beforeStateDigest: envelope.preview.beforeStateDigest },
        rejected,
        {
          status: "rejected",
          commandId: request.command.commandId,
          intentDigest: envelope.preview.intentDigest,
          code,
          evidenceRefs: envelope.preview.evidenceRefs,
        },
        "rejected",
      );
    });
    transaction();
  }

  recordRetryableLaunchExpiry(
    request: OperatorActionCommitRequest,
    envelope: StoredPreviewEnvelope,
  ): void {
    const rejected: StoredRejectedAction = {
      status: "rejected",
      commandId: request.command.commandId,
      code: "preview-expired",
      evidenceRefs: envelope.preview.evidenceRefs,
    };
    const transaction = this.#database.transaction((): void => {
      const latest = this.previewRow(request.previewId);
      this.assertPreviewClaim(latest, request.command.commandId);
      this.#database.prepare(`
        UPDATE operator_previews SET preview_json=?, confirmed_command_id=NULL WHERE preview_id=?
      `).run(canonicalJson({ preview: envelope.preview, action: rejected }), request.previewId);
    });
    transaction();
  }

  applyEffectOutcome(
    operatorId: string,
    commandId: string,
    preview: OperatorActionPreview,
    priorReceipt: OperatorActionReceipt,
    outcome: Readonly<
      | { status: "committed"; afterState: unknown; effectRef?: ArtifactRef }
      | { status: "pending"; phase: "accepted" | "observing" }
      | { status: "ambiguous"; effectRef: ArtifactRef }
      | { status: "rejected"; code: OperatorActionRejectionCode; evidenceRefs: readonly ArtifactRef[] }
    >,
    attemptGeneration: number,
  ): OperatorActionReceipt {
    if (outcome.status === "rejected") {
      const rejected: StoredRejectedAction = {
        status: "rejected",
        commandId,
        code: outcome.code,
        evidenceRefs: outcome.evidenceRefs,
      };
      const transaction = this.#database.transaction((): void => {
        this.updateStoredAction(preview.previewId, preview, rejected);
        this.#database.prepare(`
          UPDATE operator_commands SET status='rejected', result_json=?, after_json=?
           WHERE operator_id=? AND command_id=?
        `).run(
          canonicalJson({
            status: "rejected",
            commandId,
            intentDigest: preview.intentDigest,
            code: outcome.code,
            evidenceRefs: outcome.evidenceRefs,
          }),
          canonicalJson(rejected),
          operatorId,
          commandId,
        );
      });
      transaction();
      throw new ProjectFabricCoreError(protocolCodeForRejection(outcome.code), `operator effect rejected: ${outcome.code}`);
    }
    if (outcome.status === "pending") {
      const pending: StoredPendingAction = {
        status: "pending",
        commandId,
        phase: outcome.phase,
        attemptGeneration,
        receipt: priorReceipt,
      };
      this.updateStoredAction(preview.previewId, preview, pending);
      return priorReceipt;
    }
    if (outcome.status === "ambiguous") {
      const ambiguous: StoredAmbiguousAction = {
        status: "ambiguous",
        commandId,
        attemptGeneration,
        effectRef: outcome.effectRef,
        receipt: priorReceipt,
      };
      this.updateStoredAction(preview.previewId, preview, ambiguous);
      return priorReceipt;
    }
    const receipt: OperatorActionReceipt = {
      ...priorReceipt,
      afterStateDigest: digestValue(outcome.afterState, "operatorActionCommit.afterStateDigest"),
      ...(outcome.effectRef === undefined ? {} : { effectRef: outcome.effectRef }),
    };
    const terminal: StoredTerminalAction = { status: "terminal", commandId, receipt };
    const transaction = this.#database.transaction((): void => {
      this.updateStoredAction(preview.previewId, preview, terminal);
      this.#database.prepare(`
        UPDATE operator_commands SET result_json=?, after_json=?
         WHERE operator_id=? AND command_id=? AND status='committed'
      `).run(canonicalJson(receipt), canonicalJson(outcome.afterState), operatorId, commandId);
    });
    transaction();
    return receipt;
  }

  updateStoredAction(previewId: string, preview: OperatorActionPreview, action: StoredAction): void {
    this.#database.prepare(`
      UPDATE operator_previews SET preview_json=? WHERE preview_id=?
    `).run(canonicalJson({ preview, action }), previewId);
  }

  updateReconcileCommand(operatorId: string, commandId: string, result: OperatorActionStatus): void {
    this.#database.prepare(`
      UPDATE operator_commands SET result_json=?, after_json=?
       WHERE operator_id=? AND command_id=?
    `).run(canonicalJson(result), canonicalJson(result), operatorId, commandId);
  }
}
