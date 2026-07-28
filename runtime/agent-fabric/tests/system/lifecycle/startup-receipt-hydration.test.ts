import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { openFabric, type Fabric } from "../../../src/index.ts";
import { LifecycleReceiptAuthorityUnavailableError } from "../../../src/lifecycle/receipt-recovery.ts";
import type {
  LifecycleAdmittedRunScope,
  LifecycleAuthenticatedNamespaceCheckpoint,
  LifecycleAuthenticatedScopeCheckpoint,
  LifecycleDigest,
} from "../../../src/lifecycle/receipt-authority.ts";
import { canonicalJson } from "../../../src/persistence/row-codec.ts";
import { TestLifecycleReceiptAuthority } from "../../support/lifecycle-receipt-authority-fake.ts";

type StartupSeed = Readonly<{
  projectId: string;
  scope: LifecycleAdmittedRunScope;
  initial: LifecycleAuthenticatedScopeCheckpoint;
  namespace: LifecycleAuthenticatedNamespaceCheckpoint;
}>;

type StartupFixture = Readonly<{
  directory: string;
  databasePath: string;
  database: Database.Database;
  authority: TestLifecycleReceiptAuthority;
  seed: StartupSeed;
}>;

const runtimes: Fabric[] = [];
const directories: string[] = [];

function digest(domain: string, value: unknown): LifecycleDigest {
  return `sha256:${createHash("sha256")
    .update(`agent-fabric.lifecycle.v1\0${domain}\0${canonicalJson(value)}`)
    .digest("hex")}`;
}

function custodySubject(seed: StartupSeed, custodyId: string) {
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

function seedLocalScope(database: Database.Database, seed: StartupSeed): void {
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
  database.prepare(`INSERT INTO lifecycle_receipt_projects VALUES (?,?,?)`).run(
    projectId, scope.authorityId, 10,
  );
  database.prepare(`INSERT INTO lifecycle_scope_admission_outbox VALUES (?,?,?,?,?,?,?,?,?,?)`).run(
    requestId, projectId, scope.projectSessionId, scope.runId, scope.authorityId,
    scope.admissionDigest, scope.admittedAt, scopeJson, scopeDigest, 10,
  );
  database.prepare(`INSERT INTO lifecycle_admitted_run_scopes VALUES (?,?,?,?,?,?,?,?,?,?)`).run(
    projectId, scope.projectSessionId, scope.runId, scope.authorityId, scope.admissionDigest,
    scope.admittedAt, requestId, scopeDigest, initial.checkpointDigest,
    digest("scope-admission-resolution", resolution),
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
    canonicalJson(member), 10, canonicalJson(resolution), digest("scope-admission-resolution", resolution),
  );
}

function seedIntent(
  database: Database.Database,
  seed: StartupSeed,
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

function deleteBehindImmutableTrigger(
  database: Database.Database,
  triggerName: string,
  deletion: () => void,
): void {
  const stored = database.prepare(`SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?`).get(triggerName);
  if (typeof stored !== "object" || stored === null || !("sql" in stored) || typeof stored.sql !== "string") {
    throw new Error(`missing test trigger ${triggerName}`);
  }
  database.exec(`DROP TRIGGER ${triggerName}`);
  deletion();
  database.exec(stored.sql);
}

async function startupFixture(): Promise<StartupFixture> {
  const directory = await mkdtemp(join(tmpdir(), "fabric-startup-receipts-"));
  directories.push(directory);
  const databasePath = join(directory, "fabric.sqlite3");
  const bootstrap = await openFabric({ databasePath, workspaceRoots: [directory] });
  await bootstrap.close();
  const database = new Database(databasePath);
  database.pragma("foreign_keys = OFF");
  const authority = new TestLifecycleReceiptAuthority();
  const projectId = "project-startup-recovery";
  const scope: LifecycleAdmittedRunScope = {
    schemaVersion: 1,
    projectId,
    projectSessionId: "session-startup-recovery",
    runId: "run-startup-recovery",
    authorityId: authority.authorityId,
    admissionDigest: digest("admission", { projectId, runId: "run-startup-recovery" }),
    admittedAt: 10,
  };
  const initial = await authority.admitScope(scope);
  const namespace = await authority.readNamespaceCheckpoint(projectId);
  const seed = { projectId, scope, initial, namespace };
  seedLocalScope(database, seed);
  return { directory, databasePath, database, authority, seed };
}

afterEach(async () => {
  await Promise.all(runtimes.splice(0).map(async (runtime) => await runtime.close()));
  await Promise.all(directories.splice(0).map(async (directory) => await rm(directory, { recursive: true, force: true })));
});

describe("Fabric lifecycle receipt startup hydration", () => {
  it("hydrates a post-authority-append crash before startup recovery without appending again", async () => {
    const fixture = await startupFixture();
    const subject = custodySubject(fixture.seed, "custody-appended-before-crash");
    const intent = digest("intent", "appended-before-crash");
    seedIntent(fixture.database, fixture.seed, subject, intent);
    await fixture.authority.appendReceipt(intent, subject);
    const originalReadNamespace = fixture.authority.readNamespaceCheckpoint.bind(fixture.authority);
    let namespaceReads = 0;
    fixture.authority.readNamespaceCheckpoint = async (projectId) => {
      namespaceReads += 1;
      return await originalReadNamespace(projectId);
    };
    fixture.database.close();

    const runtime = await openFabric({
      databasePath: fixture.databasePath,
      workspaceRoots: [fixture.directory],
      lifecycleReceiptAuthority: fixture.authority,
    });
    runtimes.push(runtime);

    await expect(runtime.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 0 });
    expect(namespaceReads).toBeGreaterThan(0);
    expect(fixture.authority.appendCalls).toBe(1);
  });

  it("fails closed when a retained non-current pinned checkpoint is deleted from the authority", async () => {
    const fixture = await startupFixture();
    // Advance the authority ledger so the admission (initial) checkpoint the
    // fixture pinned in the local mirror is no longer the current head.
    const subject = custodySubject(fixture.seed, "custody-advances-the-ledger");
    const intent = digest("intent", "advances-the-ledger");
    seedIntent(fixture.database, fixture.seed, subject, intent);
    await fixture.authority.appendReceipt(intent, subject);
    // Delete the now non-current, retained checkpoint from the authority store.
    // A standalone authority can still reopen after this, so composed startup
    // recovery is the boundary that must fail closed when it re-requests it.
    fixture.authority.forgetScopeSnapshot(fixture.seed.initial.checkpointDigest);
    fixture.database.close();
    const recoveryFaults: string[] = [];

    const runtime = await openFabric({
      databasePath: fixture.databasePath,
      workspaceRoots: [fixture.directory],
      lifecycleReceiptAuthority: fixture.authority,
      fault: (label) => recoveryFaults.push(label),
    });
    runtimes.push(runtime);

    await expect(runtime.recoverStartupState()).rejects.toMatchObject({ code: "SNAPSHOT_INVALID" });
    expect(recoveryFaults).toEqual([]);
  });

  it.each(["custody", "run", "scope-set"] as const)(
    "rejects local %s deletion before any generic startup recovery",
    async (deletion) => {
      const fixture = await startupFixture();
      const subject = custodySubject(fixture.seed, "custody-deleted-before-startup");
      const intent = digest("intent", "deleted-before-startup");
      seedIntent(fixture.database, fixture.seed, subject, intent);
      await fixture.authority.appendReceipt(intent, subject);
      if (deletion === "custody") {
        deleteBehindImmutableTrigger(
          fixture.database,
          "lifecycle_receipt_intents_immutable_delete",
          () => { fixture.database.prepare(`DELETE FROM lifecycle_receipt_intents WHERE intent_digest=?`).run(intent); },
        );
      } else if (deletion === "run") {
        deleteBehindImmutableTrigger(
          fixture.database,
          "lifecycle_admitted_run_scopes_immutable_delete",
          () => {
            fixture.database.prepare(`DELETE FROM lifecycle_admitted_run_scopes WHERE run_id=?`)
              .run(fixture.seed.scope.runId);
          },
        );
      } else {
        deleteBehindImmutableTrigger(
          fixture.database,
          "lifecycle_admitted_run_scopes_immutable_delete",
          () => {
            deleteBehindImmutableTrigger(
              fixture.database,
              "lifecycle_scope_admission_outbox_immutable_delete",
              () => {
                fixture.database.prepare(`DELETE FROM lifecycle_admitted_run_scopes WHERE run_id=?`)
                  .run(fixture.seed.scope.runId);
                fixture.database.prepare(`DELETE FROM lifecycle_scope_admission_outbox WHERE run_id=?`)
                  .run(fixture.seed.scope.runId);
              },
            );
          },
        );
      }
      fixture.database.close();
      const recoveryFaults: string[] = [];

      const runtime = await openFabric({
        databasePath: fixture.databasePath,
        workspaceRoots: [fixture.directory],
        lifecycleReceiptAuthority: fixture.authority,
        fault: (label) => recoveryFaults.push(label),
      });
      runtimes.push(runtime);

      await expect(runtime.recoverStartupState()).rejects.toMatchObject({ code: "SNAPSHOT_INVALID" });
      expect(fixture.authority.appendCalls).toBe(1);
      expect(recoveryFaults).toEqual([]);
    },
  );

  it("rejects an externally advanced ledger before any generic startup recovery", async () => {
    const fixture = await startupFixture();
    const firstSubject = custodySubject(fixture.seed, "custody-local-snapshot");
    const firstIntent = digest("intent", "local-snapshot");
    seedIntent(fixture.database, fixture.seed, firstSubject, firstIntent);
    await fixture.authority.appendReceipt(firstIntent, firstSubject);
    await fixture.authority.appendReceipt(
      digest("intent", "external-ledger-advance"),
      custodySubject(fixture.seed, "custody-external-ledger-advance"),
    );
    fixture.database.close();
    const recoveryFaults: string[] = [];

    const runtime = await openFabric({
      databasePath: fixture.databasePath,
      workspaceRoots: [fixture.directory],
      lifecycleReceiptAuthority: fixture.authority,
      fault: (label) => recoveryFaults.push(label),
    });
    runtimes.push(runtime);

    await expect(runtime.recoverStartupState()).rejects.toMatchObject({ code: "SNAPSHOT_INVALID" });
    expect(fixture.authority.appendCalls).toBe(2);
    expect(recoveryFaults).toEqual([]);
  });

  it("keeps transient authority unavailability retryable and blocks startup recovery", async () => {
    const fixture = await startupFixture();
    fixture.authority.readNamespaceCheckpoint = async () => {
      throw new LifecycleReceiptAuthorityUnavailableError("authority restarting");
    };
    fixture.database.close();
    const recoveryFaults: string[] = [];

    const runtime = await openFabric({
      databasePath: fixture.databasePath,
      workspaceRoots: [fixture.directory],
      lifecycleReceiptAuthority: fixture.authority,
      fault: (label) => recoveryFaults.push(label),
    });
    runtimes.push(runtime);

    await expect(runtime.recoverStartupState()).rejects.toMatchObject({ code: "RECOVERY_PENDING" });
    expect(fixture.authority.appendCalls).toBe(0);
    expect(recoveryFaults).toEqual([]);
  });

  it("keeps startup pending when local receipt state has no authority", async () => {
    const fixture = await startupFixture();
    fixture.database.close();
    const recoveryFaults: string[] = [];

    const runtime = await openFabric({
      databasePath: fixture.databasePath,
      workspaceRoots: [fixture.directory],
      fault: (label) => recoveryFaults.push(label),
    });
    runtimes.push(runtime);

    await expect(runtime.recoverStartupState()).rejects.toMatchObject({ code: "RECOVERY_PENDING" });
    expect(recoveryFaults).toEqual([]);
  });

  it("rejects a marker-only restart under a different receipt authority", async () => {
    const fixture = await startupFixture();
    deleteBehindImmutableTrigger(
      fixture.database,
      "lifecycle_admitted_run_scopes_immutable_delete",
      () => {
        deleteBehindImmutableTrigger(
          fixture.database,
          "lifecycle_scope_admission_outbox_immutable_delete",
          () => {
            fixture.database.prepare(`DELETE FROM lifecycle_admitted_run_scopes WHERE run_id=?`)
              .run(fixture.seed.scope.runId);
            fixture.database.prepare(`DELETE FROM lifecycle_scope_admission_outbox WHERE run_id=?`)
              .run(fixture.seed.scope.runId);
          },
        );
      },
    );
    fixture.database.close();
    const crossedAuthority = new TestLifecycleReceiptAuthority("other-lifecycle-receipt-authority");

    const runtime = await openFabric({
      databasePath: fixture.databasePath,
      workspaceRoots: [fixture.directory],
      lifecycleReceiptAuthority: crossedAuthority,
    });
    runtimes.push(runtime);

    await expect(runtime.recoverStartupState()).rejects.toMatchObject({ code: "SNAPSHOT_INVALID" });
  });
});
