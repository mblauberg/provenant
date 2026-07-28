import { createHash, randomBytes } from "node:crypto";
import { chmod, mkdir, mkdtemp, readFile, rm, symlink, unlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { openLocalLifecycleReceiptAuthority } from "../../../src/lifecycle/local-receipt-authority.ts";
import type { LifecycleAdmittedRunScope, LifecycleDigest } from "../../../src/lifecycle/receipt-authority.ts";
import { canonicalJson } from "../../../src/persistence/row-codec.ts";

const roots: string[] = [];

function digest(domain: string, value: unknown): LifecycleDigest {
  return `sha256:${createHash("sha256")
    .update(`agent-fabric.lifecycle.v1\0${domain}\0${canonicalJson(value)}`)
    .digest("hex")}`;
}

async function provisionAuthority(
  authorityId = "local-authority",
  transformSchema: (schema: string) => string = (schema) => schema,
): Promise<string> {
  const stateDirectory = await mkdtemp(join(tmpdir(), "fabric-local-receipts-"));
  roots.push(stateDirectory);
  await chmod(stateDirectory, 0o700);
  const databasePath = join(stateDirectory, "lifecycle-receipts.sqlite3");
  const database = new Database(databasePath);
  database.exec(transformSchema(
    await readFile(new URL("../../../schemas/lifecycle-receipt-authority-v1.sql", import.meta.url), "utf8"),
  ));
  database.prepare(`INSERT INTO authority_metadata VALUES(1,1,?)`).run(authorityId);
  database.close();
  await chmod(databasePath, 0o600);
  await writeFile(join(stateDirectory, "lifecycle-receipts.hmac.key"), randomBytes(32), { mode: 0o600 });
  return stateDirectory;
}

function custodySubject(custodyId: string): Readonly<Record<string, unknown>> {
  return {
    schemaVersion: 1,
    kind: "custody-terminal",
    projectSessionId: "session",
    runId: "run",
    agentId: "agent",
    ownerRef: {
      kind: "custody",
      custodyRef: { schemaVersion: 1, runId: "run", agentId: "agent", custodyId, custodyRevision: 1 },
      sourceRefDigest: digest("source", custodyId),
    },
  };
}

function custodyOwnerRef(custodyId: string): Record<string, unknown> {
  return {
    kind: "custody",
    custodyRef: { schemaVersion: 1, runId: "run", agentId: "agent", custodyId, custodyRevision: 1 },
    sourceRefDigest: digest("source", custodyId),
  };
}

function generationLossOwnerRef(lossId: string): Record<string, unknown> {
  return {
    kind: "generation-loss",
    generationLossRef: { schemaVersion: 1, runId: "run", agentId: "agent", generationLossId: lossId, generationLossRevision: 1 },
    sourceRefDigest: digest("source", lossId),
  };
}

// Mirrors the producer at terminal-owner-receipt.ts:205-216 (loss branch): a
// generation-loss-terminal subject whose ownerRef is the generation-loss
// afterRef, generationLossRef embedding the owning runId/agentId.
function generationLossSubject(lossId: string): Record<string, unknown> {
  return {
    schemaVersion: 1,
    kind: "generation-loss-terminal",
    projectSessionId: "session",
    runId: "run",
    agentId: "agent",
    ownerRef: generationLossOwnerRef(lossId),
  };
}

function reviewAdoptionSubject(custodyId: string): Record<string, unknown> {
  return {
    schemaVersion: 1,
    kind: "review-adoption-decision",
    projectSessionId: "session",
    runId: "run",
    agentId: "agent",
    ownerRef: custodyOwnerRef(custodyId),
    reviewDecisionDigest: digest("review-decision", custodyId),
  };
}

function admittedScope(): LifecycleAdmittedRunScope {
  return {
    schemaVersion: 1,
    projectId: "project",
    projectSessionId: "session",
    runId: "run",
    authorityId: "local-authority",
    admissionDigest: digest("admission", { projectId: "project", runId: "run" }),
    admittedAt: 1,
  };
}

afterEach(async () => {
  await Promise.all(roots.splice(0).map(async (root) => await rm(root, { recursive: true, force: true })));
});

describe("local lifecycle receipt authority", () => {
  it("opens only a pre-provisioned authority with the configured immutable identity", async () => {
    const stateDirectory = await provisionAuthority();
    const authority = openLocalLifecycleReceiptAuthority({
      stateDirectory,
      expectedAuthorityId: "local-authority",
    });
    await expect(authority.admitScope(admittedScope())).resolves.toMatchObject({
      authorityId: "local-authority",
      receiptCount: 0,
      headReceiptDigest: null,
    });
    authority.close();
  });

  describe("enforces subject-to-owner binding for produced shapes", () => {
    async function openAdmitted() {
      const stateDirectory = await provisionAuthority();
      const authority = openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" });
      await authority.admitScope(admittedScope());
      return authority;
    }

    it("admits a correctly bound custody-terminal receipt", async () => {
      const authority = await openAdmitted();
      await expect(authority.appendReceipt(digest("intent", "custody-green"), custodySubject("custody-green")))
        .resolves.toMatchObject({ kind: "custody-terminal" });
      authority.close();
    });

    it("admits a correctly bound review-adoption-decision receipt", async () => {
      const authority = await openAdmitted();
      await expect(authority.appendReceipt(digest("intent", "review-green"), reviewAdoptionSubject("custody-review")))
        .resolves.toMatchObject({ kind: "review-adoption-decision" });
      authority.close();
    });

    it("rejects a custody-terminal receipt whose ownerRef kind is crossed", async () => {
      const authority = await openAdmitted();
      const subject = {
        ...custodySubject("custody-crossed"),
        ownerRef: {
          kind: "generation-loss",
          generationLossRef: { schemaVersion: 1, runId: "run", agentId: "agent", generationLossId: "loss", generationLossRevision: 1 },
          sourceRefDigest: digest("source", "loss"),
        },
      };
      await expect(authority.appendReceipt(digest("intent", "custody-crossed"), subject)).rejects.toThrowError(/binding/u);
      authority.close();
    });

    it("rejects a custody-terminal receipt whose ownerRef carries an extra variant", async () => {
      const authority = await openAdmitted();
      const subject = {
        ...custodySubject("custody-extra"),
        ownerRef: {
          ...custodyOwnerRef("custody-extra"),
          generationLossRef: { schemaVersion: 1, runId: "run", agentId: "agent", generationLossId: "loss", generationLossRevision: 1 },
        },
      };
      await expect(authority.appendReceipt(digest("intent", "custody-extra"), subject)).rejects.toThrowError(/crossed variant/u);
      authority.close();
    });

    it("rejects a custody-terminal receipt whose embedded identity is crossed", async () => {
      const authority = await openAdmitted();
      const subject = {
        ...custodySubject("custody-identity"),
        ownerRef: {
          kind: "custody",
          custodyRef: { schemaVersion: 1, runId: "other-run", agentId: "agent", custodyId: "custody-identity", custodyRevision: 1 },
          sourceRefDigest: digest("source", "custody-identity"),
        },
      };
      await expect(authority.appendReceipt(digest("intent", "custody-identity"), subject)).rejects.toThrowError(/identity is crossed/u);
      authority.close();
    });

    it("rejects a review-adoption-decision receipt whose ownerRef kind is crossed", async () => {
      const authority = await openAdmitted();
      const subject = {
        ...reviewAdoptionSubject("review-crossed"),
        ownerRef: {
          kind: "generation-loss",
          generationLossRef: { schemaVersion: 1, runId: "run", agentId: "agent", generationLossId: "loss", generationLossRevision: 1 },
          sourceRefDigest: digest("source", "loss"),
        },
      };
      await expect(authority.appendReceipt(digest("intent", "review-crossed"), subject)).rejects.toThrowError(/binding/u);
      authority.close();
    });

    it("rejects a review-adoption-decision receipt whose ownerRef carries an extra variant", async () => {
      const authority = await openAdmitted();
      const subject = {
        ...reviewAdoptionSubject("review-extra"),
        ownerRef: {
          ...custodyOwnerRef("review-extra"),
          retirementRef: { schemaVersion: 1, runId: "run", agentId: "agent", retirementId: "retire", revisionDec: "1" },
        },
      };
      await expect(authority.appendReceipt(digest("intent", "review-extra"), subject)).rejects.toThrowError(/crossed variant/u);
      authority.close();
    });

    it("rejects a review-adoption-decision receipt whose embedded identity is crossed", async () => {
      const authority = await openAdmitted();
      const subject = {
        ...reviewAdoptionSubject("review-identity"),
        ownerRef: {
          kind: "custody",
          custodyRef: { schemaVersion: 1, runId: "run", agentId: "other-agent", custodyId: "review-identity", custodyRevision: 1 },
          sourceRefDigest: digest("source", "review-identity"),
        },
      };
      await expect(authority.appendReceipt(digest("intent", "review-identity"), subject)).rejects.toThrowError(/identity is crossed/u);
      authority.close();
    });

    it("admits a correctly bound generation-loss-terminal receipt", async () => {
      const authority = await openAdmitted();
      await expect(authority.appendReceipt(digest("intent", "loss-green"), generationLossSubject("loss-green")))
        .resolves.toMatchObject({ kind: "generation-loss-terminal" });
      authority.close();
    });

    it("rejects a generation-loss-terminal receipt relabelled onto a crossed custody ownerRef", async () => {
      // The confirmed cross-family P1: a caller relabels a subject to the once
      // "unenforced" generation-loss-terminal kind and hands a custody ownerRef
      // with a crossed embedded identity. Admission must now fail closed.
      const authority = await openAdmitted();
      const subject = {
        schemaVersion: 1,
        kind: "generation-loss-terminal",
        projectSessionId: "session",
        runId: "run",
        agentId: "agent",
        ownerRef: {
          kind: "custody",
          custodyRef: { schemaVersion: 1, runId: "other-run", agentId: "other-agent", custodyId: "relabelled", custodyRevision: 1 },
          sourceRefDigest: digest("source", "relabelled"),
        },
      };
      await expect(authority.appendReceipt(digest("intent", "relabelled"), subject)).rejects.toThrowError(/binding/u);
      authority.close();
    });

    it("rejects a generation-loss-terminal receipt whose embedded identity is crossed", async () => {
      const authority = await openAdmitted();
      const subject = {
        ...generationLossSubject("loss-identity"),
        ownerRef: {
          kind: "generation-loss",
          generationLossRef: { schemaVersion: 1, runId: "other-run", agentId: "agent", generationLossId: "loss-identity", generationLossRevision: 1 },
          sourceRefDigest: digest("source", "loss-identity"),
        },
      };
      await expect(authority.appendReceipt(digest("intent", "loss-identity"), subject)).rejects.toThrowError(/identity is crossed/u);
      authority.close();
    });

    it("rejects a generation-loss-terminal receipt whose ownerRef carries an extra variant", async () => {
      const authority = await openAdmitted();
      const subject = {
        ...generationLossSubject("loss-extra"),
        ownerRef: {
          ...generationLossOwnerRef("loss-extra"),
          custodyRef: { schemaVersion: 1, runId: "run", agentId: "agent", custodyId: "loss-extra", custodyRevision: 1 },
        },
      };
      await expect(authority.appendReceipt(digest("intent", "loss-extra"), subject)).rejects.toThrowError(/crossed variant/u);
      authority.close();
    });

    it.each([
      ["fresh-origin", {
        kind: "custody",
        custodyRef: { schemaVersion: 1, runId: "run", agentId: "agent", custodyId: "origin", custodyRevision: 1 },
        sourceRefDigest: digest("source", "origin"),
      }],
      ["custody-recovery-retirement", {
        kind: "recovery-retirement",
        retirementRef: { schemaVersion: 1, runId: "run", agentId: "agent", retirementId: "retire", revisionDec: "1" },
        sourceRefDigest: digest("source", "retire"),
      }],
    ] as const)("refuses admission of %s, which has no enforced binding", async (kind, ownerRef) => {
      // Neither kind has a legitimate appendReceipt producer (see the producer
      // census above the binding table); admission fails closed rather than
      // silently authenticating a crossed-identity ownerRef.
      const authority = await openAdmitted();
      const subject = {
        schemaVersion: 1,
        kind,
        projectSessionId: "session",
        runId: "run",
        agentId: "agent",
        ownerRef,
      };
      await expect(authority.appendReceipt(digest("intent", kind), subject))
        .rejects.toThrowError(/no enforced binding for kind .*; admission refused/u);
      authority.close();
    });
  });

  it("reopens a ledger holding producer-shaped generation-loss-terminal receipts", async () => {
    // Reload re-runs enforcement via #records/#validateLedger. A ledger of
    // legitimately producer-authored generation-loss-terminal receipts (the
    // real generation-loss ownerRef shape) must still open after fail-closed.
    const stateDirectory = await provisionAuthority();
    const first = openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" });
    await first.admitScope(admittedScope());
    const custody = await first.appendReceipt(digest("intent", "reload-custody"), custodySubject("reload-custody"));
    const loss = await first.appendReceipt(digest("intent", "reload-loss"), generationLossSubject("reload-loss"));
    first.close();

    const restarted = openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" });
    await expect(restarted.verifyReceipt(custodySubject("reload-custody"), custody)).resolves.toBe(true);
    await expect(restarted.verifyReceipt(generationLossSubject("reload-loss"), loss)).resolves.toBe(true);
    await expect(restarted.appendReceipt(digest("intent", "reload-loss"), generationLossSubject("reload-loss")))
      .resolves.toStrictEqual(loss);
    restarted.close();
  });

  it("rejects receipt rows whose authoritative lookup membership was changed", async () => {
    const stateDirectory = await provisionAuthority();
    const authority = openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" });
    await authority.admitScope(admittedScope());
    await authority.appendReceipt(digest("intent", "one"), custodySubject("custody-one"));
    authority.close();

    const database = new Database(join(stateDirectory, "lifecycle-receipts.sqlite3"));
    database.prepare(`UPDATE receipts SET owner_ref_digest=?`).run(digest("crossed-owner", "other"));
    database.close();

    expect(() => openLocalLifecycleReceiptAuthority({
      stateDirectory,
      expectedAuthorityId: "local-authority",
    })).toThrowError(/membership/u);
  });

  it("persists authenticated receipts and pinned checkpoints across restart and idempotent retry", async () => {
    const stateDirectory = await provisionAuthority();
    const first = openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" });
    const initialCheckpoint = await first.admitScope(admittedScope());
    const subject = custodySubject("custody-restart");
    const intent = digest("intent", "restart");
    const receipt = await first.appendReceipt(intent, subject);
    await expect(first.admitScope(admittedScope())).resolves.toStrictEqual(initialCheckpoint);
    const scopeCheckpoint = await first.readScopeCheckpoint("session", "run");
    const namespaceCheckpoint = await first.readNamespaceCheckpoint("project");
    first.close();

    const restarted = openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" });
    await expect(restarted.appendReceipt(intent, subject)).resolves.toStrictEqual(receipt);
    await expect(restarted.verifyReceipt(subject, receipt)).resolves.toBe(true);
    await expect(restarted.verifyScopeCheckpoint(scopeCheckpoint)).resolves.toBe(true);
    await expect(restarted.verifyNamespaceCheckpoint(namespaceCheckpoint)).resolves.toBe(true);
    await expect(restarted.readScopePageAt(scopeCheckpoint.checkpointDigest, 0)).resolves.toEqual({
      orderedRecords: [{ subject, receipt }],
      nextAfter: null,
    });
    await expect(restarted.readNamespacePageAt(namespaceCheckpoint.checkpointDigest, null)).resolves.toMatchObject({
      orderedScopeHeads: [scopeCheckpoint],
      nextAfter: null,
    });
    restarted.close();
  });

  it("fails closed when an authenticated receipt is modified", async () => {
    const stateDirectory = await provisionAuthority();
    const authority = openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" });
    await authority.admitScope(admittedScope());
    await authority.appendReceipt(digest("intent", "tamper"), custodySubject("custody-tamper"));
    authority.close();

    const database = new Database(join(stateDirectory, "lifecycle-receipts.sqlite3"));
    const stored = database.prepare(`SELECT receipt_json FROM receipts`).get() as { receipt_json: string };
    const receipt = JSON.parse(stored.receipt_json) as Record<string, unknown>;
    receipt.attestation = `hmac-sha256:${"0".repeat(64)}`;
    database.prepare(`UPDATE receipts SET receipt_json=?`).run(canonicalJson(receipt));
    database.close();

    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" }))
      .toThrowError(/chain/u);
  });

  it("fails closed when an admitted scope is modified", async () => {
    const stateDirectory = await provisionAuthority();
    const authority = openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" });
    await authority.admitScope(admittedScope());
    authority.close();

    const database = new Database(join(stateDirectory, "lifecycle-receipts.sqlite3"));
    const stored = database.prepare(`SELECT scope_json FROM admitted_scopes`).get() as { scope_json: string };
    const scope = JSON.parse(stored.scope_json) as Record<string, unknown>;
    scope.admittedAt = 2;
    database.prepare(`UPDATE admitted_scopes SET scope_json=?`).run(canonicalJson(scope));
    database.close();

    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" }))
      .toThrowError(/scope authentication/u);
  });

  it("fails closed when checkpoint membership columns are crossed", async () => {
    const stateDirectory = await provisionAuthority();
    const authority = openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" });
    await authority.admitScope(admittedScope());
    authority.close();

    const database = new Database(join(stateDirectory, "lifecycle-receipts.sqlite3"));
    database.pragma("foreign_keys = OFF");
    database.prepare(`UPDATE scope_snapshots SET project_session_id='crossed-session'`).run();
    database.close();

    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" }))
      .toThrowError(/membership/u);
  });

  it("fails closed on a receipt chain gap", async () => {
    const stateDirectory = await provisionAuthority();
    const authority = openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" });
    await authority.admitScope(admittedScope());
    await authority.appendReceipt(digest("intent", "one"), custodySubject("custody-one"));
    await authority.appendReceipt(digest("intent", "two"), custodySubject("custody-two"));
    authority.close();

    const database = new Database(join(stateDirectory, "lifecycle-receipts.sqlite3"));
    database.prepare(`DELETE FROM receipts WHERE authority_sequence=1`).run();
    database.close();

    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" }))
      .toThrowError(/chain/u);
  });

  it("fails closed when current authenticated membership was deleted", async () => {
    const stateDirectory = await provisionAuthority();
    const authority = openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" });
    await authority.admitScope(admittedScope());
    authority.close();

    const database = new Database(join(stateDirectory, "lifecycle-receipts.sqlite3"));
    database.pragma("foreign_keys = ON");
    database.prepare(`DELETE FROM namespace_snapshot_members`).run();
    database.prepare(`DELETE FROM namespace_snapshots`).run();
    database.close();

    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" }))
      .toThrowError(/membership/u);
  });

  it.each([
    ["directory mode", async (root: string) => await chmod(root, 0o755)],
    ["database mode", async (root: string) => await chmod(join(root, "lifecycle-receipts.sqlite3"), 0o644)],
    ["key mode", async (root: string) => await chmod(join(root, "lifecycle-receipts.hmac.key"), 0o644)],
    ["key length", async (root: string) => await writeFile(join(root, "lifecycle-receipts.hmac.key"), randomBytes(31), { mode: 0o600 })],
    ["schema version", async (root: string) => {
      const database = new Database(join(root, "lifecycle-receipts.sqlite3"));
      database.pragma("user_version = 2");
      database.close();
    }],
  ] as const)("fails closed on invalid %s", async (_label, corrupt) => {
    const stateDirectory = await provisionAuthority();
    await corrupt(stateDirectory);
    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" }))
      .toThrow();
  });

  it("rejects an unrecognized ledger schema surface", async () => {
    const stateDirectory = await provisionAuthority();
    const database = new Database(join(stateDirectory, "lifecycle-receipts.sqlite3"));
    database.exec(`CREATE TABLE unexpected(value TEXT) STRICT`);
    database.close();

    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" }))
      .toThrowError(/schema mismatch/u);
  });

  it.each([
    ["CHECK", (schema: string) => schema.replace(
      "authority_sequence INTEGER NOT NULL CHECK(authority_sequence>0)",
      "authority_sequence INTEGER NOT NULL",
    )],
    ["UNIQUE", (schema: string) => schema.replace(
      "  UNIQUE(kind,project_session_id,run_id,agent_id,owner_ref_digest,owner_revision),\n",
      "",
    )],
    ["FOREIGN KEY", (schema: string) => schema.replace(
      ",\n  FOREIGN KEY(project_session_id,run_id) REFERENCES admitted_scopes(project_session_id,run_id)\n) STRICT;",
      "\n) STRICT;",
    )],
  ] as const)("rejects a ledger missing its load-bearing %s constraint", async (_label, transformSchema) => {
    const stateDirectory = await provisionAuthority("local-authority", transformSchema);

    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" }))
      .toThrowError(/schema mismatch/u);
  });

  it.each(["INDEX", "VIEW", "TRIGGER"] as const)("rejects an unexpected schema %s", async (kind) => {
    const stateDirectory = await provisionAuthority();
    const database = new Database(join(stateDirectory, "lifecycle-receipts.sqlite3"));
    if (kind === "INDEX") {
      database.exec("CREATE INDEX receipt_digest_index ON receipts(receipt_digest)");
    } else if (kind === "VIEW") {
      database.exec("CREATE VIEW receipt_view AS SELECT * FROM receipts");
    } else {
      database.exec("CREATE TRIGGER receipt_trigger AFTER INSERT ON receipts BEGIN SELECT 1; END");
    }
    database.close();

    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" }))
      .toThrowError(/schema mismatch/u);
  });

  it("opens neither absent nor symlinked authority files", async () => {
    const absent = await mkdtemp(join(tmpdir(), "fabric-local-receipts-absent-"));
    roots.push(absent);
    await chmod(absent, 0o700);
    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory: absent, expectedAuthorityId: "local-authority" }))
      .toThrow();

    const stateDirectory = await provisionAuthority();
    const keyPath = join(stateDirectory, "lifecycle-receipts.hmac.key");
    const target = join(stateDirectory, "actual.key");
    await writeFile(target, randomBytes(32), { mode: 0o600 });
    await unlink(keyPath);
    await symlink(target, keyPath);
    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory, expectedAuthorityId: "local-authority" }))
      .toThrowError(/regular file/u);
  });

  it("rejects a non-regular key and a crossed configured identity", async () => {
    const nonRegular = await provisionAuthority();
    const keyPath = join(nonRegular, "lifecycle-receipts.hmac.key");
    await unlink(keyPath);
    await mkdir(keyPath, { mode: 0o700 });
    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory: nonRegular, expectedAuthorityId: "local-authority" }))
      .toThrowError(/regular file/u);

    const crossed = await provisionAuthority();
    expect(() => openLocalLifecycleReceiptAuthority({ stateDirectory: crossed, expectedAuthorityId: "other-authority" }))
      .toThrowError(/identity mismatch/u);
  });
});
