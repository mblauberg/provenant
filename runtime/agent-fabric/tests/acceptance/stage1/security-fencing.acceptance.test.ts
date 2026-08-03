import { mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { loadFabricConfig } from "../../../src/config/index.ts";
import { FabricError } from "../../../src/errors.ts";
import { openFabric } from "../../../src/index.ts";

import { ManualClock } from "../../support/manual-clock.ts";
import { createCurrentSessionRun } from "../../support/current-session-testkit.ts";
import { createStage1Fixture, ROOT_AUTHORITY } from "../../support/stage1-fixture.ts";

// Contract tests for the Stage 1 security-fencing findings in
// .agent-run/AFAB-001/findings/fable-stage1-contract-challenge.md (A1, A2, A4,
// A5, A6, A7). Each test states the spec clause it enforces. Two tests call
// The chair records revocation, isolation or patch-only proof with the daemon;
// recovery verifies that record rather than trusting the recovering caller.

const cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((close) => close()));
});

async function fixtureWithCleanup() {
  const fixture = await createStage1Fixture();
  cleanup.push(async () => {
    await fixture.fabric.close();
    await rm(fixture.directory, { recursive: true, force: true });
  });
  return fixture;
}

describe("Stage 1 security fencing: authority-gated write scopes (finding A1; FR-005/FR-006, spec §12)", () => {
  it("rejects a write lease whose scope lies outside the actor's source authority", async () => {
    const fixture = await fixtureWithCleanup();
    // Alice's delegated authority is sourcePaths ["src/alice"]
    // (tests/support/stage1-fixture.ts). "src/shared" is outside it.
    await expect(
      fixture.alice.acquireWriteLease({
        scope: ["src/shared"],
        ttlMs: 1_000,
        commandId: "sec:alice:outside-authority",
      }),
    ).rejects.toMatchObject({ name: "FabricError", code: "AUTHORITY_WIDENING" });
  });

  it("rejects an absolute write lease scope even when it resolves inside the actor workspace", async () => {
    const fixture = await fixtureWithCleanup();
    await expect(
      fixture.alice.acquireWriteLease({
        scope: [join(fixture.directory, "src", "alice")],
        ttlMs: 1_000,
        commandId: "sec:alice:outside-workspace",
      }),
    ).rejects.toMatchObject({ name: "FabricError", code: "AUTHORITY_WIDENING" });
  });
});

describe("Stage 1 security fencing: recovery proof is daemon-recorded, not caller-asserted (finding A2; spec §12, AC-005)", () => {
  it("rejects predecessor-terminal recovery evidence asserted by the recovering agent itself", async () => {
    const fixture = await fixtureWithCleanup();
    const lease = await fixture.alice.acquireWriteLease({
      scope: ["src/alice"],
      ttlMs: 1_000,
      commandId: "sec:alice:lease-for-theft",
    });
    fixture.clock.advance(1_001);
    // Bob simply *claims* Alice's session is terminal. Spec §12: the daemon
    // must prove revocation/isolation; a bare caller assertion is not proof.
    await expect(
      fixture.bob.recoverWriteLease({
        leaseId: lease.leaseId,
        expectedGeneration: 1,
        commandId: "sec:bob:self-asserted-terminal",
        evidence: { kind: "predecessor-terminal", agentId: "alice", providerSessionRef: "session-alice" },
      }),
    ).rejects.toMatchObject({ name: "FabricError", code: "CAPABILITY_FORBIDDEN" });
  });

  it("rejects os-isolated recovery evidence with no daemon-recorded proof behind it", async () => {
    const fixture = await fixtureWithCleanup();
    const lease = await fixture.alice.acquireWriteLease({
      scope: ["src/alice"],
      ttlMs: 1_000,
      commandId: "sec:alice:lease-for-theft-2",
    });
    fixture.clock.advance(1_001);
    await expect(
      fixture.bob.recoverWriteLease({
        leaseId: lease.leaseId,
        expectedGeneration: 1,
        commandId: "sec:bob:self-asserted-isolation",
        evidence: { kind: "os-isolated", proofRef: "trust-me" },
      }),
    ).rejects.toMatchObject({ name: "FabricError", code: "CAPABILITY_FORBIDDEN" });
  });

  it("permits recovery only after the chair records the revocation proof (proposed minimal API)", async () => {
    const fixture = await fixtureWithCleanup();
    const lease = await fixture.alice.acquireWriteLease({
      scope: ["src/alice"],
      ttlMs: 1_000,
      commandId: "sec:alice:lease-for-recovery",
    });
    fixture.clock.advance(1_001);
    await fixture.chair.recordRevocationProof({
      leaseId: lease.leaseId,
      generation: 1,
      kind: "predecessor-terminal",
      detail: { agentId: "alice", providerSessionRef: "session-alice" },
      commandId: "sec:chair:record-proof",
    });
    const successor = await fixture.chair.recoverWriteLease({
      leaseId: lease.leaseId,
      expectedGeneration: 1,
      commandId: "sec:bob:recover-with-recorded-proof",
      evidence: { kind: "predecessor-terminal", agentId: "alice", providerSessionRef: "session-alice" },
    });
    expect(successor).toMatchObject({ holderAgentId: "chair", generation: 2, status: "active" });
  });
});

describe("Stage 1 security fencing: capability expiry and revocation fence live clients (finding A4; spec §12, §18)", () => {
  it("rejects operations from an already-connected client once its capability expiry passes", async () => {
    const fixture = await fixtureWithCleanup();
    // Advance the injected clock past the shared fixture expiry without
    // coupling this regression to the host calendar.
    fixture.clock.advance(
      Date.parse(ROOT_AUTHORITY.expiresAt) - fixture.clock.now().getTime() + 1,
    );
    await expect(
      fixture.alice.sendMessage({
        audience: { kind: "agents", agentIds: ["bob"] },
        kind: "request",
        body: "after expiry",
        requiresAck: false,
        dedupeKey: "sec:alice:after-expiry",
      }),
    ).rejects.toMatchObject({ name: "FabricError", code: "AUTHENTICATION_FAILED" });
  });

  it("rejects operations from an already-connected client after explicit revocation (proposed minimal API)", async () => {
    const fixture = await fixtureWithCleanup();
    await fixture.chair.revokeCapability({ agentId: "alice", commandId: "sec:chair:revoke-alice" });
    await expect(
      fixture.alice.sendMessage({
        audience: { kind: "agents", agentIds: ["bob"] },
        kind: "request",
        body: "after revoke",
        requiresAck: false,
        dedupeKey: "sec:alice:after-revoke",
      }),
    ).rejects.toMatchObject({ name: "FabricError", code: "AUTHENTICATION_FAILED" });
    expect(() => fixture.fabric.connect(fixture.capabilities.alice)).toThrowError(
      expect.objectContaining({ code: "AUTHENTICATION_FAILED" }),
    );
  });
});

describe("Stage 1 security fencing: symlink escapes are rejected (finding A5; spec §31, AC-010, NFR: FR-006)", () => {
  it("rejects delegated authority whose path is a symlink escaping the workspace root", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-symlink-authority-"));
    cleanup.push(async () => {
      await rm(directory, { recursive: true, force: true });
    });
    const project = join(directory, "project");
    const outside = join(directory, "outside");
    await mkdir(join(project, "src"), { recursive: true });
    await mkdir(outside, { recursive: true });
    await symlink(outside, join(project, "escape"));

    const databasePath = join(directory, "fabric.sqlite3");
    const clock = new ManualClock();
    const fabric = await openFabric({ databasePath, workspaceRoots: [project], clock: clock.now });
    cleanup.push(async () => {
      await fabric.close();
    });
    const run = await createCurrentSessionRun({
      databasePath,
      workspaceRoot: project,
      runId: "run-symlink",
      chair: {
        agentId: "chair",
        authority: {
          ...ROOT_AUTHORITY,
          workspaceRoots: ["."],
          sourcePaths: ["."],
          artifactPaths: ["."],
        },
      },
    });
    const chair = fabric.connect(run.chairCapability);
    // Textually join(project, "escape") is inside the root; its real target is
    // not. Spec §31: unresolved symlink escapes are rejected.
    await expect(
      chair.delegateAuthority({
        parentAuthorityId: run.chairAuthorityId,
        authority: {
          ...ROOT_AUTHORITY,
          workspaceRoots: ["."],
          sourcePaths: ["escape"],
          artifactPaths: ["."],
          budget: { turns: 1, "cost:USD": 1 },
        },
      }),
    ).rejects.toMatchObject({ name: "FabricError", code: "AUTHORITY_WIDENING" });
  });

  it("rejects an untrusted project workspace root that symlinks outside the global root", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-symlink-config-"));
    cleanup.push(async () => {
      await rm(directory, { recursive: true, force: true });
    });
    const project = join(directory, "project");
    const outside = join(directory, "outside");
    await mkdir(project, { recursive: true });
    await mkdir(outside, { recursive: true });
    await symlink(outside, join(project, "escape"));

    const globalPath = join(directory, "global.yaml");
    const projectPath = join(project, ".agents", "agent-fabric.yaml");
    const writeJson = async (path: string, value: unknown): Promise<void> => {
      await mkdir(dirname(path), { recursive: true });
      await writeFile(path, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 });
    };
    await writeJson(globalPath, {
      schemaVersion: 1,
      allowedAdapters: ["codex"],
      allowedProfiles: ["headless"],
      workspaceRoots: [project],
      limits: { maximumConcurrentProviderTurns: 8 },
    });
    await writeJson(projectPath, {
      schemaVersion: 1,
      workspaceRoots: [join(project, "escape")],
    });
    // AC-010: a project layer must not widen a workspace root; a symlinked
    // "narrowing" whose real path is outside the global root is a widening.
    await expect(loadFabricConfig({ globalPath, projectPath })).rejects.toMatchObject({
      code: "CONFIG_WIDENING_FORBIDDEN",
    });
  });
});

describe("Stage 1 security fencing: expiry fences renewal (finding A6; spec §12)", () => {
  it("rejects renewal of an expired lease even by its holder at the current generation", async () => {
    const fixture = await fixtureWithCleanup();
    const lease = await fixture.alice.acquireWriteLease({
      scope: ["src/alice"],
      ttlMs: 1_000,
      commandId: "sec:alice:lease-expiring",
    });
    fixture.clock.advance(1_001);
    // Proposed minimal error code: LEASE_EXPIRED. Recovery with proof is the
    // only path back after expiry; silent resurrection un-fences a successor.
    await expect(
      fixture.alice.renewWriteLease({
        leaseId: lease.leaseId,
        expectedGeneration: 1,
        ttlMs: 1_000,
        commandId: "sec:alice:renew-after-expiry",
      }),
    ).rejects.toMatchObject({ name: "FabricError", code: "LEASE_EXPIRED" });
  });
});

describe("Stage 1 security fencing: retried commands never leak raw SQLite errors (finding A7; spec §11, NFR-004 shape)", () => {
  it("returns the committed result or a typed FabricError when a second connection retries run creation", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-two-connections-"));
    cleanup.push(async () => {
      await rm(directory, { recursive: true, force: true });
    });
    const databasePath = join(directory, "fabric.sqlite3");
    const clock = new ManualClock();
    const first = await openFabric({ databasePath, workspaceRoots: [directory], clock: clock.now });
    const second = await openFabric({ databasePath, workspaceRoots: [directory], clock: clock.now });
    cleanup.push(async () => {
      await first.close();
      await second.close();
    });

    const creation = {
      runId: "run-retry",
      chair: { agentId: "chair", authority: ROOT_AUTHORITY },
    };
    const original = await createCurrentSessionRun({ databasePath, workspaceRoot: directory, ...creation });
    expect(original.runId).toBe("run-retry");

    // A client that lost the first acknowledgement retries the identical
    // creation through another connection to the same store. Spec §11: "Every
    // transition has a stable command ID and returns the committed result on
    // retry." Proposed minimal API change: createRun accepts a commandId and
    // replays idempotently; at minimum the failure must be a typed
    // FabricError, never a raw SqliteError constraint leak.
    const retry = await createCurrentSessionRun({ databasePath, workspaceRoot: directory, ...creation }).then(
      (result) => ({ kind: "result" as const, result }),
      (error: unknown) => ({ kind: "error" as const, error }),
    );
    if (retry.kind === "error") {
      expect(retry.error).toBeInstanceOf(FabricError);
    } else {
      expect(retry.result.runId).toBe("run-retry");
    }
  });
});
