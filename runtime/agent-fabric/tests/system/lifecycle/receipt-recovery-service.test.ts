import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

import Database from "better-sqlite3";
import { describe, expect, it } from "vitest";

import {
  LifecycleReceiptAuthorityUnavailableError,
  LifecycleReceiptRecoveryError,
  LifecycleReceiptRecoveryService,
} from "../../../src/lifecycle/receipt-recovery.ts";
import type {
  LifecycleAdmittedRunScope,
  LifecycleAuthenticatedNamespaceCheckpoint,
  LifecycleAuthenticatedReceipt,
  LifecycleAuthenticatedScopeCheckpoint,
  LifecycleDigest,
} from "../../../src/lifecycle/receipt-authority.ts";
import { canonicalJson } from "../../../src/persistence/row-codec.ts";
import { TestLifecycleReceiptAuthority } from "../../support/lifecycle-receipt-authority-fake.ts";

function digest(domain: string, value: unknown): LifecycleDigest {
  return `sha256:${createHash("sha256")
    .update(`agent-fabric.lifecycle.v1\0${domain}\0${canonicalJson(value)}`)
    .digest("hex")}`;
}

type Seed = Readonly<{
  projectId: string;
  scope: LifecycleAdmittedRunScope;
  initial: LifecycleAuthenticatedScopeCheckpoint;
  namespace: LifecycleAuthenticatedNamespaceCheckpoint;
}>;

async function seededScope(): Promise<Readonly<{
  database: Database.Database;
  authority: TestLifecycleReceiptAuthority;
  seed: Seed;
}>> {
  const database = new Database(":memory:");
  database.exec(readFileSync(new URL("../../../migrations/0001-current-baseline.sql", import.meta.url), "utf8"));
  database.pragma("foreign_keys = OFF");
  const authority = new TestLifecycleReceiptAuthority();
  const projectId = "project-recovery";
  const scope: LifecycleAdmittedRunScope = {
    schemaVersion: 1,
    projectId,
    projectSessionId: "session-recovery",
    runId: "run-recovery",
    authorityId: authority.authorityId,
    admissionDigest: digest("admission", { projectId, runId: "run-recovery" }),
    admittedAt: 10,
  };
  const initial = await authority.admitScope(scope);
  const namespace = await authority.readNamespaceCheckpoint(projectId);
  seedLocalScope(database, { projectId, scope, initial, namespace });
  return { database, authority, seed: { projectId, scope, initial, namespace } };
}

function seedLocalScope(database: Database.Database, seed: Seed): void {
  const { projectId, scope, initial, namespace } = seed;
  const scopeJson = canonicalJson(scope);
  const scopeDigest = digest("admitted-scope", scope);
  const requestId = digest("scope-admission-outbox", { schemaVersion: 1, scopeDigest });
  const member = {
    projectSessionId: scope.projectSessionId,
    runId: scope.runId,
    authorityId: scope.authorityId,
    scopeCheckpointDigest: initial.checkpointDigest,
    receiptCountDec: "0",
    headReceiptDigest: null,
  };
  const resolution = {
    schemaVersion: 1,
    admissionRequestId: requestId,
    scopeDigest,
    initialScopeCheckpoint: initial,
    namespaceCheckpointDigest: namespace.checkpointDigest,
    namespaceMember: member,
    verifiedAt: 10,
  };
  const resolutionDigest = digest("scope-admission-resolution", resolution);
  const initialBody = {
    schemaVersion: 1,
    authorityId: initial.authorityId,
    projectSessionId: initial.projectSessionId,
    runId: initial.runId,
    receiptCountDec: String(initial.receiptCount),
    headAuthoritySequenceDec: String(initial.headAuthoritySequence),
    headReceiptDigest: initial.headReceiptDigest,
    orderedRecordSetDigest: initial.orderedRecordSetDigest,
  };
  const namespaceBody = {
    schemaVersion: 1,
    authorityId: namespace.authorityId,
    projectId: namespace.projectId,
    scopeCountDec: String(namespace.scopeCount),
    orderedScopeHeadSetDigest: namespace.orderedScopeHeadSetDigest,
  };
  database.prepare(`INSERT INTO lifecycle_scope_admission_outbox VALUES (?,?,?,?,?,?,?,?,?,?)`).run(
    requestId, projectId, scope.projectSessionId, scope.runId, scope.authorityId,
    scope.admissionDigest, scope.admittedAt, scopeJson, scopeDigest, 10,
  );
  database.prepare(`INSERT INTO lifecycle_admitted_run_scopes VALUES (?,?,?,?,?,?,?,?,?,?)`).run(
    projectId, scope.projectSessionId, scope.runId, scope.authorityId, scope.admissionDigest,
    scope.admittedAt, requestId, scopeDigest, initial.checkpointDigest, resolutionDigest,
  );
  database.prepare(`INSERT INTO lifecycle_receipt_scope_checkpoints VALUES (?,?,?,?,?,?,?,?,?,?,?)`).run(
    scope.projectSessionId, scope.runId, scope.authorityId, 0, 0, null,
    initial.orderedRecordSetDigest, canonicalJson(initialBody), initial.checkpointDigest,
    initial.attestation, 10,
  );
  database.prepare(`INSERT INTO lifecycle_receipt_scope_heads VALUES (?,?,?,1)`).run(
    scope.projectSessionId, scope.runId, initial.checkpointDigest,
  );
  database.prepare(`INSERT INTO lifecycle_receipt_namespace_checkpoints VALUES (?,?,?,?,?,?,?,?)`).run(
    projectId, namespace.authorityId, namespace.scopeCount, namespace.orderedScopeHeadSetDigest,
    canonicalJson(namespaceBody), namespace.checkpointDigest, namespace.attestation, 10,
  );
  database.prepare(`INSERT INTO lifecycle_receipt_namespace_members VALUES (?,?,?,?,?,?,?,?,?)`).run(
    projectId, namespace.checkpointDigest, 1, scope.projectSessionId, scope.runId,
    scope.authorityId, initial.checkpointDigest, 0, null,
  );
  database.prepare(`INSERT INTO lifecycle_receipt_namespace_heads VALUES (?,?,?,?,?,1)`).run(
    projectId, namespace.authorityId, namespace.scopeCount,
    namespace.orderedScopeHeadSetDigest, namespace.checkpointDigest,
  );
  database.prepare(`INSERT INTO lifecycle_scope_admission_resolutions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`).run(
    requestId, projectId, scope.projectSessionId, scope.runId, scope.authorityId,
    scope.admissionDigest, scope.admittedAt, scopeDigest, 0, 0, initial.orderedRecordSetDigest,
    canonicalJson(initialBody), initial.checkpointDigest, 1, namespace.checkpointDigest,
    canonicalJson(member), 10, canonicalJson(resolution), resolutionDigest,
  );
}

function custodySubject(seed: Seed, custodyId: string) {
  return {
    schemaVersion: 1,
    kind: "custody-terminal",
    projectSessionId: seed.scope.projectSessionId,
    runId: seed.scope.runId,
    agentId: "chair",
    ownerRef: {
      kind: "custody",
      custodyRef: {
        schemaVersion: 1,
        runId: seed.scope.runId,
        agentId: "chair",
        custodyId,
        custodyRevision: 1,
      },
      sourceRefDigest: digest("test-owner", custodyId),
    },
  } as const;
}

function seedIntent(
  database: Database.Database,
  seed: Seed,
  subject: ReturnType<typeof custodySubject>,
  intentDigest: LifecycleDigest,
): void {
  database.prepare(`
    INSERT INTO lifecycle_receipt_intents VALUES (
      ?,1,'custody-terminal',1,'none','custody-terminal',?,?,?,'custody',?,1,
      ?,NULL,NULL,NULL,NULL,?,?,?,10
    )
  `).run(
    `batch:${subject.ownerRef.custodyRef.custodyId}`,
    seed.scope.projectSessionId,
    seed.scope.runId,
    "chair",
    subject.ownerRef.custodyRef.custodyId,
    digest("effect", subject.ownerRef.custodyRef.custodyId),
    canonicalJson(subject),
    digest("receipt-subject", subject),
    intentDigest,
  );
}

function seedCommittedReceiptWithoutAuthority(
  database: Database.Database,
  seed: Seed,
  subject: ReturnType<typeof custodySubject>,
  intentDigest: LifecycleDigest,
): void {
  const subjectDigest = digest("receipt-subject", subject);
  const receipt = {
    schemaVersion: 1,
    kind: "custody-terminal",
    authorityId: seed.scope.authorityId,
    authoritySequence: 1,
    previousReceiptDigest: null,
    subjectDigest,
    intentDigest,
    receiptDigest: digest("authenticated-receipt", { missing: intentDigest }),
    attestation: "missing-authority-receipt",
  } as const;
  database.prepare(`INSERT INTO lifecycle_authority_receipts VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`).run(
    intentDigest, `batch:${subject.ownerRef.custodyRef.custodyId}`, 1,
    seed.scope.projectSessionId, seed.scope.runId, "chair", "custody-terminal", "custody",
    subject.ownerRef.custodyRef.custodyId, 1, subjectDigest, seed.scope.authorityId, 1,
    null, null, canonicalJson(receipt), receipt.receiptDigest, receipt.attestation, 10,
  );
}

function seedAuthorityReceipt(
  database: Database.Database,
  seed: Seed,
  subject: ReturnType<typeof custodySubject>,
  receipt: LifecycleAuthenticatedReceipt,
): void {
  database.prepare(`INSERT INTO lifecycle_authority_receipts VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`).run(
    receipt.intentDigest, `batch:${subject.ownerRef.custodyRef.custodyId}`, 1,
    seed.scope.projectSessionId, seed.scope.runId, "chair", receipt.kind, "custody",
    subject.ownerRef.custodyRef.custodyId, 1, receipt.subjectDigest, receipt.authorityId,
    receipt.authoritySequence, receipt.authoritySequence === 1 ? null : receipt.authoritySequence - 1,
    receipt.previousReceiptDigest, canonicalJson(receipt),
    receipt.receiptDigest, receipt.attestation, 10,
  );
}

function advanceLocalScopeHead(
  database: Database.Database,
  checkpoint: LifecycleAuthenticatedScopeCheckpoint,
): void {
  const checkpointBody = {
    schemaVersion: 1,
    authorityId: checkpoint.authorityId,
    projectSessionId: checkpoint.projectSessionId,
    runId: checkpoint.runId,
    receiptCountDec: String(checkpoint.receiptCount),
    headAuthoritySequenceDec: String(checkpoint.headAuthoritySequence),
    headReceiptDigest: checkpoint.headReceiptDigest,
    orderedRecordSetDigest: checkpoint.orderedRecordSetDigest,
    checkpointDigest: checkpoint.checkpointDigest,
    attestation: checkpoint.attestation,
  };
  database.prepare(`INSERT INTO lifecycle_receipt_scope_checkpoints VALUES (?,?,?,?,?,?,?,?,?,?,?)`).run(
    checkpoint.projectSessionId, checkpoint.runId, checkpoint.authorityId,
    checkpoint.receiptCount, checkpoint.headAuthoritySequence, checkpoint.headReceiptDigest,
    checkpoint.orderedRecordSetDigest, canonicalJson(checkpointBody), checkpoint.checkpointDigest,
    checkpoint.attestation, 11,
  );
  database.prepare(`
    UPDATE lifecycle_receipt_scope_heads SET checkpoint_digest=?,revision=revision+1
     WHERE project_session_id=? AND run_id=?
  `).run(checkpoint.checkpointDigest, checkpoint.projectSessionId, checkpoint.runId);
}

describe("LifecycleReceiptRecoveryService", () => {
  it("hydrates an authenticated namespace without writing and allows pending intents to be absent or present", async () => {
    const { database, authority, seed } = await seededScope();
    const absent = custodySubject(seed, "custody-absent");
    const present = custodySubject(seed, "custody-present");
    const absentIntent = digest("intent", "absent");
    const presentIntent = digest("intent", "present");
    seedIntent(database, seed, absent, absentIntent);
    seedIntent(database, seed, present, presentIntent);
    await authority.appendReceipt(presentIntent, present);
    const before = (database.prepare(`SELECT total_changes() AS value`).get() as { value: number }).value;

    const result = await new LifecycleReceiptRecoveryService(database, authority).hydrateProject(seed.projectId);

    expect(result).toMatchObject({ scopeCount: 1, receiptCount: 1, pendingIntentCount: 2 });
    expect((database.prepare(`SELECT total_changes() AS value`).get() as { value: number }).value).toBe(before);
    database.close();
  });

  it("returns RECOVERY_PENDING for an unresolved admission outbox without calling admission", async () => {
    const database = new Database(":memory:");
    database.exec(readFileSync(new URL("../../../migrations/0001-current-baseline.sql", import.meta.url), "utf8"));
    const authority = new TestLifecycleReceiptAuthority();
    const projectId = "project-pending-admission";
    const scope = {
      schemaVersion: 1,
      projectId,
      projectSessionId: "session-pending-admission",
      runId: "run-pending-admission",
      authorityId: authority.authorityId,
      admissionDigest: digest("admission", projectId),
      admittedAt: 10,
    };
    database.prepare(`INSERT INTO projects VALUES (?,?,NULL,1,1,?,?)`).run(
      projectId, "/tmp/project-pending-admission", 10, 10,
    );
    database.prepare(`INSERT INTO lifecycle_receipt_projects VALUES (?,?,?)`).run(
      projectId, authority.authorityId, 10,
    );
    database.prepare(`INSERT INTO lifecycle_scope_admission_outbox VALUES (?,?,?,?,?,?,?,?,?,?)`).run(
      digest("scope-admission-outbox", {
        schemaVersion: 1,
        scopeDigest: digest("admitted-scope", scope),
      }), projectId, scope.projectSessionId, scope.runId,
      scope.authorityId, scope.admissionDigest, scope.admittedAt, canonicalJson(scope),
      digest("admitted-scope", scope), 10,
    );

    await expect(new LifecycleReceiptRecoveryService(database, authority).hydrateProject(projectId))
      .rejects.toEqual(expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "RECOVERY_PENDING" }));
    expect(authority.admitCalls).toBe(0);
    database.close();
  });

  it("bounds an unresponsive authority call as retryable RECOVERY_PENDING", async () => {
    const { database, authority, seed } = await seededScope();
    authority.readNamespaceCheckpoint = async () => await new Promise<never>(() => undefined);

    await expect(new LifecycleReceiptRecoveryService(
      database,
      authority,
      { authorityCallTimeoutMs: 5 },
    ).hydrateProject(seed.projectId)).rejects.toEqual(
      expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "RECOVERY_PENDING" }),
    );
    database.close();
  });

  it("maps typed transient authority unavailability to RECOVERY_PENDING but generic evidence failures to SNAPSHOT_INVALID", async () => {
    const transient = await seededScope();
    transient.authority.readNamespaceCheckpoint = async () => {
      throw new LifecycleReceiptAuthorityUnavailableError("authority restarting");
    };
    await expect(new LifecycleReceiptRecoveryService(transient.database, transient.authority)
      .hydrateProject(transient.seed.projectId)).rejects.toEqual(
      expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "RECOVERY_PENDING" }),
    );
    transient.database.close();

    const invalid = await seededScope();
    invalid.authority.readNamespaceCheckpoint = async () => {
      throw new Error("unknown pinned namespace");
    };
    await expect(new LifecycleReceiptRecoveryService(invalid.database, invalid.authority)
      .hydrateProject(invalid.seed.projectId)).rejects.toEqual(
      expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "SNAPSHOT_INVALID" }),
    );
    invalid.database.close();
  });

  it("accepts a real-writer-shaped nonzero full checkpoint with an externally present pending intent", async () => {
    const { database, authority, seed } = await seededScope();
    const subject = custodySubject(seed, "custody-pending-present");
    const intent = digest("intent", "pending-present");
    seedIntent(database, seed, subject, intent);
    await authority.appendReceipt(intent, subject);
    advanceLocalScopeHead(database, authority.latestScopeCheckpoint(seed.scope.projectSessionId, seed.scope.runId));

    await expect(new LifecycleReceiptRecoveryService(database, authority).hydrateProject(seed.projectId))
      .resolves.toMatchObject({ receiptCount: 1, pendingIntentCount: 1, committedReceiptCount: 0 });
    database.close();
  });

  it.each(["gap", "duplicate", "wrong-set-digest"] as const)(
    "rejects authenticated scope %s drift",
    async (corruption) => {
      const { database, authority, seed } = await seededScope();
      for (const id of ["one", "two"]) {
        const subject = custodySubject(seed, `custody-${id}`);
        const intent = digest("intent", id);
        seedIntent(database, seed, subject, intent);
        await authority.appendReceipt(intent, subject);
      }
      authority.corruption = corruption;

      await expect(new LifecycleReceiptRecoveryService(database, authority).hydrateProject(seed.projectId))
        .rejects.toEqual(expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "SNAPSHOT_INVALID" }));
      database.close();
    },
  );

  it("rejects an external receipt with no local intent", async () => {
    const { database, authority, seed } = await seededScope();
    await authority.appendReceipt(digest("intent", "extra"), custodySubject(seed, "custody-extra"));

    await expect(new LifecycleReceiptRecoveryService(database, authority).hydrateProject(seed.projectId))
      .rejects.toEqual(expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "SNAPSHOT_INVALID" }));
    database.close();
  });

  it("rejects a locally committed receipt missing from the authority", async () => {
    const { database, authority, seed } = await seededScope();
    const subject = custodySubject(seed, "custody-missing");
    const intent = digest("intent", "missing");
    seedIntent(database, seed, subject, intent);
    seedCommittedReceiptWithoutAuthority(database, seed, subject, intent);

    await expect(new LifecycleReceiptRecoveryService(database, authority).hydrateProject(seed.projectId))
      .rejects.toEqual(expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "SNAPSHOT_INVALID" }));
    database.close();
  });

  it("rejects invalid namespace attestation", async () => {
    const { database, authority, seed } = await seededScope();
    authority.verifyNamespaceCheckpoint = async () => false;

    await expect(new LifecycleReceiptRecoveryService(database, authority).hydrateProject(seed.projectId))
      .rejects.toEqual(expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "SNAPSHOT_INVALID" }));
    database.close();
  });

  it.each(["stored-attestation", "resolution-member"] as const)(
    "rejects mutated admission namespace %s",
    async (mutation) => {
      const { database, authority, seed } = await seededScope();
      if (mutation === "stored-attestation") {
        database.exec(`DROP TRIGGER lifecycle_receipt_namespace_checkpoints_immutable_update`);
        database.prepare(`UPDATE lifecycle_receipt_namespace_checkpoints SET attestation='forged'`).run();
      } else {
        database.exec(`DROP TRIGGER lifecycle_scope_admission_resolutions_immutable_update`);
        database.prepare(`UPDATE lifecycle_scope_admission_resolutions SET namespace_member_json='{}'`).run();
      }

      await expect(new LifecycleReceiptRecoveryService(database, authority).hydrateProject(seed.projectId))
        .rejects.toEqual(expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "SNAPSHOT_INVALID" }));
      database.close();
    },
  );

  it.each([
    "outbox-admission-digest",
    "admitted-at",
    "scope-json",
    "scope-digest-all-copies",
    "resolution-json-and-digest",
  ] as const)("rejects mutated admission tuple %s", async (mutation) => {
    const { database, authority, seed } = await seededScope();
    if (mutation === "outbox-admission-digest") {
      database.exec(`DROP TRIGGER lifecycle_scope_admission_outbox_immutable_update`);
      database.prepare(`UPDATE lifecycle_scope_admission_outbox SET admission_digest=?`)
        .run(digest("forged", mutation));
    } else if (mutation === "admitted-at") {
      database.exec(`DROP TRIGGER lifecycle_admitted_run_scopes_immutable_update`);
      database.prepare(`UPDATE lifecycle_admitted_run_scopes SET admitted_at=admitted_at+1`).run();
    } else if (mutation === "scope-json") {
      database.exec(`DROP TRIGGER lifecycle_scope_admission_outbox_immutable_update`);
      database.prepare(`UPDATE lifecycle_scope_admission_outbox SET scope_json='{}'`).run();
    } else if (mutation === "scope-digest-all-copies") {
      database.exec(`
        DROP TRIGGER lifecycle_scope_admission_outbox_immutable_update;
        DROP TRIGGER lifecycle_admitted_run_scopes_immutable_update;
        DROP TRIGGER lifecycle_scope_admission_resolutions_immutable_update;
      `);
      const forged = digest("forged", mutation);
      database.prepare(`UPDATE lifecycle_scope_admission_outbox SET scope_digest=?`).run(forged);
      database.prepare(`UPDATE lifecycle_admitted_run_scopes SET scope_digest=?`).run(forged);
      database.prepare(`UPDATE lifecycle_scope_admission_resolutions SET scope_digest=?`).run(forged);
    } else {
      database.exec(`
        DROP TRIGGER lifecycle_admitted_run_scopes_immutable_update;
        DROP TRIGGER lifecycle_scope_admission_resolutions_immutable_update;
      `);
      const forged = digest("forged", mutation);
      database.prepare(`UPDATE lifecycle_scope_admission_resolutions SET resolution_json='{}',resolution_digest=?`)
        .run(forged);
      database.prepare(`UPDATE lifecycle_admitted_run_scopes SET scope_admission_resolution_digest=?`)
        .run(forged);
    }

    await expect(new LifecycleReceiptRecoveryService(database, authority).hydrateProject(seed.projectId))
      .rejects.toEqual(expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "SNAPSHOT_INVALID" }));
    database.close();
  });

  it("rejects a nullable partial local authority receipt row", async () => {
    const { database, authority, seed } = await seededScope();
    const subject = custodySubject(seed, "custody-partial");
    const intent = digest("intent", "partial");
    seedIntent(database, seed, subject, intent);
    database.prepare(`INSERT INTO lifecycle_authority_receipts VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`).run(
      intent, "batch:custody-partial", 1, seed.scope.projectSessionId, seed.scope.runId,
      "chair", "custody-terminal", "custody", "custody-partial", 1,
      digest("receipt-subject", subject), seed.scope.authorityId, 1, null, null,
      null, null, null, 10,
    );

    await expect(new LifecycleReceiptRecoveryService(database, authority).hydrateProject(seed.projectId))
      .rejects.toEqual(expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "SNAPSHOT_INVALID" }));
    database.close();
  });

  it("rejects a downgraded local receipt tuple even when the external chain is valid", async () => {
    const { database, authority, seed } = await seededScope();
    const subject = custodySubject(seed, "custody-downgraded");
    const intent = digest("intent", "downgraded");
    seedIntent(database, seed, subject, intent);
    const receipt = await authority.appendReceipt(intent, subject);
    seedAuthorityReceipt(database, seed, subject, receipt);
    advanceLocalScopeHead(database, authority.latestScopeCheckpoint(seed.scope.projectSessionId, seed.scope.runId));
    await expect(new LifecycleReceiptRecoveryService(database, authority).hydrateProject(seed.projectId))
      .resolves.toMatchObject({ receiptCount: 1, pendingIntentCount: 0, committedReceiptCount: 1 });
    database.exec(`DROP TRIGGER lifecycle_authority_receipts_immutable_update`);
    database.prepare(`UPDATE lifecycle_authority_receipts SET receipt_json='{}' WHERE intent_digest=?`).run(intent);

    await expect(new LifecycleReceiptRecoveryService(database, authority).hydrateProject(seed.projectId))
      .rejects.toEqual(expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "SNAPSHOT_INVALID" }));
    database.close();
  });

  it.each(["custody", "run"] as const)("rejects whole-%s deletion behind the namespace", async (kind) => {
    const { database, authority, seed } = await seededScope();
    const subject = custodySubject(seed, "custody-deleted");
    const intent = digest("intent", "deleted");
    seedIntent(database, seed, subject, intent);
    await authority.appendReceipt(intent, subject);
    if (kind === "custody") {
      database.exec(`DROP TRIGGER lifecycle_receipt_intents_immutable_delete`);
      database.prepare(`DELETE FROM lifecycle_receipt_intents WHERE intent_digest=?`).run(intent);
    } else {
      database.exec(`DROP TRIGGER lifecycle_admitted_run_scopes_immutable_delete`);
      database.prepare(`DELETE FROM lifecycle_admitted_run_scopes WHERE run_id=?`).run(seed.scope.runId);
    }

    await expect(new LifecycleReceiptRecoveryService(database, authority).hydrateProject(seed.projectId))
      .rejects.toEqual(expect.objectContaining<Partial<LifecycleReceiptRecoveryError>>({ code: "SNAPSHOT_INVALID" }));
    database.close();
  });
});
