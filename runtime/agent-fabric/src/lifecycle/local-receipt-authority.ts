import { createHash, createHmac, timingSafeEqual } from "node:crypto";
import {
  closeSync,
  constants,
  fstatSync,
  lstatSync,
  openSync,
  readFileSync,
} from "node:fs";
import { join } from "node:path";

import Database from "better-sqlite3";

import { canonicalJson } from "../persistence/row-codec.js";
import type {
  LifecycleAdmittedRunScope,
  LifecycleAuthenticatedNamespaceCheckpoint,
  LifecycleAuthenticatedReceipt,
  LifecycleAuthenticatedScopeCheckpoint,
  LifecycleDigest,
  LifecycleIntegrityReceiptAuthorityPort,
  LifecycleReceiptLookup,
  LifecycleReceiptRecord,
} from "./receipt-authority.js";

const DATABASE_NAME = "lifecycle-receipts.sqlite3";
const KEY_NAME = "lifecycle-receipts.hmac.key";
const PAGE_LIMIT = 256;

type JsonRow = Readonly<Record<string, unknown>>;
type StoredReceiptRow = Readonly<{
  subject_json: string;
  receipt_json: string;
}>;
type StoredReceiptLedgerRow = StoredReceiptRow & Readonly<{
  kind: string;
  project_session_id: string;
  run_id: string;
  agent_id: string;
  owner_ref_digest: string;
  owner_revision: number;
}>;
type StoredScopeRow = Readonly<{ project_id: string; scope_json: string; scope_attestation: string }>;
type StoredSnapshotRow = Readonly<{
  project_session_id: string;
  run_id: string;
  checkpoint_json: string;
  max_authority_sequence: number;
}>;
type StoredNamespaceRow = Readonly<{ project_id: string; checkpoint_json: string }>;

export type LocalLifecycleReceiptAuthorityOptions = Readonly<{
  stateDirectory: string;
  expectedAuthorityId: string;
}>;

function lifecycleDigest(domain: string, value: unknown): LifecycleDigest {
  return `sha256:${createHash("sha256")
    .update(`agent-fabric.lifecycle.v1\0${domain}\0${canonicalJson(value)}`)
    .digest("hex")}`;
}

function object(value: unknown, label: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} is invalid`);
  return value as Record<string, unknown>;
}

function text(value: JsonRow, key: string): string {
  const field = value[key];
  if (typeof field !== "string" || field.length === 0) throw new Error(`${key} is invalid`);
  return field;
}

function integer(value: JsonRow, key: string): number {
  const field = value[key];
  if (typeof field !== "number" || !Number.isSafeInteger(field) || field < 0) throw new Error(`${key} is invalid`);
  return field;
}

function positiveInteger(value: JsonRow, key: string): number {
  const field = integer(value, key);
  if (field === 0) throw new Error(`${key} is invalid`);
  return field;
}

function digest(value: JsonRow, key: string): LifecycleDigest {
  const field = text(value, key);
  if (!/^sha256:[0-9a-f]{64}$/u.test(field)) throw new Error(`${key} is invalid`);
  return field as LifecycleDigest;
}

function nullableDigest(value: JsonRow, key: string): LifecycleDigest | null {
  return value[key] === null ? null : digest(value, key);
}

function schemaVersion(value: JsonRow): 1 {
  if (integer(value, "schemaVersion") !== 1) throw new Error("schemaVersion is invalid");
  return 1;
}

function receiptKind(value: string): LifecycleReceiptLookup["kind"] {
  switch (value) {
    case "custody-terminal":
    case "generation-loss-terminal":
    case "custody-recovery-retirement":
    case "review-adoption-decision":
    case "fresh-origin":
      return value;
    default:
      throw new Error("receipt kind is invalid");
  }
}

function parseObject(json: string, label: string): Record<string, unknown> {
  const parsed: unknown = JSON.parse(json);
  const value = object(parsed, label);
  if (canonicalJson(value) !== json) throw new Error(`${label} is not canonical JSON`);
  return value;
}

function scopeKey(projectSessionId: string, runId: string): string {
  return `${projectSessionId}\0${runId}`;
}

type OwnerVariant = "custody" | "generation-loss" | "recovery-retirement";

/**
 * Subject-kind to ownerRef-variant binding table (fail-closed).
 *
 * `appendReceipt` is a documented external append-only trust boundary
 * (`receipt-authority.ts`) exported via `src/index.ts`; a caller may relabel a
 * subject to any kind, so a subject's kind cannot be assumed producer-authored.
 * Every kind that is authenticated MUST therefore have an enforced binding row
 * here: presence = enforced, absence = admission refused. For a bound kind,
 * `receiptOwner` asserts that (1) the subject kind is bound to the ownerRef
 * variant, (2) the owner carries exactly one variant payload, and (3) the
 * variant's embedded runId/agentId match the subject's top-level identity.
 *
 * Producer census (subjects that reach `appendReceipt` — sole production caller
 * is `terminal-receipt-authority.ts:69`, gated by the `candidateKind` allowlist
 * at `terminal-receipt-authority.ts:33-40`):
 *   - custody-terminal          -> custody variant. Authored at
 *     `terminal-owner-receipt.ts:205-216` (custody branch `:278-293`); ownerRef
 *     is the custody afterRef, custodyRef embeds runId/agentId.
 *   - generation-loss-terminal  -> generation-loss variant. Authored at
 *     `terminal-owner-receipt.ts:205-216` (loss branch `:294-310`) with afterRef
 *     `terminal-owner-receipt.ts:540-548` built by `generationLossRef`
 *     (`generation-loss-repository.ts:185-198`); generationLossRef embeds
 *     runId/agentId. Re-materialised at `terminal-owner-receipt.ts:463` and
 *     `receipt-repository.ts:498`. (The prior lane wrongly deferred this kind as
 *     producerless; it has live producers today.)
 *   - review-adoption-decision  -> custody variant. Authored at
 *     `review-adoption.ts:166-185`; ownerRef is the paired custody afterRef.
 *   - custody-recovery-retirement, fresh-origin -> NO producer. Neither kind is
 *     in the `candidateKind` allowlist and no subject of either kind is
 *     constructed anywhere in src; both are refused at admission (fail closed).
 */
const OWNER_BINDINGS: Readonly<Partial<Record<
  LifecycleReceiptLookup["kind"],
  OwnerVariant
>>> = {
  "custody-terminal": "custody",
  "review-adoption-decision": "custody",
  "generation-loss-terminal": "generation-loss",
};

const OWNER_VARIANT_REF_KEYS: Readonly<Record<OwnerVariant, string>> = {
  custody: "custodyRef",
  "generation-loss": "generationLossRef",
  "recovery-retirement": "retirementRef",
};

function ownerVariant(kind: unknown): OwnerVariant {
  if (kind === "custody" || kind === "generation-loss" || kind === "recovery-retirement") return kind;
  throw new Error("receipt owner kind is invalid");
}

function assertSingleOwnerVariant(owner: Record<string, unknown>, variant: OwnerVariant): void {
  for (const [candidate, refKey] of Object.entries(OWNER_VARIANT_REF_KEYS)) {
    if (candidate !== variant && refKey in owner) throw new Error("receipt owner carries a crossed variant");
  }
}

function assertEmbeddedOwnerIdentity(subject: JsonRow, owner: Record<string, unknown>, variant: OwnerVariant): void {
  // Every bound variant (custody, generation-loss) embeds the owning
  // runId/agentId; the guard keeps the check defensive if an unbound variant
  // is ever reached.
  if (variant !== "custody" && variant !== "generation-loss") return;
  const ref = object(owner[OWNER_VARIANT_REF_KEYS[variant]], `${variant} owner`);
  if (text(ref, "runId") !== text(subject, "runId") || text(ref, "agentId") !== text(subject, "agentId")) {
    throw new Error("receipt owner identity is crossed");
  }
}

function ownerRevision(owner: Record<string, unknown>, variant: OwnerVariant): number {
  if (variant === "custody") return positiveInteger(object(owner.custodyRef, "custody owner"), "custodyRevision");
  if (variant === "generation-loss") {
    return positiveInteger(object(owner.generationLossRef, "generation-loss owner"), "generationLossRevision");
  }
  const revision = Number(text(object(owner.retirementRef, "retirement owner"), "revisionDec"));
  if (!Number.isSafeInteger(revision) || revision < 1) throw new Error("receipt owner revision is invalid");
  return revision;
}

function receiptOwner(
  subject: JsonRow,
  kind: LifecycleReceiptLookup["kind"],
): Readonly<{ digest: LifecycleDigest; revision: number }> {
  const owner = object(subject.ownerRef, "receipt owner");
  const variant = ownerVariant(owner.kind);
  const bound = OWNER_BINDINGS[kind];
  if (bound === undefined) throw new Error(`no enforced binding for kind ${kind}; admission refused`);
  if (variant !== bound) throw new Error("receipt owner binding is invalid");
  assertSingleOwnerVariant(owner, variant);
  assertEmbeddedOwnerIdentity(subject, owner, variant);
  return { digest: lifecycleDigest("receipt-owner-ref", owner), revision: ownerRevision(owner, variant) };
}

function receiptLookup(subject: JsonRow): LifecycleReceiptLookup {
  const kind = receiptKind(text(subject, "kind"));
  const owner = receiptOwner(subject, kind);
  return {
    kind,
    projectSessionId: text(subject, "projectSessionId"),
    runId: text(subject, "runId"),
    agentId: text(subject, "agentId"),
    ownerRefDigest: owner.digest,
    ownerRevision: owner.revision,
  };
}

function parseReceipt(json: string): LifecycleAuthenticatedReceipt {
  const value = parseObject(json, "receipt");
  const kind = receiptKind(text(value, "kind"));
  return {
    schemaVersion: schemaVersion(value),
    kind,
    authorityId: text(value, "authorityId"),
    authoritySequence: positiveInteger(value, "authoritySequence"),
    previousReceiptDigest: nullableDigest(value, "previousReceiptDigest"),
    subjectDigest: digest(value, "subjectDigest"),
    intentDigest: digest(value, "intentDigest"),
    receiptDigest: digest(value, "receiptDigest"),
    attestation: text(value, "attestation"),
  };
}

function parseScope(json: string): LifecycleAdmittedRunScope {
  const value = parseObject(json, "admitted scope");
  return {
    schemaVersion: schemaVersion(value),
    projectId: text(value, "projectId"),
    projectSessionId: text(value, "projectSessionId"),
    runId: text(value, "runId"),
    authorityId: text(value, "authorityId"),
    admissionDigest: digest(value, "admissionDigest"),
    admittedAt: integer(value, "admittedAt"),
  };
}

function parseScopeCheckpoint(json: string): LifecycleAuthenticatedScopeCheckpoint {
  const value = parseObject(json, "scope checkpoint");
  return {
    schemaVersion: schemaVersion(value),
    projectSessionId: text(value, "projectSessionId"),
    runId: text(value, "runId"),
    authorityId: text(value, "authorityId"),
    receiptCount: integer(value, "receiptCount"),
    headAuthoritySequence: integer(value, "headAuthoritySequence"),
    headReceiptDigest: nullableDigest(value, "headReceiptDigest"),
    orderedRecordSetDigest: digest(value, "orderedRecordSetDigest"),
    checkpointDigest: digest(value, "checkpointDigest"),
    attestation: text(value, "attestation"),
  };
}

function parseNamespaceCheckpoint(json: string): LifecycleAuthenticatedNamespaceCheckpoint {
  const value = parseObject(json, "namespace checkpoint");
  return {
    schemaVersion: schemaVersion(value),
    projectId: text(value, "projectId"),
    authorityId: text(value, "authorityId"),
    scopeCount: integer(value, "scopeCount"),
    orderedScopeHeadSetDigest: digest(value, "orderedScopeHeadSetDigest"),
    checkpointDigest: digest(value, "checkpointDigest"),
    attestation: text(value, "attestation"),
  };
}

function receiptSetMember(record: LifecycleReceiptRecord): readonly [
  string, LifecycleDigest, LifecycleDigest, string, string, string, string, string,
] {
  const subject = record.subject;
  const owner = object(subject.ownerRef, "receipt owner");
  let ownerKind: string;
  let ownerId: string;
  let ownerRevision: string;
  if (owner.kind === "custody") {
    const ref = object(owner.custodyRef, "custody owner");
    ownerKind = "custody";
    ownerId = text(ref, "custodyId");
    ownerRevision = String(positiveInteger(ref, "custodyRevision"));
  } else if (owner.kind === "generation-loss") {
    const ref = object(owner.generationLossRef, "generation-loss owner");
    ownerKind = "generation-loss";
    ownerId = text(ref, "generationLossId");
    ownerRevision = String(positiveInteger(ref, "generationLossRevision"));
  } else if (owner.kind === "recovery-retirement") {
    const ref = object(owner.retirementRef, "retirement owner");
    ownerKind = "recovery-retirement";
    ownerId = text(ref, "retirementId");
    ownerRevision = text(ref, "revisionDec");
  } else {
    throw new Error("receipt owner kind is invalid");
  }
  return [
    String(record.receipt.authoritySequence),
    record.receipt.receiptDigest,
    record.receipt.intentDigest,
    text(subject, "kind"),
    text(subject, "agentId"),
    ownerKind,
    ownerId,
    ownerRevision,
  ];
}

function scopeCheckpointBody(checkpoint: LifecycleAuthenticatedScopeCheckpoint): JsonRow {
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

function namespaceCheckpointBody(checkpoint: LifecycleAuthenticatedNamespaceCheckpoint): JsonRow {
  return {
    schemaVersion: 1,
    authorityId: checkpoint.authorityId,
    projectId: checkpoint.projectId,
    scopeCountDec: String(checkpoint.scopeCount),
    orderedScopeHeadSetDigest: checkpoint.orderedScopeHeadSetDigest,
  };
}

function assertSecurePath(path: string, kind: "directory" | "file", expectedMode: number): void {
  const stat = lstatSync(path);
  const expectedType = kind === "directory" ? stat.isDirectory() : stat.isFile();
  if (!expectedType || stat.isSymbolicLink()) throw new Error(`${path} must be a regular ${kind}`);
  if ((stat.mode & 0o777) !== expectedMode) throw new Error(`${path} must have mode ${expectedMode.toString(8)}`);
  if (typeof process.getuid === "function" && stat.uid !== process.getuid()) throw new Error(`${path} has the wrong owner`);
}

function readKey(path: string): Buffer {
  const descriptor = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const stat = fstatSync(descriptor);
    if (!stat.isFile()) throw new Error("lifecycle receipt key must be a regular file");
    const key = readFileSync(descriptor);
    if (key.length !== 32) throw new Error("lifecycle receipt key must be exactly 32 bytes");
    return key;
  } finally {
    closeSync(descriptor);
  }
}

function assertSchema(database: Database.Database): void {
  if (database.pragma("user_version", { simple: true }) !== 1) throw new Error("lifecycle receipt schema mismatch");
  const expectedSql: Readonly<Record<string, string>> = {
    authority_metadata: `CREATE TABLE authority_metadata(
      singleton INTEGER PRIMARY KEY CHECK(singleton=1),
      schema_version INTEGER NOT NULL CHECK(schema_version=1),
      authority_id TEXT NOT NULL CHECK(length(authority_id)>0)
    ) STRICT`,
    admitted_scopes: `CREATE TABLE admitted_scopes(
      project_session_id TEXT NOT NULL,
      run_id TEXT NOT NULL,
      project_id TEXT NOT NULL,
      scope_json TEXT NOT NULL,
      scope_attestation TEXT NOT NULL,
      PRIMARY KEY(project_session_id,run_id)
    ) STRICT`,
    receipts: `CREATE TABLE receipts(
      project_session_id TEXT NOT NULL,
      run_id TEXT NOT NULL,
      authority_sequence INTEGER NOT NULL CHECK(authority_sequence>0),
      kind TEXT NOT NULL,
      agent_id TEXT NOT NULL,
      owner_ref_digest TEXT NOT NULL,
      owner_revision INTEGER NOT NULL CHECK(owner_revision>0),
      intent_digest TEXT NOT NULL,
      subject_json TEXT NOT NULL,
      receipt_json TEXT NOT NULL,
      receipt_digest TEXT NOT NULL,
      PRIMARY KEY(project_session_id,run_id,authority_sequence),
      UNIQUE(kind,project_session_id,run_id,agent_id,owner_ref_digest,owner_revision),
      FOREIGN KEY(project_session_id,run_id) REFERENCES admitted_scopes(project_session_id,run_id)
    ) STRICT`,
    scope_snapshots: `CREATE TABLE scope_snapshots(
      checkpoint_digest TEXT PRIMARY KEY,
      project_session_id TEXT NOT NULL,
      run_id TEXT NOT NULL,
      max_authority_sequence INTEGER NOT NULL CHECK(max_authority_sequence>=0),
      checkpoint_json TEXT NOT NULL,
      FOREIGN KEY(project_session_id,run_id) REFERENCES admitted_scopes(project_session_id,run_id)
    ) STRICT`,
    namespace_snapshots: `CREATE TABLE namespace_snapshots(
      checkpoint_digest TEXT PRIMARY KEY,
      project_id TEXT NOT NULL,
      checkpoint_json TEXT NOT NULL
    ) STRICT`,
    namespace_snapshot_members: `CREATE TABLE namespace_snapshot_members(
      checkpoint_digest TEXT NOT NULL,
      member_order INTEGER NOT NULL CHECK(member_order>=0),
      scope_checkpoint_digest TEXT NOT NULL,
      PRIMARY KEY(checkpoint_digest,member_order),
      FOREIGN KEY(checkpoint_digest) REFERENCES namespace_snapshots(checkpoint_digest),
      FOREIGN KEY(scope_checkpoint_digest) REFERENCES scope_snapshots(checkpoint_digest)
    ) STRICT`,
  };
  const normalizeSql = (sql: string): string => sql.replaceAll(/\s+/gu, "").toLowerCase();
  const objects = database.prepare(`
    SELECT type,name,tbl_name,sql FROM sqlite_schema
     WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name
  `).all().map((entry) => {
    const value = entry as { type: unknown; name: unknown; tbl_name: unknown; sql: unknown };
    return {
      type: String(value.type),
      name: String(value.name),
      table: String(value.tbl_name),
      sql: typeof value.sql === "string" ? normalizeSql(value.sql) : null,
    };
  });
  const expectedObjects = Object.entries(expectedSql).sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)
    .map(([name, sql]) => ({ type: "table", name, table: name, sql: normalizeSql(sql) }));
  if (canonicalJson(objects) !== canonicalJson(expectedObjects)) {
    throw new Error("lifecycle receipt schema mismatch");
  }
}

export class LocalLifecycleReceiptAuthority implements LifecycleIntegrityReceiptAuthorityPort {
  readonly authorityId: string;
  readonly #database: Database.Database;
  readonly #key: Buffer;

  constructor(options: LocalLifecycleReceiptAuthorityOptions) {
    if (options.expectedAuthorityId.length === 0) throw new Error("expected authority ID is required");
    assertSecurePath(options.stateDirectory, "directory", 0o700);
    const databasePath = join(options.stateDirectory, DATABASE_NAME);
    const keyPath = join(options.stateDirectory, KEY_NAME);
    assertSecurePath(databasePath, "file", 0o600);
    assertSecurePath(keyPath, "file", 0o600);
    this.#key = readKey(keyPath);
    this.#database = new Database(databasePath, { fileMustExist: true });
    try {
      this.#database.pragma("trusted_schema = OFF");
      this.#database.pragma("foreign_keys = ON");
      this.#database.pragma("synchronous = FULL");
      this.#database.pragma("journal_mode = DELETE");
      assertSchema(this.#database);
      const metadata = this.#database.prepare(`SELECT schema_version,authority_id FROM authority_metadata WHERE singleton=1`).get() as { schema_version: unknown; authority_id: unknown } | undefined;
      if (metadata?.schema_version !== 1 || metadata.authority_id !== options.expectedAuthorityId) {
        throw new Error("lifecycle receipt authority identity mismatch");
      }
      this.authorityId = options.expectedAuthorityId;
      this.#validateLedger();
    } catch (error: unknown) {
      this.#database.close();
      this.#key.fill(0);
      throw error;
    }
  }

  close(): void {
    this.#database.close();
    this.#key.fill(0);
  }

  async admitScope(scope: LifecycleAdmittedRunScope): Promise<LifecycleAuthenticatedScopeCheckpoint> {
    if (scope.schemaVersion !== 1 || scope.authorityId !== this.authorityId) throw new Error("scope authority crossed");
    return this.#database.transaction(() => {
      const existing = this.#database.prepare(`SELECT project_id,scope_json,scope_attestation FROM admitted_scopes WHERE project_session_id=? AND run_id=?`)
        .get(scope.projectSessionId, scope.runId) as StoredScopeRow | undefined;
      const scopeJson = canonicalJson(scope);
      if (existing !== undefined) {
        if (existing.project_id !== scope.projectId || existing.scope_json !== scopeJson || !this.#validScope(scope, existing.scope_attestation)) throw new Error("scope admission conflict");
        return this.#initialScopeCheckpoint(scope.projectSessionId, scope.runId);
      }
      this.#database.prepare(`INSERT INTO admitted_scopes(project_session_id,run_id,project_id,scope_json,scope_attestation) VALUES(?,?,?,?,?)`)
        .run(scope.projectSessionId, scope.runId, scope.projectId, scopeJson, this.#attest("scope-admission", lifecycleDigest("scope-admission", scope)));
      const checkpoint = this.#createScopeCheckpoint(scope.projectSessionId, scope.runId);
      this.#createNamespaceCheckpoint(scope.projectId);
      return checkpoint;
    })();
  }

  async appendReceipt(intentDigest: LifecycleDigest, subject: JsonRow): Promise<LifecycleAuthenticatedReceipt> {
    const lookup = receiptLookup(subject);
    return this.#database.transaction(() => {
      const existing = this.#findReceipt(lookup);
      if (existing !== null) {
        if (existing.receipt.intentDigest !== intentDigest || canonicalJson(existing.subject) !== canonicalJson(subject)) {
          throw new Error("receipt lookup conflict");
        }
        return existing.receipt;
      }
      const scope = this.#requiredScope(lookup.projectSessionId, lookup.runId);
      const previous = this.#database.prepare(`SELECT receipt_json FROM receipts WHERE project_session_id=? AND run_id=? ORDER BY authority_sequence DESC LIMIT 1`)
        .get(lookup.projectSessionId, lookup.runId) as { receipt_json: string } | undefined;
      const previousReceipt = previous === undefined ? null : parseReceipt(previous.receipt_json);
      const body = {
        schemaVersion: 1 as const,
        kind: lookup.kind,
        authorityId: this.authorityId,
        authoritySequence: (previousReceipt?.authoritySequence ?? 0) + 1,
        previousReceiptDigest: previousReceipt?.receiptDigest ?? null,
        intentDigest,
        subjectDigest: lifecycleDigest("receipt-subject", subject),
      };
      const receiptDigest = lifecycleDigest("authenticated-receipt", body);
      const receipt: LifecycleAuthenticatedReceipt = {
        ...body,
        receiptDigest,
        attestation: this.#attest("receipt", receiptDigest),
      };
      this.#database.prepare(`
        INSERT INTO receipts(project_session_id,run_id,authority_sequence,kind,agent_id,owner_ref_digest,owner_revision,intent_digest,subject_json,receipt_json,receipt_digest)
        VALUES(?,?,?,?,?,?,?,?,?,?,?)
      `).run(
        lookup.projectSessionId, lookup.runId, receipt.authoritySequence, lookup.kind, lookup.agentId,
        lookup.ownerRefDigest, lookup.ownerRevision, intentDigest, canonicalJson(subject), canonicalJson(receipt), receiptDigest,
      );
      this.#createScopeCheckpoint(lookup.projectSessionId, lookup.runId);
      this.#createNamespaceCheckpoint(scope.project_id);
      return receipt;
    })();
  }

  async readReceipt(lookup: LifecycleReceiptLookup): Promise<LifecycleReceiptRecord | null> {
    return this.#findReceipt(lookup);
  }

  async readScopeCheckpoint(projectSessionId: string, runId: string): Promise<LifecycleAuthenticatedScopeCheckpoint> {
    this.#requiredScope(projectSessionId, runId);
    return this.#currentScopeCheckpoint(projectSessionId, runId);
  }

  async readScopeCheckpointAt(checkpointDigest: LifecycleDigest): Promise<LifecycleAuthenticatedScopeCheckpoint> {
    const row = this.#database.prepare(`SELECT project_session_id,run_id,checkpoint_json,max_authority_sequence FROM scope_snapshots WHERE checkpoint_digest=?`)
      .get(checkpointDigest) as StoredSnapshotRow | undefined;
    if (row === undefined) throw new Error("unknown pinned scope checkpoint");
    const checkpoint = parseScopeCheckpoint(row.checkpoint_json);
    if (!this.#validScopeSnapshot(row, checkpoint)) throw new Error("scope checkpoint membership or authentication failed");
    return checkpoint;
  }

  async readScopePageAt(checkpointDigest: LifecycleDigest, afterAuthoritySequence: number, limit = PAGE_LIMIT): Promise<Readonly<{ orderedRecords: readonly LifecycleReceiptRecord[]; nextAfter: number | null }>> {
    if (!Number.isSafeInteger(afterAuthoritySequence) || afterAuthoritySequence < 0) throw new Error("invalid page cursor");
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > PAGE_LIMIT) throw new Error("invalid page limit");
    const row = this.#database.prepare(`SELECT project_session_id,run_id,max_authority_sequence,checkpoint_json FROM scope_snapshots WHERE checkpoint_digest=?`)
      .get(checkpointDigest) as (StoredSnapshotRow & { project_session_id: string; run_id: string }) | undefined;
    if (row === undefined || !this.#validScopeCheckpoint(parseScopeCheckpoint(row.checkpoint_json), row.max_authority_sequence)) throw new Error("unknown or invalid pinned scope checkpoint");
    const records = this.#records(row.project_session_id, row.run_id, row.max_authority_sequence)
      .filter((record) => record.receipt.authoritySequence > afterAuthoritySequence)
      .slice(0, limit);
    const last = records.at(-1)?.receipt.authoritySequence ?? afterAuthoritySequence;
    return { orderedRecords: records, nextAfter: last < row.max_authority_sequence ? last : null };
  }

  async readNamespaceCheckpoint(projectId: string): Promise<LifecycleAuthenticatedNamespaceCheckpoint> {
    return this.#database.transaction(() => this.#createNamespaceCheckpoint(projectId))();
  }

  async readNamespacePageAt(checkpointDigest: LifecycleDigest, afterScopeKey: string | null, limit = PAGE_LIMIT): Promise<Readonly<{ orderedScopeHeads: readonly LifecycleAuthenticatedScopeCheckpoint[]; nextAfter: string | null }>> {
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > PAGE_LIMIT) throw new Error("invalid page limit");
    const namespace = this.#database.prepare(`SELECT project_id,checkpoint_json FROM namespace_snapshots WHERE checkpoint_digest=?`)
      .get(checkpointDigest) as StoredNamespaceRow | undefined;
    if (namespace === undefined) throw new Error("unknown pinned namespace checkpoint");
    const checkpoint = parseNamespaceCheckpoint(namespace.checkpoint_json);
    if (!this.#validNamespaceSnapshot(namespace, checkpoint)) throw new Error("namespace checkpoint authentication failed");
    const members = this.#database.prepare(`
      SELECT s.checkpoint_json
        FROM namespace_snapshot_members m
        JOIN scope_snapshots s ON s.checkpoint_digest=m.scope_checkpoint_digest
       WHERE m.checkpoint_digest=? ORDER BY m.member_order
    `).all(checkpointDigest) as Array<{ checkpoint_json: string }>;
    const heads = members.map((member) => parseScopeCheckpoint(member.checkpoint_json));
    const start = afterScopeKey === null ? 0 : heads.findIndex((head) => scopeKey(head.projectSessionId, head.runId) > afterScopeKey);
    if (start < 0) return { orderedScopeHeads: [], nextAfter: null };
    const orderedScopeHeads = heads.slice(start, start + limit);
    const end = start + orderedScopeHeads.length;
    return {
      orderedScopeHeads,
      nextAfter: end < heads.length ? scopeKey(orderedScopeHeads.at(-1)!.projectSessionId, orderedScopeHeads.at(-1)!.runId) : null,
    };
  }

  async verifyReceipt(subject: JsonRow, receipt: LifecycleAuthenticatedReceipt): Promise<boolean> {
    try {
      if (!this.#validReceipt(subject, receipt)) return false;
      const stored = this.#database.prepare(`SELECT subject_json,receipt_json FROM receipts WHERE receipt_digest=?`)
        .get(receipt.receiptDigest) as StoredReceiptRow | undefined;
      return stored !== undefined && stored.subject_json === canonicalJson(subject) && stored.receipt_json === canonicalJson(receipt);
    } catch {
      return false;
    }
  }

  async verifyScopeCheckpoint(checkpoint: LifecycleAuthenticatedScopeCheckpoint): Promise<boolean> {
    const row = this.#database.prepare(`SELECT project_session_id,run_id,checkpoint_json,max_authority_sequence FROM scope_snapshots WHERE checkpoint_digest=?`)
      .get(checkpoint.checkpointDigest) as StoredSnapshotRow | undefined;
    return row !== undefined && row.checkpoint_json === canonicalJson(checkpoint) && this.#validScopeSnapshot(row, checkpoint);
  }

  async verifyNamespaceCheckpoint(checkpoint: LifecycleAuthenticatedNamespaceCheckpoint): Promise<boolean> {
    const row = this.#database.prepare(`SELECT project_id,checkpoint_json FROM namespace_snapshots WHERE checkpoint_digest=?`)
      .get(checkpoint.checkpointDigest) as StoredNamespaceRow | undefined;
    return row !== undefined && row.checkpoint_json === canonicalJson(checkpoint) && this.#validNamespaceSnapshot(row, checkpoint);
  }

  #attest(domain: string, authenticatedDigest: LifecycleDigest): string {
    return `hmac-sha256:${createHmac("sha256", this.#key).update(`agent-fabric.lifecycle.local.v1\0${domain}\0${authenticatedDigest}`).digest("hex")}`;
  }

  #attestationMatches(domain: string, authenticatedDigest: LifecycleDigest, actual: string): boolean {
    const expected = this.#attest(domain, authenticatedDigest);
    const left = Buffer.from(expected);
    const right = Buffer.from(actual);
    return left.length === right.length && timingSafeEqual(left, right);
  }

  #requiredScope(projectSessionId: string, runId: string): StoredScopeRow {
    const scope = this.#database.prepare(`SELECT project_id,scope_json,scope_attestation FROM admitted_scopes WHERE project_session_id=? AND run_id=?`)
      .get(projectSessionId, runId) as StoredScopeRow | undefined;
    if (scope === undefined) throw new Error("scope is not admitted");
    const parsed = parseScope(scope.scope_json);
    if (parsed.authorityId !== this.authorityId || parsed.projectSessionId !== projectSessionId || parsed.runId !== runId || parsed.projectId !== scope.project_id) throw new Error("scope membership mismatch");
    if (!this.#validScope(parsed, scope.scope_attestation)) throw new Error("scope authentication failed");
    return scope;
  }

  #validScope(scope: LifecycleAdmittedRunScope, attestation: string): boolean {
    return scope.schemaVersion === 1 && scope.authorityId === this.authorityId &&
      this.#attestationMatches("scope-admission", lifecycleDigest("scope-admission", scope), attestation);
  }

  #findReceipt(lookup: LifecycleReceiptLookup): LifecycleReceiptRecord | null {
    const row = this.#database.prepare(`
      SELECT subject_json,receipt_json FROM receipts
       WHERE kind=? AND project_session_id=? AND run_id=? AND agent_id=? AND owner_ref_digest=? AND owner_revision=?
    `).get(lookup.kind, lookup.projectSessionId, lookup.runId, lookup.agentId, lookup.ownerRefDigest, lookup.ownerRevision) as StoredReceiptRow | undefined;
    if (row === undefined) return null;
    const subject = parseObject(row.subject_json, "receipt subject");
    const receipt = parseReceipt(row.receipt_json);
    if (canonicalJson(receiptLookup(subject)) !== canonicalJson(lookup) || !this.#validReceipt(subject, receipt)) throw new Error("receipt membership mismatch");
    return { subject, receipt };
  }

  #validReceipt(subject: JsonRow, receipt: LifecycleAuthenticatedReceipt): boolean {
    const body = {
      schemaVersion: 1 as const,
      kind: receipt.kind,
      authorityId: receipt.authorityId,
      authoritySequence: receipt.authoritySequence,
      previousReceiptDigest: receipt.previousReceiptDigest,
      intentDigest: receipt.intentDigest,
      subjectDigest: receipt.subjectDigest,
    };
    return receipt.schemaVersion === 1 && receipt.authorityId === this.authorityId &&
      receipt.subjectDigest === lifecycleDigest("receipt-subject", subject) &&
      receipt.receiptDigest === lifecycleDigest("authenticated-receipt", body) &&
      this.#attestationMatches("receipt", receipt.receiptDigest, receipt.attestation);
  }

  #records(projectSessionId: string, runId: string, maximum = Number.MAX_SAFE_INTEGER): LifecycleReceiptRecord[] {
    const rows = this.#database.prepare(`
      SELECT kind,project_session_id,run_id,agent_id,owner_ref_digest,owner_revision,subject_json,receipt_json FROM receipts
       WHERE project_session_id=? AND run_id=? AND authority_sequence<=? ORDER BY authority_sequence
    `).all(projectSessionId, runId, maximum) as StoredReceiptLedgerRow[];
    let previous: LifecycleAuthenticatedReceipt | null = null;
    return rows.map((row, index) => {
      const subject = parseObject(row.subject_json, "receipt subject");
      const receipt = parseReceipt(row.receipt_json);
      if (!this.#validReceipt(subject, receipt) || receipt.authoritySequence !== index + 1 || receipt.previousReceiptDigest !== (previous?.receiptDigest ?? null)) throw new Error("lifecycle receipt chain is invalid");
      const lookup = receiptLookup(subject);
      if (
        lookup.kind !== receipt.kind || row.kind !== lookup.kind ||
        row.project_session_id !== lookup.projectSessionId || row.run_id !== lookup.runId ||
        row.agent_id !== lookup.agentId || row.owner_ref_digest !== lookup.ownerRefDigest ||
        row.owner_revision !== lookup.ownerRevision
      ) throw new Error("receipt membership mismatch");
      previous = receipt;
      return { subject, receipt };
    });
  }

  #createScopeCheckpoint(projectSessionId: string, runId: string): LifecycleAuthenticatedScopeCheckpoint {
    this.#requiredScope(projectSessionId, runId);
    const records = this.#records(projectSessionId, runId);
    const body = {
      schemaVersion: 1,
      authorityId: this.authorityId,
      projectSessionId,
      runId,
      receiptCountDec: String(records.length),
      headAuthoritySequenceDec: String(records.length),
      headReceiptDigest: records.at(-1)?.receipt.receiptDigest ?? null,
      orderedRecordSetDigest: lifecycleDigest("scope-record-set", records.map(receiptSetMember)),
    };
    const checkpointDigest = lifecycleDigest("scope-checkpoint", body);
    const checkpoint: LifecycleAuthenticatedScopeCheckpoint = {
      schemaVersion: 1,
      projectSessionId,
      runId,
      authorityId: this.authorityId,
      receiptCount: records.length,
      headAuthoritySequence: records.length,
      headReceiptDigest: body.headReceiptDigest,
      orderedRecordSetDigest: body.orderedRecordSetDigest,
      checkpointDigest,
      attestation: this.#attest("scope-checkpoint", checkpointDigest),
    };
    this.#database.prepare(`INSERT OR IGNORE INTO scope_snapshots(checkpoint_digest,project_session_id,run_id,max_authority_sequence,checkpoint_json) VALUES(?,?,?,?,?)`)
      .run(checkpointDigest, projectSessionId, runId, records.length, canonicalJson(checkpoint));
    return checkpoint;
  }

  #currentScopeCheckpoint(projectSessionId: string, runId: string): LifecycleAuthenticatedScopeCheckpoint {
    const row = this.#database.prepare(`SELECT project_session_id,run_id,checkpoint_json,max_authority_sequence FROM scope_snapshots WHERE project_session_id=? AND run_id=? ORDER BY max_authority_sequence DESC LIMIT 1`)
      .get(projectSessionId, runId) as StoredSnapshotRow | undefined;
    if (row === undefined) throw new Error("scope checkpoint is absent");
    const checkpoint = parseScopeCheckpoint(row.checkpoint_json);
    if (!this.#validScopeSnapshot(row, checkpoint)) throw new Error("scope checkpoint membership or authentication failed");
    return checkpoint;
  }

  #initialScopeCheckpoint(projectSessionId: string, runId: string): LifecycleAuthenticatedScopeCheckpoint {
    const row = this.#database.prepare(`SELECT project_session_id,run_id,checkpoint_json,max_authority_sequence FROM scope_snapshots WHERE project_session_id=? AND run_id=? AND max_authority_sequence=0`)
      .get(projectSessionId, runId) as StoredSnapshotRow | undefined;
    if (row === undefined) throw new Error("initial scope checkpoint is absent");
    const checkpoint = parseScopeCheckpoint(row.checkpoint_json);
    if (!this.#validScopeSnapshot(row, checkpoint)) throw new Error("scope checkpoint membership or authentication failed");
    return checkpoint;
  }

  #validScopeSnapshot(row: StoredSnapshotRow, checkpoint: LifecycleAuthenticatedScopeCheckpoint): boolean {
    return row.project_session_id === checkpoint.projectSessionId && row.run_id === checkpoint.runId &&
      this.#validScopeCheckpoint(checkpoint, row.max_authority_sequence);
  }

  #validScopeCheckpoint(checkpoint: LifecycleAuthenticatedScopeCheckpoint, maximum: number): boolean {
    try {
      const records = this.#records(checkpoint.projectSessionId, checkpoint.runId, maximum);
      return checkpoint.schemaVersion === 1 && checkpoint.authorityId === this.authorityId &&
        checkpoint.receiptCount === records.length && checkpoint.headAuthoritySequence === records.length &&
        checkpoint.headReceiptDigest === (records.at(-1)?.receipt.receiptDigest ?? null) &&
        checkpoint.orderedRecordSetDigest === lifecycleDigest("scope-record-set", records.map(receiptSetMember)) &&
        checkpoint.checkpointDigest === lifecycleDigest("scope-checkpoint", scopeCheckpointBody(checkpoint)) &&
        this.#attestationMatches("scope-checkpoint", checkpoint.checkpointDigest, checkpoint.attestation);
    } catch {
      return false;
    }
  }

  #createNamespaceCheckpoint(projectId: string): LifecycleAuthenticatedNamespaceCheckpoint {
    const { checkpoint, heads } = this.#buildNamespaceCheckpoint(projectId);
    const checkpointDigest = checkpoint.checkpointDigest;
    this.#database.prepare(`INSERT OR IGNORE INTO namespace_snapshots(checkpoint_digest,project_id,checkpoint_json) VALUES(?,?,?)`)
      .run(checkpointDigest, projectId, canonicalJson(checkpoint));
    const insert = this.#database.prepare(`INSERT OR IGNORE INTO namespace_snapshot_members(checkpoint_digest,member_order,scope_checkpoint_digest) VALUES(?,?,?)`);
    heads.forEach((head, index) => insert.run(checkpointDigest, index, head.checkpointDigest));
    return checkpoint;
  }

  #buildNamespaceCheckpoint(projectId: string): Readonly<{
    checkpoint: LifecycleAuthenticatedNamespaceCheckpoint;
    heads: readonly LifecycleAuthenticatedScopeCheckpoint[];
  }> {
    const scopes = this.#database.prepare(`SELECT project_session_id,run_id FROM admitted_scopes WHERE project_id=? ORDER BY project_session_id,run_id`)
      .all(projectId) as Array<{ project_session_id: string; run_id: string }>;
    const heads = scopes.map((scope) => this.#currentScopeCheckpoint(scope.project_session_id, scope.run_id));
    const members = heads.map((head) => ({
      projectSessionId: head.projectSessionId,
      runId: head.runId,
      authorityId: head.authorityId,
      scopeCheckpointDigest: head.checkpointDigest,
      receiptCountDec: String(head.receiptCount),
      headReceiptDigest: head.headReceiptDigest,
    }));
    const body = {
      schemaVersion: 1,
      authorityId: this.authorityId,
      projectId,
      scopeCountDec: String(heads.length),
      orderedScopeHeadSetDigest: lifecycleDigest("namespace-scope-head-set", members),
    };
    const checkpointDigest = lifecycleDigest("namespace-checkpoint", body);
    const checkpoint: LifecycleAuthenticatedNamespaceCheckpoint = {
      schemaVersion: 1,
      projectId,
      authorityId: this.authorityId,
      scopeCount: heads.length,
      orderedScopeHeadSetDigest: body.orderedScopeHeadSetDigest,
      checkpointDigest,
      attestation: this.#attest("namespace-checkpoint", checkpointDigest),
    };
    return { checkpoint, heads };
  }

  #validNamespaceCheckpoint(checkpoint: LifecycleAuthenticatedNamespaceCheckpoint): boolean {
    try {
      const rows = this.#database.prepare(`
        SELECT s.project_session_id,s.run_id,s.checkpoint_json,s.max_authority_sequence
          FROM namespace_snapshot_members m
          JOIN scope_snapshots s ON s.checkpoint_digest=m.scope_checkpoint_digest
         WHERE m.checkpoint_digest=? ORDER BY m.member_order
      `).all(checkpoint.checkpointDigest) as Array<StoredSnapshotRow>;
      const heads = rows.map((row) => parseScopeCheckpoint(row.checkpoint_json));
      if (heads.some((head, index) => !this.#validScopeSnapshot(rows[index]!, head))) return false;
      const members = heads.map((head) => ({
        projectSessionId: head.projectSessionId,
        runId: head.runId,
        authorityId: head.authorityId,
        scopeCheckpointDigest: head.checkpointDigest,
        receiptCountDec: String(head.receiptCount),
        headReceiptDigest: head.headReceiptDigest,
      }));
      return checkpoint.schemaVersion === 1 && checkpoint.authorityId === this.authorityId &&
        checkpoint.scopeCount === heads.length &&
        checkpoint.orderedScopeHeadSetDigest === lifecycleDigest("namespace-scope-head-set", members) &&
        checkpoint.checkpointDigest === lifecycleDigest("namespace-checkpoint", namespaceCheckpointBody(checkpoint)) &&
        this.#attestationMatches("namespace-checkpoint", checkpoint.checkpointDigest, checkpoint.attestation);
    } catch {
      return false;
    }
  }

  #validNamespaceSnapshot(row: StoredNamespaceRow, checkpoint: LifecycleAuthenticatedNamespaceCheckpoint): boolean {
    return row.project_id === checkpoint.projectId && this.#validNamespaceCheckpoint(checkpoint);
  }

  #validateLedger(): void {
    const integrity = this.#database.pragma("integrity_check", { simple: true });
    if (integrity !== "ok") throw new Error("lifecycle receipt database integrity failed");
    const foreignKeyFailures = this.#database.pragma("foreign_key_check") as unknown[];
    if (foreignKeyFailures.length !== 0) {
      throw new Error("lifecycle receipt membership mismatch");
    }
    const scopes = this.#database.prepare(`SELECT project_session_id,run_id,project_id,scope_json,scope_attestation FROM admitted_scopes ORDER BY project_session_id,run_id`)
      .all() as Array<{ project_session_id: string; run_id: string; project_id: string; scope_json: string; scope_attestation: string }>;
    for (const row of scopes) {
      const scope = parseScope(row.scope_json);
      if (scope.authorityId !== this.authorityId || scope.projectSessionId !== row.project_session_id || scope.runId !== row.run_id || scope.projectId !== row.project_id) throw new Error("scope membership mismatch");
      if (!this.#validScope(scope, row.scope_attestation)) throw new Error("scope authentication failed");
      const records = this.#records(row.project_session_id, row.run_id);
      const initial = this.#initialScopeCheckpoint(row.project_session_id, row.run_id);
      const current = this.#currentScopeCheckpoint(row.project_session_id, row.run_id);
      if (initial.receiptCount !== 0 || current.receiptCount !== records.length) {
        throw new Error("lifecycle receipt membership mismatch");
      }
    }
    const scopeSnapshots = this.#database.prepare(`SELECT project_session_id,run_id,checkpoint_json,max_authority_sequence FROM scope_snapshots`).all() as StoredSnapshotRow[];
    if (scopeSnapshots.some((row) => !this.#validScopeSnapshot(row, parseScopeCheckpoint(row.checkpoint_json)))) throw new Error("scope checkpoint membership or authentication failed");
    const namespaceSnapshots = this.#database.prepare(`SELECT project_id,checkpoint_json FROM namespace_snapshots`).all() as StoredNamespaceRow[];
    if (namespaceSnapshots.some((row) => !this.#validNamespaceSnapshot(row, parseNamespaceCheckpoint(row.checkpoint_json)))) throw new Error("namespace checkpoint authentication failed");
    const projects = [...new Set(scopes.map((scope) => scope.project_id))];
    for (const projectId of projects) {
      const expected = this.#buildNamespaceCheckpoint(projectId).checkpoint;
      const stored = this.#database.prepare(`SELECT project_id,checkpoint_json FROM namespace_snapshots WHERE checkpoint_digest=?`)
        .get(expected.checkpointDigest) as StoredNamespaceRow | undefined;
      if (stored === undefined || stored.checkpoint_json !== canonicalJson(expected) || !this.#validNamespaceSnapshot(stored, expected)) {
        throw new Error("lifecycle receipt membership mismatch");
      }
    }
  }
}

export function openLocalLifecycleReceiptAuthority(options: LocalLifecycleReceiptAuthorityOptions): LocalLifecycleReceiptAuthority {
  return new LocalLifecycleReceiptAuthority(options);
}
