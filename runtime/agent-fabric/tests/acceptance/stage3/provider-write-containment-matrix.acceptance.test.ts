import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { compileProviderPayload } from "../../../src/authority/authority-compiler.ts";
import { claudeProviderOptions } from "../../../src/adapters/providers/claude-agent-sdk.ts";
import {
  codexThreadConfiguration,
  codexTurnContainment,
} from "../../../src/adapters/providers/codex-app-server.ts";
import { AUTHORITY_ACTION_VOCABULARY, openFabric } from "../../../src/index.ts";
import { TEST_AUTHORITY_V2_FIELDS } from "../../support/authority-v2-testkit.ts";
import { createCurrentSessionRun } from "../../support/current-session-testkit.ts";
import { ManualClock } from "../../support/manual-clock.ts";
import {
  EXECUTION_MODES,
  type ContainmentCase,
  wouldDenyClaude,
  wouldDenyCodex,
} from "../../support/provider-write-containment-testkit.ts";

const lifecycleAdapter = fileURLToPath(
  new URL("../../support/lifecycle-fake-provider.ts", import.meta.url),
);
const matrixUrl = new URL("../../fixtures/provider-write-containment/matrix.json", import.meta.url);
const cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map(async (close) => await close()));
});

async function matrixCases(): Promise<ContainmentCase[]> {
  const value = JSON.parse(await readFile(matrixUrl, "utf8")) as { cases: ContainmentCase[] };
  return value.cases;
}

function authority(expiresAt = "2099-01-01T00:00:00.000Z", externalEffects = false) {
  return {
    ...TEST_AUTHORITY_V2_FIELDS,
    workspaceRoots: ["."],
    sourcePaths: ["src"],
    artifactPaths: [".agent-run"],
    actions: [...AUTHORITY_ACTION_VOCABULARY],
    disclosure: { level: "scoped", scopes: ["local", "approved-provider"] } as const,
    expiresAt,
    budget: { turns: 40, provider_calls: 40 },
    ...(externalEffects ? { deployment: { allowed: true as const, targets: ["test-target"] } } : {}),
  };
}

function compiledWritePayload(workspaceRoot: string): Record<string, unknown> {
  return compileProviderPayload({
    authority: authority(),
    workspaceRoot: () => workspaceRoot,
    payload: { cwd: "src", prompt: "projection-only matrix probe" },
    trustedProjection: { kind: "workspace-write-offline", workspacePath: "src" },
    now: Date.parse("2026-07-10T00:00:00.000Z"),
    validateCurrent: true,
  });
}

async function fabricFixture(options: Readonly<{
  externalEffects?: boolean;
  expiringWorker?: boolean;
}> = {}) {
  const directory = await mkdtemp(join(tmpdir(), "fabric-provider-containment-"));
  cleanup.push(async () => await rm(directory, { recursive: true, force: true }));
  await mkdir(join(directory, "src"), { recursive: true });
  const workspaceRoot = await realpath(directory);
  const databasePath = join(directory, "fabric.sqlite3");
  const journalPath = join(directory, "provider-journal.json");
  const clock = new ManualClock();
  const workerExpiry = options.expiringWorker
    ? new Date(clock.now().getTime() + 1_000).toISOString()
    : "2099-01-01T00:00:00.000Z";
  const fabric = await openFabric({
    databasePath,
    fabricSocketPath: join(directory, "fabric.sock"),
    workspaceRoots: [workspaceRoot],
    adapters: {
      lifecycle: {
        command: [process.execPath, "--import", "tsx", lifecycleAdapter],
        environment: { LIFECYCLE_FAKE_JOURNAL: journalPath },
      },
    },
    clock: clock.now,
  });
  cleanup.push(async () => await fabric.close());
  const runId = "provider-containment";
  const run = await createCurrentSessionRun({
    databasePath,
    workspaceRoot,
    runId,
    chair: {
      agentId: "chair",
      authority: authority("2099-01-01T00:00:00.000Z", options.externalEffects),
    },
  });
  const chair = fabric.connect(run.chairCapability);
  const workerAuthority = await chair.delegateAuthority({
    parentAuthorityId: run.chairAuthorityId,
    authority: {
      ...authority(workerExpiry, options.externalEffects),
      budget: { turns: 20, provider_calls: 20 },
    },
  });
  const successorAuthority = await chair.delegateAuthority({
    parentAuthorityId: run.chairAuthorityId,
    authority: { ...authority(), budget: { turns: 5, provider_calls: 5 } },
  });
  const workerRegistration = await chair.registerAgent({
    agentId: "worker",
    authorityId: workerAuthority.authorityId,
    adapterId: "lifecycle",
    providerSessionRef: "containment-session",
  });
  await chair.registerAgent({
    agentId: "successor",
    authorityId: successorAuthority.authorityId,
  });
  const worker = fabric.connect(workerRegistration.capability);
  const task = await chair.createTask({
    taskId: "containment-task",
    authorityId: workerAuthority.authorityId,
    eligibleAgentIds: ["worker", "successor"],
    proposedOwnerAgentId: "worker",
    participantAgentIds: ["chair", "worker", "successor"],
    objective: "exercise provider containment admission",
    baseRevision: "test-base",
    commandId: "containment-task:create",
  });
  const activeTask = await worker.claimTask({
    taskId: task.taskId,
    expectedRevision: task.revision,
    commandId: "containment-task:claim",
  });
  const lease = await chair.acquireWriteLease({
    scope: ["src"],
    ttlMs: 60_000,
    taskId: activeTask.taskId,
    commandId: "containment-lease:acquire",
  });
  return {
    activeTask,
    chair,
    clock,
    databasePath,
    fabric,
    journalPath,
    lease,
    runId,
    workerAuthority,
    workspaceRoot,
  };
}

async function dispatch(
  fixture: Awaited<ReturnType<typeof fabricFixture>>,
  mode: typeof EXECUTION_MODES[number],
  actionId: string,
  payload: Record<string, unknown>,
): Promise<unknown> {
  if (mode === "fresh") {
    return await fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "lifecycle",
      actionId,
      operation: "spawn",
      taskId: fixture.activeTask.taskId,
      authorityId: fixture.workerAuthority.authorityId,
      payload: {
        cwd: "src",
        modelFamily: "anthropic",
        prompt: "containment admission probe",
        maxTurns: 1,
        ...payload,
      },
      commandId: `${actionId}:dispatch`,
    });
  }
  return await fixture.chair.dispatchProviderAction({
    certifyingReview: null,
    adapterId: "lifecycle",
    actionId,
    operation: "send_turn",
    payload: {
      agentId: "worker",
      providerSessionGeneration: 1,
      taskId: fixture.activeTask.taskId,
      cwd: "src",
      prompt: "containment admission probe",
      ...payload,
    },
    commandId: `${actionId}:dispatch`,
  });
}

async function setupLifecycle(
  fixture: Awaited<ReturnType<typeof fabricFixture>>,
  mode: typeof EXECUTION_MODES[number],
): Promise<void> {
  if (mode === "resume") {
    await dispatch(fixture, "resume", `lifecycle-warmup:${mode}`, {});
  }
  await dispatch(fixture, "resume", `lifecycle-setup:${mode}`, {});
  expect(existsSync(fixture.journalPath)).toBe(true);
  await rm(fixture.journalPath, { force: true });
}

async function storedPayload(databasePath: string, actionId: string): Promise<Record<string, unknown>> {
  const database = new Database(databasePath, { readonly: true });
  try {
    const row = database.prepare(
      "SELECT payload_json FROM provider_actions WHERE run_id=? AND adapter_id='lifecycle' AND action_id=?",
    ).get("provider-containment", actionId) as { payload_json: string } | undefined;
    if (row === undefined) throw new Error(`missing provider action ${actionId}`);
    return JSON.parse(row.payload_json) as Record<string, unknown>;
  } finally {
    database.close();
  }
}

describe("provider-write containment matrix evidence", () => {
  describe("Channel B projection-only (no syscall or provider claim)", () => {
    it.each(EXECUTION_MODES)("models the requested vendor settings for %s turns", async (mode) => {
      const directory = await mkdtemp(join(tmpdir(), "fabric-containment-projection-"));
      cleanup.push(async () => await rm(directory, { recursive: true, force: true }));
      await mkdir(join(directory, "src"), { recursive: true });
      const payload = compiledWritePayload(await realpath(directory));
      const claude = claudeProviderOptions(payload, mode === "resume" ? "claude-session" : undefined);
      const codex = {
        ...codexThreadConfiguration(payload, mode === "resume" ? "resume" : "start"),
        ...codexTurnContainment(payload),
      };
      const cases = (await matrixCases()).filter((entry) => entry.failureMapClass === "T");

      for (const entry of cases) {
        const expectedDenials = entry.expected.map((tuple) => tuple.status === "denied");
        expect(
          entry.expected.map((tuple) => wouldDenyClaude(claude, tuple)),
          `Claude projection-only ${entry.id}:${mode}`,
        ).toEqual(expectedDenials);
        const codexResults = entry.expected.map((tuple) => wouldDenyCodex(codex, tuple));
        if (entry.id === "denied-path-and-credential-reads") {
          expect(codexResults, `Codex has no projected read fence ${entry.id}:${mode}`)
            .toEqual(entry.expected.map(() => false));
        } else if (entry.id === "synthetic-secret-exfiltration") {
          expect(codexResults, `Codex env is empty but credential reads are unfenced ${entry.id}:${mode}`)
            .toEqual([true, true, false]);
        } else {
          expect(codexResults, `Codex projection-only ${entry.id}:${mode}`).toEqual(expectedDenials);
        }
      }
    });
  });

  describe("Channel A admission-verified", () => {
    it.each(EXECUTION_MODES)("rejects raw controls before %s adapter invocation", async (mode) => {
      const fixture = await fabricFixture();
      await expect(dispatch(fixture, mode, `raw-controls:${mode}`, {
        sandbox: "danger-full-access",
        approvalPolicy: "on-request",
        allowedTools: ["Bash", "WebFetch"],
        environments: [{ inherit: true }],
      })).rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
      expect(existsSync(fixture.journalPath)).toBe(false);
    });

    it.each(EXECUTION_MODES)("rejects external-effect write authority before %s adapter invocation", async (mode) => {
      const fixture = await fabricFixture({ externalEffects: true });
      await expect(dispatch(fixture, mode, `external-effects:${mode}`, {}))
        .rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
      expect(existsSync(fixture.journalPath)).toBe(false);
    });

    it.each(EXECUTION_MODES)("rejects a revoked provider principal before %s adapter invocation", async (mode) => {
      const fixture = await fabricFixture();
      await setupLifecycle(fixture, mode);
      await fixture.chair.revokeCapability({
        agentId: "worker",
        commandId: `lifecycle-revoked:${mode}:revoke`,
      });
      await expect(dispatch(fixture, "resume", `lifecycle-revoked:${mode}`, {}))
        .rejects.toMatchObject({ code: "AUTHENTICATION_FAILED" });
      expect(existsSync(fixture.journalPath)).toBe(false);
    });

    it.each(EXECUTION_MODES)("rejects an expired provider principal before %s adapter invocation", async (mode) => {
      const fixture = await fabricFixture({ expiringWorker: true });
      await setupLifecycle(fixture, mode);
      fixture.clock.advance(1_001);
      await expect(dispatch(fixture, "resume", `lifecycle-expired:${mode}`, {}))
        .rejects.toMatchObject({ code: "AUTHENTICATION_FAILED" });
      expect(existsSync(fixture.journalPath)).toBe(false);
    });

    it.each(EXECUTION_MODES)("rejects the stale owner after a generation change before %s adapter invocation", async (mode) => {
      const fixture = await fabricFixture();
      await setupLifecycle(fixture, mode);
      await fixture.chair.revokeCapability({
        agentId: "worker",
        commandId: `lifecycle-owner:${mode}:revoke`,
      });
      const proof = await fixture.chair.recordTaskOwnerRecoveryProof({
        taskId: fixture.activeTask.taskId,
        ownerLeaseGeneration: fixture.activeTask.ownerLeaseGeneration,
        kind: "predecessor-terminal",
        detail: { agentId: "worker" },
        commandId: `lifecycle-owner:${mode}:proof`,
      });
      const recovered = await fixture.chair.recoverTaskOwner({
        taskId: fixture.activeTask.taskId,
        expectedRevision: fixture.activeTask.revision,
        expectedOwnerLeaseGeneration: fixture.activeTask.ownerLeaseGeneration,
        successorAgentId: "successor",
        proofId: proof.proofId,
        commandId: `lifecycle-owner:${mode}:recover`,
      });
      expect(recovered.ownerLeaseGeneration).toBe(fixture.activeTask.ownerLeaseGeneration + 1);
      await expect(dispatch(fixture, "resume", `lifecycle-owner:${mode}`, {}))
        .rejects.toMatchObject({ code: "AUTHENTICATION_FAILED" });
      expect(existsSync(fixture.journalPath)).toBe(false);
    });
  });

  describe("case 19 shipped read-only downgrade", () => {
    it.each(EXECUTION_MODES)("dispatches %s without retaining write permission after lease removal", async (mode) => {
      const fixture = await fabricFixture();
      await setupLifecycle(fixture, mode);
      await fixture.chair.releaseWriteLease({
        leaseId: fixture.lease.leaseId,
        expectedGeneration: fixture.lease.generation,
        commandId: `lifecycle-lease-removed:${mode}:release`,
      });
      const actionId = `lifecycle-lease-removed:${mode}`;
      await expect(dispatch(fixture, "resume", actionId, {})).resolves.toMatchObject({ status: "terminal" });
      expect(existsSync(fixture.journalPath)).toBe(true);
      const payload = await storedPayload(fixture.databasePath, actionId);
      expect(payload).toMatchObject({
        sandbox: "read-only",
        readOnlyRoot: join(fixture.workspaceRoot, "src"),
      });
      expect(payload).not.toHaveProperty("executionProfile");
      expect(payload).not.toHaveProperty("writeRoot");
    });
  });
});
