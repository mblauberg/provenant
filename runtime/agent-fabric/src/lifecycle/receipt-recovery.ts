import { createHash } from "node:crypto";

import type Database from "better-sqlite3";

import { canonicalJson, integer, nullableText, row, text, type Row } from "../persistence/row-codec.js";
import type {
  LifecycleAuthenticatedNamespaceCheckpoint,
  LifecycleAuthenticatedScopeCheckpoint,
  LifecycleDigest,
  LifecycleIntegrityReceiptAuthorityPort,
  LifecycleReceiptRecord,
} from "./receipt-authority.js";

const PAGE_LIMIT = 256;
const MAX_NAMESPACE_SCOPES = 65_536;
const MAX_SCOPE_RECEIPTS = 65_536;
const DEFAULT_AUTHORITY_CALL_TIMEOUT_MS = 5_000;

class LifecycleReceiptAuthorityTimeoutError extends Error {}

/** A typed, retryable authority transport/availability failure. */
export class LifecycleReceiptAuthorityUnavailableError extends Error {
  constructor(message = "lifecycle receipt authority is unavailable", options?: ErrorOptions) {
    super(message, options);
    this.name = "LifecycleReceiptAuthorityUnavailableError";
  }
}

export type LifecycleReceiptRecoveryErrorCode = "RECOVERY_PENDING" | "SNAPSHOT_INVALID";

export class LifecycleReceiptRecoveryError extends Error {
  readonly code: LifecycleReceiptRecoveryErrorCode;

  constructor(code: LifecycleReceiptRecoveryErrorCode, message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "LifecycleReceiptRecoveryError";
    this.code = code;
  }
}

export type LifecycleReceiptRecoveryResult = Readonly<{
  projectId: string;
  namespaceCheckpointDigest: LifecycleDigest;
  scopeCount: number;
  receiptCount: number;
  pendingIntentCount: number;
  committedReceiptCount: number;
}>;

export type LifecycleReceiptRecoveryOptions = Readonly<{
  authorityCallTimeoutMs?: number;
}>;

export function hasKnownLifecycleReceiptState(database: Database.Database): boolean {
  return database.prepare(`
    SELECT 1
      FROM (
        SELECT project_id FROM lifecycle_receipt_projects
        UNION
        SELECT project_id FROM lifecycle_admitted_run_scopes
        UNION
        SELECT project_id FROM lifecycle_scope_admission_outbox
      )
     LIMIT 1
  `).get() !== undefined;
}

type NamespaceMember = Readonly<{
  projectSessionId: string;
  runId: string;
  authorityId: string;
  scopeCheckpointDigest: LifecycleDigest;
  receiptCountDec: string;
  headReceiptDigest: LifecycleDigest | null;
}>;

type LocalScope = Readonly<{
  projectId: string;
  projectSessionId: string;
  runId: string;
  authorityId: string;
  admissionDigest: LifecycleDigest;
  admittedAt: number;
  admissionRequestId: string;
  scopeDigest: LifecycleDigest;
  initialScopeCheckpointDigest: LifecycleDigest;
  resolutionDigest: LifecycleDigest;
}>;

type LocalIntent = Readonly<{
  batchId: string;
  ordinal: number;
  intentDigest: LifecycleDigest;
  subjectJson: string;
  subjectDigest: LifecycleDigest;
  kind: string;
  projectSessionId: string;
  runId: string;
  agentId: string;
  ownerKind: string;
  ownerId: string;
  ownerRevision: number;
  receipt: LocalReceipt | null;
  applied: boolean;
}>;

type LocalReceipt = Readonly<{
  batchId: string;
  ordinal: number;
  intentDigest: LifecycleDigest;
  projectSessionId: string;
  runId: string;
  agentId: string;
  kind: string;
  ownerKind: string;
  ownerId: string;
  ownerRevision: number;
  subjectDigest: LifecycleDigest;
  authorityId: string;
  authoritySequence: number;
  previousAuthoritySequence: number | null;
  previousReceiptDigest: LifecycleDigest | null;
  receiptJson: string;
  receiptDigest: LifecycleDigest;
  attestation: string;
  verifiedAt: number;
}>;

function lifecycleDigest(domain: string, value: unknown): LifecycleDigest {
  return `sha256:${createHash("sha256")
    .update(`agent-fabric.lifecycle.v1\0${domain}\0${canonicalJson(value)}`)
    .digest("hex")}`;
}

function scopeKey(projectSessionId: string, runId: string): string {
  return `${projectSessionId}\0${runId}`;
}

function receiptOwner(record: LifecycleReceiptRecord): Readonly<{
  kind: string;
  id: string;
  revision: number;
}> {
  const owner = row(record.subject.ownerRef, "lifecycle receipt owner");
  if (owner.kind === "custody") {
    const custody = row(owner.custodyRef, "lifecycle receipt custody owner");
    return { kind: "custody", id: text(custody, "custodyId"), revision: integer(custody, "custodyRevision") };
  }
  if (owner.kind === "generation-loss") {
    const loss = row(owner.generationLossRef, "lifecycle receipt generation-loss owner");
    return {
      kind: "generation-loss",
      id: text(loss, "generationLossId"),
      revision: integer(loss, "generationLossRevision"),
    };
  }
  if (owner.kind === "recovery-retirement") {
    const retirement = row(owner.retirementRef, "lifecycle receipt retirement owner");
    const revision = Number(text(retirement, "revisionDec"));
    if (!Number.isSafeInteger(revision) || revision < 1) throw new Error("invalid lifecycle receipt owner revision");
    return { kind: "recovery-retirement", id: text(retirement, "retirementId"), revision };
  }
  throw new Error("invalid lifecycle receipt owner kind");
}

function receiptSetMember(record: LifecycleReceiptRecord): readonly [
  string, LifecycleDigest, LifecycleDigest, string, string, string, string, string,
] {
  const owner = receiptOwner(record);
  return [
    String(record.receipt.authoritySequence),
    record.receipt.receiptDigest,
    record.receipt.intentDigest,
    text(record.subject as Row, "kind"),
    text(record.subject as Row, "agentId"),
    owner.kind,
    owner.id,
    String(owner.revision),
  ];
}

function scopeCheckpointBody(checkpoint: LifecycleAuthenticatedScopeCheckpoint): Readonly<Record<string, unknown>> {
  return {
    schemaVersion: 1,
    authorityId: checkpoint.authorityId,
    projectSessionId: checkpoint.projectSessionId,
    runId: checkpoint.runId,
    receiptCountDec: String(checkpoint.receiptCount),
    headAuthoritySequenceDec: String(checkpoint.headAuthoritySequence),
    headReceiptDigest: checkpoint.headReceiptDigest,
    orderedRecordSetDigest: checkpoint.orderedRecordSetDigest,
  };
}

function namespaceCheckpointBody(
  checkpoint: LifecycleAuthenticatedNamespaceCheckpoint,
): Readonly<Record<string, unknown>> {
  return {
    schemaVersion: 1,
    authorityId: checkpoint.authorityId,
    projectId: checkpoint.projectId,
    scopeCountDec: String(checkpoint.scopeCount),
    orderedScopeHeadSetDigest: checkpoint.orderedScopeHeadSetDigest,
  };
}

function namespaceMember(head: LifecycleAuthenticatedScopeCheckpoint): NamespaceMember {
  return {
    projectSessionId: head.projectSessionId,
    runId: head.runId,
    authorityId: head.authorityId,
    scopeCheckpointDigest: head.checkpointDigest,
    receiptCountDec: String(head.receiptCount),
    headReceiptDigest: head.headReceiptDigest,
  };
}

function storedScopeCheckpointMatches(storedJson: string, checkpoint: LifecycleAuthenticatedScopeCheckpoint): boolean {
  const canonicalStored = canonicalJson(JSON.parse(storedJson));
  const authenticatedBody = {
    ...scopeCheckpointBody(checkpoint),
    checkpointDigest: checkpoint.checkpointDigest,
    attestation: checkpoint.attestation,
  };
  return canonicalStored === canonicalJson(scopeCheckpointBody(checkpoint)) ||
    canonicalStored === canonicalJson(authenticatedBody);
}

function localScope(value: unknown): LocalScope {
  const stored = row(value, "admitted lifecycle receipt scope");
  return {
    projectId: text(stored, "project_id"),
    projectSessionId: text(stored, "project_session_id"),
    runId: text(stored, "run_id"),
    authorityId: text(stored, "authority_id"),
    admissionDigest: text(stored, "admission_digest") as LifecycleDigest,
    admittedAt: integer(stored, "admitted_at"),
    admissionRequestId: text(stored, "admission_request_id"),
    scopeDigest: text(stored, "scope_digest") as LifecycleDigest,
    initialScopeCheckpointDigest: text(stored, "initial_scope_checkpoint_digest") as LifecycleDigest,
    resolutionDigest: text(stored, "scope_admission_resolution_digest") as LifecycleDigest,
  };
}

function localIntent(value: unknown): LocalIntent {
  const stored = row(value, "lifecycle receipt intent");
  const receiptMarker = nullableText(stored, "receipt_row_intent_digest");
  const receiptFields = [
    "receipt_batch_id", "receipt_ordinal", "receipt_project_session_id", "receipt_run_id", "receipt_agent_id", "receipt_kind",
    "receipt_owner_kind", "receipt_owner_id", "receipt_owner_revision", "receipt_subject_digest",
    "receipt_authority_id", "receipt_authority_sequence", "previous_authority_sequence",
    "previous_receipt_digest", "receipt_json", "receipt_digest", "receipt_attestation", "receipt_verified_at",
  ] as const;
  if (receiptMarker === null && receiptFields.some((field) => stored[field] !== null)) {
    throw new Error("lifecycle authority receipt row is partial");
  }
  const receipt = receiptMarker === null ? null : {
    batchId: text(stored, "receipt_batch_id"),
    ordinal: integer(stored, "receipt_ordinal"),
    intentDigest: receiptMarker as LifecycleDigest,
    projectSessionId: text(stored, "receipt_project_session_id"),
    runId: text(stored, "receipt_run_id"),
    agentId: text(stored, "receipt_agent_id"),
    kind: text(stored, "receipt_kind"),
    ownerKind: text(stored, "receipt_owner_kind"),
    ownerId: text(stored, "receipt_owner_id"),
    ownerRevision: integer(stored, "receipt_owner_revision"),
    subjectDigest: text(stored, "receipt_subject_digest") as LifecycleDigest,
    authorityId: text(stored, "receipt_authority_id"),
    authoritySequence: integer(stored, "receipt_authority_sequence"),
    previousAuthoritySequence: nullableInteger(stored, "previous_authority_sequence"),
    previousReceiptDigest: nullableText(stored, "previous_receipt_digest") as LifecycleDigest | null,
    receiptJson: text(stored, "receipt_json"),
    receiptDigest: text(stored, "receipt_digest") as LifecycleDigest,
    attestation: text(stored, "receipt_attestation"),
    verifiedAt: integer(stored, "receipt_verified_at"),
  } satisfies LocalReceipt;
  return {
    batchId: text(stored, "batch_id"),
    ordinal: integer(stored, "ordinal"),
    intentDigest: text(stored, "intent_digest") as LifecycleDigest,
    subjectJson: text(stored, "subject_json"),
    subjectDigest: text(stored, "subject_digest") as LifecycleDigest,
    kind: text(stored, "kind"),
    projectSessionId: text(stored, "project_session_id"),
    runId: text(stored, "run_id"),
    agentId: text(stored, "agent_id"),
    ownerKind: text(stored, "subject_owner_kind"),
    ownerId: text(stored, "subject_owner_id"),
    ownerRevision: integer(stored, "subject_owner_revision"),
    receipt,
    applied: integer(stored, "is_applied") === 1,
  };
}

function nullableInteger(value: Row, field: string): number | null {
  const item = value[field];
  if (item === null) return null;
  if (typeof item !== "number" || !Number.isSafeInteger(item)) throw new Error(`${field} is not a nullable integer`);
  return item;
}

export class LifecycleReceiptRecoveryService {
  readonly #database: Database.Database;
  readonly #authority: LifecycleIntegrityReceiptAuthorityPort;
  readonly #authorityCallTimeoutMs: number;

  constructor(
    database: Database.Database,
    authority: LifecycleIntegrityReceiptAuthorityPort,
    options: LifecycleReceiptRecoveryOptions = {},
  ) {
    this.#database = database;
    this.#authority = authority;
    const timeout = options.authorityCallTimeoutMs ?? DEFAULT_AUTHORITY_CALL_TIMEOUT_MS;
    if (!Number.isSafeInteger(timeout) || timeout < 1) {
      throw new TypeError("lifecycle receipt authority timeout must be a positive integer");
    }
    this.#authorityCallTimeoutMs = timeout;
  }

  async hydrateKnownProjects(): Promise<readonly LifecycleReceiptRecoveryResult[]> {
    const projects = this.#database.prepare(`
      SELECT project_id FROM lifecycle_receipt_projects
      UNION
      SELECT project_id FROM lifecycle_admitted_run_scopes
      UNION
      SELECT project_id FROM lifecycle_scope_admission_outbox
      ORDER BY project_id
    `).all() as ReadonlyArray<Readonly<{ project_id: unknown }>>;
    const results: LifecycleReceiptRecoveryResult[] = [];
    for (const project of projects) {
      if (typeof project.project_id !== "string" || project.project_id.length === 0) {
        throw new LifecycleReceiptRecoveryError(
          "SNAPSHOT_INVALID",
          "lifecycle receipt project id is invalid",
        );
      }
      const registration = this.#database.prepare(`
        SELECT authority_id FROM lifecycle_receipt_projects WHERE project_id=?
      `).get(project.project_id);
      if (
        registration !== undefined &&
        text(row(registration, "lifecycle receipt project"), "authority_id") !== this.#authority.authorityId
      ) {
        throw new LifecycleReceiptRecoveryError(
          "SNAPSHOT_INVALID",
          "lifecycle receipt project authority crossed",
        );
      }
      results.push(await this.hydrateProject(project.project_id));
    }
    return results;
  }

  async hydrateProject(projectId: string): Promise<LifecycleReceiptRecoveryResult> {
    if (projectId.length === 0) {
      throw new LifecycleReceiptRecoveryError("SNAPSHOT_INVALID", "lifecycle receipt project id is empty");
    }
    if (this.#hasUnresolvedAdmission(projectId)) {
      throw new LifecycleReceiptRecoveryError(
        "RECOVERY_PENDING",
        "lifecycle receipt scope admission recovery is pending",
      );
    }
    try {
      return await this.#hydrateAuthenticatedProject(projectId);
    } catch (error: unknown) {
      if (error instanceof LifecycleReceiptRecoveryError) throw error;
      if (
        error instanceof LifecycleReceiptAuthorityTimeoutError ||
        error instanceof LifecycleReceiptAuthorityUnavailableError
      ) {
        throw new LifecycleReceiptRecoveryError(
          "RECOVERY_PENDING",
          "lifecycle receipt authority recovery is pending",
          { cause: error },
        );
      }
      throw new LifecycleReceiptRecoveryError(
        "SNAPSHOT_INVALID",
        "lifecycle receipt namespace hydration failed",
        { cause: error },
      );
    }
  }

  async #authorityCall<T>(label: string, operation: () => Promise<T>): Promise<T> {
    let timeout: NodeJS.Timeout | undefined;
    try {
      return await Promise.race([
        operation(),
        new Promise<never>((_resolve, reject) => {
          timeout = setTimeout(() => reject(new LifecycleReceiptAuthorityTimeoutError(label)), this.#authorityCallTimeoutMs);
          timeout.unref();
        }),
      ]);
    } finally {
      if (timeout !== undefined) clearTimeout(timeout);
    }
  }

  #hasUnresolvedAdmission(projectId: string): boolean {
    return this.#database.prepare(`
      SELECT 1
        FROM lifecycle_scope_admission_outbox outbox
        LEFT JOIN lifecycle_scope_admission_resolutions resolution
          ON resolution.admission_request_id=outbox.admission_request_id
       WHERE outbox.project_id=? AND resolution.admission_request_id IS NULL
       LIMIT 1
    `).get(projectId) !== undefined;
  }

  async #hydrateAuthenticatedProject(projectId: string): Promise<LifecycleReceiptRecoveryResult> {
    const namespace = await this.#authorityCall(
      "lifecycle receipt namespace read timed out",
      async () => await this.#authority.readNamespaceCheckpoint(projectId),
    );
    if (
      namespace.projectId !== projectId || namespace.authorityId !== this.#authority.authorityId ||
      namespace.scopeCount < 0 || !Number.isSafeInteger(namespace.scopeCount) ||
      namespace.checkpointDigest !== lifecycleDigest("namespace-checkpoint", namespaceCheckpointBody(namespace)) ||
      !await this.#authorityCall(
        "lifecycle receipt namespace verification timed out",
        async () => await this.#authority.verifyNamespaceCheckpoint(namespace),
      )
    ) {
      throw new Error("invalid lifecycle receipt namespace checkpoint");
    }
    const heads = await this.#readNamespaceHeads(namespace);
    const members = heads.map(namespaceMember);
    if (
      heads.length !== namespace.scopeCount ||
      namespace.orderedScopeHeadSetDigest !== lifecycleDigest("namespace-scope-head-set", members)
    ) {
      throw new Error("lifecycle receipt namespace set drifted");
    }

    const scopes = (this.#database.prepare(`
      SELECT project_id,project_session_id,run_id,authority_id,admission_request_id,
             admission_digest,admitted_at,scope_digest,initial_scope_checkpoint_digest,
             scope_admission_resolution_digest
        FROM lifecycle_admitted_run_scopes WHERE project_id=?
       ORDER BY project_session_id,run_id
    `).all(projectId) as unknown[]).map(localScope);
    if (scopes.length !== heads.length) throw new Error("lifecycle receipt namespace scope set drifted");

    const intents = (this.#database.prepare(`
      SELECT intent.batch_id,intent.ordinal,intent.intent_digest,intent.subject_json,intent.subject_digest,intent.kind,
             intent.project_session_id,intent.run_id,intent.agent_id,
             intent.subject_owner_kind,intent.subject_owner_id,intent.subject_owner_revision,
             receipt.intent_digest AS receipt_row_intent_digest,
             receipt.batch_id AS receipt_batch_id,receipt.ordinal AS receipt_ordinal,
             receipt.project_session_id AS receipt_project_session_id,
             receipt.run_id AS receipt_run_id,receipt.agent_id AS receipt_agent_id,
             receipt.kind AS receipt_kind,receipt.subject_owner_kind AS receipt_owner_kind,
             receipt.subject_owner_id AS receipt_owner_id,
             receipt.subject_owner_revision AS receipt_owner_revision,
             receipt.subject_digest AS receipt_subject_digest,
             receipt.authority_id AS receipt_authority_id,
             receipt.authority_sequence AS receipt_authority_sequence,
             receipt.previous_authority_sequence,receipt.previous_receipt_digest,
             receipt.receipt_json,receipt.receipt_digest,receipt.attestation AS receipt_attestation,
             receipt.verified_at AS receipt_verified_at,
             EXISTS(SELECT 1 FROM lifecycle_transition_applies applied
                      WHERE applied.receipt_batch_id=intent.batch_id) AS is_applied
        FROM lifecycle_receipt_intents intent
        LEFT JOIN lifecycle_authority_receipts receipt ON receipt.intent_digest=intent.intent_digest
        JOIN lifecycle_admitted_run_scopes scope
          ON scope.project_session_id=intent.project_session_id AND scope.run_id=intent.run_id
       WHERE scope.project_id=?
    `).all(projectId) as unknown[]).map(localIntent);
    if (intents.some((intent) => intent.applied && intent.receipt === null)) {
      throw new Error("applied lifecycle receipt intent is missing its committed receipt");
    }
    if (intents.some((intent) => intent.applied && !this.#appliedOwnerExists(intent))) {
      throw new Error("applied lifecycle receipt owner is missing");
    }
    const intentByDigest = new Map(intents.map((intent) => [intent.intentDigest, intent]));
    const externalIntentDigests = new Set<LifecycleDigest>();
    let receiptCount = 0;

    for (let index = 0; index < heads.length; index += 1) {
      const head = heads[index]!;
      const scope = scopes[index]!;
      if (
        scopeKey(scope.projectSessionId, scope.runId) !== scopeKey(head.projectSessionId, head.runId) ||
        scope.authorityId !== head.authorityId || head.authorityId !== this.#authority.authorityId
      ) {
        throw new Error("lifecycle receipt scope authority crossed");
      }
      await this.#verifyLocalAdmission(scope);
      await this.#verifyLocalScopeHead(scope, head);
      const records = await this.#readScopeRecords(head);
      receiptCount += records.length;
      for (const record of records) {
        const local = intentByDigest.get(record.receipt.intentDigest);
        if (local === undefined || externalIntentDigests.has(record.receipt.intentDigest)) {
          throw new Error("lifecycle receipt authority contains an extra receipt");
        }
        this.#compareExternalReceipt(local, record);
        externalIntentDigests.add(record.receipt.intentDigest);
      }
    }
    for (const intent of intents) {
      if (intent.receipt !== null && !externalIntentDigests.has(intent.intentDigest)) {
        throw new Error("lifecycle receipt authority is missing a committed receipt");
      }
    }
    const committedReceiptCount = intents.filter((intent) => intent.receipt !== null).length;
    return {
      projectId,
      namespaceCheckpointDigest: namespace.checkpointDigest,
      scopeCount: heads.length,
      receiptCount,
      pendingIntentCount: intents.length - committedReceiptCount,
      committedReceiptCount,
    };
  }

  #appliedOwnerExists(intent: LocalIntent): boolean {
    if (intent.ownerKind === "custody") {
      return this.#database.prepare(`
        SELECT 1 FROM lifecycle_rotation_custody_revisions
         WHERE project_session_id=? AND run_id=? AND agent_id=?
           AND custody_id=? AND revision=? LIMIT 1
      `).get(
        intent.projectSessionId,
        intent.runId,
        intent.agentId,
        intent.ownerId,
        intent.ownerRevision,
      ) !== undefined;
    }
    if (intent.ownerKind === "generation-loss") {
      return this.#database.prepare(`
        SELECT 1 FROM lifecycle_generation_loss_revisions
         WHERE project_session_id=? AND run_id=? AND agent_id=?
           AND generation_loss_id=? AND revision=? LIMIT 1
      `).get(
        intent.projectSessionId,
        intent.runId,
        intent.agentId,
        intent.ownerId,
        intent.ownerRevision,
      ) !== undefined;
    }
    if (intent.ownerKind === "recovery-retirement") {
      return this.#database.prepare(`
        SELECT 1 FROM lifecycle_recovery_retirement_plans
         WHERE project_session_id=? AND run_id=? AND agent_id=?
           AND retirement_id=? AND revision=? LIMIT 1
      `).get(
        intent.projectSessionId,
        intent.runId,
        intent.agentId,
        intent.ownerId,
        intent.ownerRevision,
      ) !== undefined;
    }
    return false;
  }

  async #verifyLocalScopeHead(
    scope: LocalScope,
    externalHead: LifecycleAuthenticatedScopeCheckpoint,
  ): Promise<void> {
    const stored = row(this.#database.prepare(`
      SELECT checkpoint.authority_id,checkpoint.receipt_count,
             checkpoint.checkpoint_json,checkpoint.checkpoint_digest
        FROM lifecycle_receipt_scope_heads head
        JOIN lifecycle_receipt_scope_checkpoints checkpoint
          ON checkpoint.project_session_id=head.project_session_id
         AND checkpoint.run_id=head.run_id
         AND checkpoint.checkpoint_digest=head.checkpoint_digest
       WHERE head.project_session_id=? AND head.run_id=?
    `).get(scope.projectSessionId, scope.runId), "local lifecycle receipt scope head");
    const checkpointDigest = text(stored, "checkpoint_digest") as LifecycleDigest;
    const pinned = await this.#authorityCall(
      "local lifecycle receipt scope head read timed out",
      async () => await this.#authority.readScopeCheckpointAt(checkpointDigest),
    );
    if (
      text(stored, "authority_id") !== scope.authorityId ||
      pinned.receiptCount !== integer(stored, "receipt_count") ||
      pinned.receiptCount > externalHead.receiptCount ||
      !storedScopeCheckpointMatches(text(stored, "checkpoint_json"), pinned) ||
      !await this.#authorityCall(
        "local lifecycle receipt scope head verification timed out",
        async () => await this.#authority.verifyScopeCheckpoint(pinned),
      )
    ) {
      throw new Error("local lifecycle receipt scope head drifted");
    }
  }

  async #readNamespaceHeads(
    namespace: LifecycleAuthenticatedNamespaceCheckpoint,
  ): Promise<readonly LifecycleAuthenticatedScopeCheckpoint[]> {
    const heads: LifecycleAuthenticatedScopeCheckpoint[] = [];
    let after: string | null = null;
    do {
      const page = await this.#authorityCall(
        "lifecycle receipt namespace page timed out",
        async () => await this.#authority.readNamespacePageAt(namespace.checkpointDigest, after, PAGE_LIMIT),
      );
      if (page.orderedScopeHeads.length > PAGE_LIMIT || heads.length + page.orderedScopeHeads.length > MAX_NAMESPACE_SCOPES) {
        throw new Error("lifecycle receipt namespace exceeds its bounded scan");
      }
      for (const head of page.orderedScopeHeads) {
        const pinned = await this.#authorityCall(
          "lifecycle receipt scope checkpoint read timed out",
          async () => await this.#authority.readScopeCheckpointAt(head.checkpointDigest),
        );
        const key = scopeKey(head.projectSessionId, head.runId);
        const previous = heads.at(-1);
        if (
          canonicalJson(head) !== canonicalJson(pinned) ||
          head.authorityId !== this.#authority.authorityId ||
          head.checkpointDigest !== lifecycleDigest("scope-checkpoint", scopeCheckpointBody(head)) ||
          !await this.#authorityCall(
            "lifecycle receipt scope verification timed out",
            async () => await this.#authority.verifyScopeCheckpoint(head),
          ) ||
          (previous !== undefined && key <= scopeKey(previous.projectSessionId, previous.runId))
        ) {
          throw new Error("lifecycle receipt namespace contains a crossed scope");
        }
        heads.push(head);
      }
      if (page.nextAfter === null) break;
      if (page.orderedScopeHeads.length === 0 || page.nextAfter.length === 0 || page.nextAfter === after) {
        throw new Error("lifecycle receipt namespace pagination crossed");
      }
      after = page.nextAfter;
    } while (true);
    return heads;
  }

  async #verifyLocalAdmission(scope: LocalScope): Promise<void> {
    const crossing = this.#database.prepare(`
      SELECT outbox.authority_id AS outbox_authority_id,
             outbox.admission_digest AS outbox_admission_digest,
             outbox.admitted_at AS outbox_admitted_at,outbox.scope_json,outbox.scope_digest AS outbox_scope_digest,
             resolution.authority_id AS resolution_authority_id,
             resolution.project_id AS resolution_project_id,
             resolution.project_session_id AS resolution_project_session_id,
             resolution.run_id AS resolution_run_id,
             resolution.admission_digest AS resolution_admission_digest,
             resolution.admitted_at AS resolution_admitted_at,
             resolution.scope_digest AS resolution_scope_digest,
             resolution.initial_receipt_count,resolution.initial_head_authority_sequence,
             resolution.initial_ordered_record_set_digest,
             resolution.initial_scope_checkpoint_json,resolution.initial_scope_head_revision,
             resolution.initial_scope_checkpoint_digest,
             resolution.namespace_checkpoint_digest,
             resolution.namespace_member_json,resolution.verified_at AS resolution_verified_at,
             resolution.resolution_json,resolution.resolution_digest,
             checkpoint.checkpoint_json,member.scope_checkpoint_digest,
             namespace.authority_id AS namespace_authority_id,
             namespace.scope_count AS namespace_scope_count,
             namespace.ordered_scope_head_set_digest AS namespace_set_digest,
             namespace.checkpoint_json AS namespace_checkpoint_json,
             namespace.attestation AS namespace_attestation
        FROM lifecycle_scope_admission_outbox outbox
        JOIN lifecycle_scope_admission_resolutions resolution
          ON resolution.admission_request_id=outbox.admission_request_id
        JOIN lifecycle_receipt_scope_checkpoints checkpoint
          ON checkpoint.project_session_id=resolution.project_session_id
         AND checkpoint.run_id=resolution.run_id
         AND checkpoint.checkpoint_digest=resolution.initial_scope_checkpoint_digest
        JOIN lifecycle_receipt_namespace_members member
          ON member.project_id=resolution.project_id
         AND member.checkpoint_digest=resolution.namespace_checkpoint_digest
         AND member.project_session_id=resolution.project_session_id
         AND member.run_id=resolution.run_id
        JOIN lifecycle_receipt_namespace_checkpoints namespace
          ON namespace.project_id=resolution.project_id
         AND namespace.checkpoint_digest=resolution.namespace_checkpoint_digest
       WHERE outbox.admission_request_id=? AND outbox.project_id=?
         AND outbox.project_session_id=? AND outbox.run_id=?
    `).get(
      scope.admissionRequestId,
      scope.projectId,
      scope.projectSessionId,
      scope.runId,
    );
    const stored = row(crossing, "lifecycle scope admission crossing");
    const admittedScope = {
      schemaVersion: 1,
      projectId: scope.projectId,
      projectSessionId: scope.projectSessionId,
      runId: scope.runId,
      authorityId: scope.authorityId,
      admissionDigest: scope.admissionDigest,
      admittedAt: scope.admittedAt,
    };
    if (
      text(stored, "outbox_authority_id") !== scope.authorityId ||
      text(stored, "outbox_admission_digest") !== scope.admissionDigest ||
      integer(stored, "outbox_admitted_at") !== scope.admittedAt ||
      canonicalJson(JSON.parse(text(stored, "scope_json"))) !== canonicalJson(admittedScope) ||
      text(stored, "outbox_scope_digest") !== scope.scopeDigest ||
      scope.scopeDigest !== lifecycleDigest("admitted-scope", admittedScope) ||
      scope.admissionRequestId !== lifecycleDigest(
        "scope-admission-outbox",
        { schemaVersion: 1, scopeDigest: scope.scopeDigest },
      ) ||
      text(stored, "resolution_authority_id") !== scope.authorityId ||
      text(stored, "resolution_project_id") !== scope.projectId ||
      text(stored, "resolution_project_session_id") !== scope.projectSessionId ||
      text(stored, "resolution_run_id") !== scope.runId ||
      text(stored, "resolution_admission_digest") !== scope.admissionDigest ||
      integer(stored, "resolution_admitted_at") !== scope.admittedAt ||
      text(stored, "resolution_scope_digest") !== scope.scopeDigest ||
      text(stored, "initial_scope_checkpoint_digest") !== scope.initialScopeCheckpointDigest ||
      text(stored, "scope_checkpoint_digest") !== scope.initialScopeCheckpointDigest ||
      text(stored, "resolution_digest") !== scope.resolutionDigest
    ) {
      throw new Error("lifecycle scope admission crossing drifted");
    }
    const namespaceCheckpoint: LifecycleAuthenticatedNamespaceCheckpoint = {
      schemaVersion: 1,
      projectId: scope.projectId,
      authorityId: text(stored, "namespace_authority_id"),
      scopeCount: integer(stored, "namespace_scope_count"),
      orderedScopeHeadSetDigest: text(stored, "namespace_set_digest") as LifecycleDigest,
      checkpointDigest: text(stored, "namespace_checkpoint_digest") as LifecycleDigest,
      attestation: text(stored, "namespace_attestation"),
    };
    const historicalHeads = await this.#readNamespaceHeads(namespaceCheckpoint);
    const historicalMembers = historicalHeads.map(namespaceMember);
    const storedMembers = (this.#database.prepare(`
      SELECT project_session_id,run_id,authority_id,scope_checkpoint_digest,
             receipt_count,head_receipt_digest
        FROM lifecycle_receipt_namespace_members
       WHERE project_id=? AND checkpoint_digest=? ORDER BY ordinal
    `).all(scope.projectId, namespaceCheckpoint.checkpointDigest) as unknown[]).map((value) => {
      const member = row(value, "stored lifecycle receipt namespace member");
      return {
        projectSessionId: text(member, "project_session_id"),
        runId: text(member, "run_id"),
        authorityId: text(member, "authority_id"),
        scopeCheckpointDigest: text(member, "scope_checkpoint_digest") as LifecycleDigest,
        receiptCountDec: String(integer(member, "receipt_count")),
        headReceiptDigest: nullableText(member, "head_receipt_digest") as LifecycleDigest | null,
      } satisfies NamespaceMember;
    });
    const target = historicalMembers.find((member) =>
      member.projectSessionId === scope.projectSessionId && member.runId === scope.runId);
    if (
      namespaceCheckpoint.authorityId !== scope.authorityId ||
      namespaceCheckpoint.checkpointDigest !== lifecycleDigest(
        "namespace-checkpoint",
        namespaceCheckpointBody(namespaceCheckpoint),
      ) ||
      canonicalJson(JSON.parse(text(stored, "namespace_checkpoint_json"))) !==
        canonicalJson(namespaceCheckpointBody(namespaceCheckpoint)) ||
      !await this.#authorityCall(
        "lifecycle receipt admission namespace verification timed out",
        async () => await this.#authority.verifyNamespaceCheckpoint(namespaceCheckpoint),
      ) ||
      historicalMembers.length !== namespaceCheckpoint.scopeCount ||
      namespaceCheckpoint.orderedScopeHeadSetDigest !==
        lifecycleDigest("namespace-scope-head-set", historicalMembers) ||
      canonicalJson(storedMembers) !== canonicalJson(historicalMembers) ||
      target === undefined || target.scopeCheckpointDigest !== scope.initialScopeCheckpointDigest ||
      target.receiptCountDec !== "0" || target.headReceiptDigest !== null ||
      canonicalJson(JSON.parse(text(stored, "namespace_member_json"))) !== canonicalJson(target)
    ) {
      throw new Error("lifecycle scope admission namespace crossing drifted");
    }
    const pinned = await this.#authorityCall(
      "lifecycle receipt admission checkpoint read timed out",
      async () => await this.#authority.readScopeCheckpointAt(scope.initialScopeCheckpointDigest),
    );
    const initialBody = scopeCheckpointBody(pinned);
    const resolutionBody = {
      schemaVersion: 1,
      admissionRequestId: scope.admissionRequestId,
      scopeDigest: scope.scopeDigest,
      initialScopeCheckpoint: pinned,
      namespaceCheckpointDigest: namespaceCheckpoint.checkpointDigest,
      namespaceMember: target,
      verifiedAt: integer(stored, "resolution_verified_at"),
    };
    if (
      pinned.projectSessionId !== scope.projectSessionId || pinned.runId !== scope.runId ||
      pinned.authorityId !== scope.authorityId || pinned.receiptCount !== 0 ||
      pinned.headAuthoritySequence !== 0 || pinned.headReceiptDigest !== null ||
      integer(stored, "initial_receipt_count") !== 0 ||
      integer(stored, "initial_head_authority_sequence") !== 0 ||
      text(stored, "initial_ordered_record_set_digest") !== pinned.orderedRecordSetDigest ||
      canonicalJson(JSON.parse(text(stored, "initial_scope_checkpoint_json"))) !== canonicalJson(initialBody) ||
      integer(stored, "initial_scope_head_revision") !== 1 ||
      text(stored, "namespace_checkpoint_digest") !== namespaceCheckpoint.checkpointDigest ||
      canonicalJson(JSON.parse(text(stored, "resolution_json"))) !== canonicalJson(resolutionBody) ||
      scope.resolutionDigest !== lifecycleDigest("scope-admission-resolution", resolutionBody) ||
      !storedScopeCheckpointMatches(text(stored, "checkpoint_json"), pinned) ||
      !await this.#authorityCall(
        "lifecycle receipt admission checkpoint verification timed out",
        async () => await this.#authority.verifyScopeCheckpoint(pinned),
      )
    ) {
      throw new Error("lifecycle scope admission checkpoint drifted");
    }
  }

  async #readScopeRecords(
    checkpoint: LifecycleAuthenticatedScopeCheckpoint,
  ): Promise<readonly LifecycleReceiptRecord[]> {
    if (
      checkpoint.receiptCount !== checkpoint.headAuthoritySequence ||
      (checkpoint.receiptCount === 0) !== (checkpoint.headReceiptDigest === null)
    ) {
      throw new Error("lifecycle receipt scope head drifted");
    }
    const records: LifecycleReceiptRecord[] = [];
    let after = 0;
    let previousReceiptDigest: LifecycleDigest | null = null;
    do {
      const page = await this.#authorityCall(
        "lifecycle receipt scope page timed out",
        async () => await this.#authority.readScopePageAt(checkpoint.checkpointDigest, after, PAGE_LIMIT),
      );
      if (page.orderedRecords.length > PAGE_LIMIT || records.length + page.orderedRecords.length > MAX_SCOPE_RECEIPTS) {
        throw new Error("lifecycle receipt scope exceeds its bounded scan");
      }
      for (const record of page.orderedRecords) {
        const expectedSequence = records.length + 1;
        if (
          record.receipt.authorityId !== checkpoint.authorityId ||
          record.receipt.authoritySequence !== expectedSequence ||
          record.receipt.previousReceiptDigest !== previousReceiptDigest ||
          record.subject.projectSessionId !== checkpoint.projectSessionId ||
          record.subject.runId !== checkpoint.runId ||
          record.subject.kind !== record.receipt.kind ||
          record.receipt.subjectDigest !== lifecycleDigest("receipt-subject", record.subject) ||
          !await this.#authorityCall(
            "lifecycle receipt verification timed out",
            async () => await this.#authority.verifyReceipt(record.subject, record.receipt),
          )
        ) {
          throw new Error("lifecycle receipt checkpoint page crossed");
        }
        records.push(record);
        previousReceiptDigest = record.receipt.receiptDigest;
      }
      if (page.nextAfter === null) break;
      if (
        page.orderedRecords.length === 0 || page.nextAfter !== records.length ||
        page.nextAfter <= after || page.nextAfter > checkpoint.receiptCount
      ) {
        throw new Error("lifecycle receipt checkpoint pagination crossed");
      }
      after = page.nextAfter;
    } while (true);
    if (
      records.length !== checkpoint.receiptCount ||
      previousReceiptDigest !== checkpoint.headReceiptDigest ||
      checkpoint.orderedRecordSetDigest !== lifecycleDigest("scope-record-set", records.map(receiptSetMember))
    ) {
      throw new Error("lifecycle receipt scope set drifted");
    }
    return records;
  }

  #compareExternalReceipt(local: LocalIntent, record: LifecycleReceiptRecord): void {
    const owner = receiptOwner(record);
    const storedReceipt = local.receipt;
    if (
      canonicalJson(record.subject) !== local.subjectJson ||
      record.receipt.subjectDigest !== local.subjectDigest ||
      record.receipt.kind !== local.kind ||
      record.subject.projectSessionId !== local.projectSessionId ||
      record.subject.runId !== local.runId ||
      record.subject.agentId !== local.agentId ||
      owner.kind !== local.ownerKind || owner.id !== local.ownerId || owner.revision !== local.ownerRevision ||
      (storedReceipt !== null && (
        storedReceipt.intentDigest !== local.intentDigest ||
        storedReceipt.batchId !== local.batchId || storedReceipt.ordinal !== local.ordinal ||
        storedReceipt.projectSessionId !== local.projectSessionId ||
        storedReceipt.runId !== local.runId ||
        storedReceipt.agentId !== local.agentId ||
        storedReceipt.kind !== local.kind ||
        storedReceipt.ownerKind !== local.ownerKind ||
        storedReceipt.ownerId !== local.ownerId ||
        storedReceipt.ownerRevision !== local.ownerRevision ||
        storedReceipt.subjectDigest !== local.subjectDigest ||
        storedReceipt.authorityId !== record.receipt.authorityId ||
        storedReceipt.authoritySequence !== record.receipt.authoritySequence ||
        storedReceipt.previousAuthoritySequence !==
          (record.receipt.authoritySequence === 1 ? null : record.receipt.authoritySequence - 1) ||
        storedReceipt.previousReceiptDigest !== record.receipt.previousReceiptDigest ||
        storedReceipt.receiptDigest !== record.receipt.receiptDigest ||
        storedReceipt.attestation !== record.receipt.attestation ||
        canonicalJson(JSON.parse(storedReceipt.receiptJson)) !== canonicalJson(record.receipt)
      ))
    ) {
      throw new Error("lifecycle receipt authority evidence crossed a local intent");
    }
  }
}
