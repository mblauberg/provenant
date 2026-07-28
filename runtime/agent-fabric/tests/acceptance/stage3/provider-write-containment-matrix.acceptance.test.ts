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
  BOUND_FIXTURE_VARIABLES,
  CONTAINMENT_CASE_IDS,
  EXECUTION_MODES,
  admitsOnlyPilotWrites,
  assertDistinctTempRoots,
  type ContainmentCase,
  wouldDenyClaude,
  wouldDenyCodex,
} from "../../support/provider-write-containment-testkit.ts";

const lifecycleAdapter = fileURLToPath(
  new URL("../../support/lifecycle-fake-provider.ts", import.meta.url),
);
const matrixUrl = new URL("../../fixtures/provider-write-containment/matrix.json", import.meta.url);
const ledgerUrl = new URL("../../fixtures/provider-write-containment/coverage-ledger.json", import.meta.url);
const unresolvedUrl = new URL("../../fixtures/provider-write-containment/unverified-golden.json", import.meta.url);
const specUrl = new URL("../../../../../docs/specs/agent-fabric/provider-write-containment.md", import.meta.url);
const cleanup: Array<() => Promise<void>> = [];
const normativeFailureMapClasses = Object.freeze({
  "positive-owned-write": "T",
  "filesystem-path-escapes": "T",
  "filesystem-subprocess-escapes": "T",
  "filesystem-native-edit-escapes": "T",
  "filesystem-git-c-escape": "T",
  "filesystem-symlink-escapes": "T",
  "filesystem-symlink-swap": "T",
  "git-metadata-mutations": "T",
  "unreceipted-temp-writes": "T",
  "denied-path-and-credential-reads": "T",
  "network-tool-egress": "T",
  "hostile-settings-cannot-widen": "T",
  "synthetic-secret-exfiltration": "T",
  "admission-rejects-raw-controls": "A",
  "admission-rejects-external-effects": "A",
  "lifecycle-revoked": "L",
  "lifecycle-expired": "L",
  "lifecycle-owner-generation-changed": "L",
  "lifecycle-write-lease-removed": "L",
  "lifecycle-restart-before-execution": "L",
  "lifecycle-restart-after-provider-acceptance": "L",
} satisfies Record<typeof CONTAINMENT_CASE_IDS[number], ContainmentCase["failureMapClass"]>);
const normativeLifecycleStructures = Object.freeze({
  "lifecycle-revoked": {
    expected: [{ operation: "create", target: "$LIFECYCLE_TARGET", status: "succeeded" }],
    turnStatus: "denied-before-dispatch",
  },
  "lifecycle-expired": {
    expected: [{ operation: "create", target: "$LIFECYCLE_TARGET", status: "succeeded" }],
    turnStatus: "denied-before-dispatch",
  },
  "lifecycle-owner-generation-changed": {
    expected: [{ operation: "create", target: "$LIFECYCLE_TARGET", status: "succeeded" }],
    turnStatus: "denied-before-dispatch",
  },
  "lifecycle-write-lease-removed": {
    expected: [{ operation: "create", target: "$LIFECYCLE_TARGET", status: "succeeded" }],
    turnStatus: "read-only-dispatched",
  },
  "lifecycle-restart-before-execution": {
    expected: [{ operation: "create", target: "$LIFECYCLE_TARGET", status: "admitted" }],
    turnStatus: "denied-before-dispatch",
  },
  "lifecycle-restart-after-provider-acceptance": {
    expected: [{ operation: "create", target: "$LIFECYCLE_TARGET", status: "accepted" }],
    turnStatus: "denied-before-dispatch",
    acceptedEffectStatuses: ["succeeded", "no-effect"],
  },
} satisfies Partial<Record<typeof CONTAINMENT_CASE_IDS[number], Partial<ContainmentCase>>>);

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
  describe("anti-silent-skip invariants", () => {
    it("transcribes exactly 21 case IDs in normative spec order with live line anchors", async () => {
      const cases = await matrixCases();
      const spec = await readFile(specUrl, "utf8");
      const specLines = spec.split("\n");
      const specRows = [...spec.matchAll(/^\| `([^`]+)` \|.*\| (.*) \|$/gmu)];
      const specCaseIds = specRows.map((match) => match[1]);

      expect(cases.map((entry) => entry.id)).toStrictEqual([...CONTAINMENT_CASE_IDS]);
      expect(specCaseIds).toStrictEqual([...CONTAINMENT_CASE_IDS]);
      expect(cases).toHaveLength(21);
      for (const entry of cases) {
        expect(specLines[entry.specLine - 1], `stale specLine for ${entry.id}`).toContain(`\`${entry.id}\``);
        const row = specRows.find((match) => match[1] === entry.id);
        expect(row, `missing normative row for ${entry.id}`).toBeDefined();
        const normativeResult = row?.[2] ?? "";
        const normativeTurnStatus = /turn status `([^`]+)`/u.exec(normativeResult)?.[1];
        const failureMapClass = normativeFailureMapClasses[entry.id];
        expect(entry.failureMapClass, `failureMapClass drift for ${entry.id}`).toBe(failureMapClass);
        expect(entry.turnStatus, `turnStatus drift for ${entry.id}`).toBe(normativeTurnStatus);
        if (failureMapClass === "T") {
          const normativeTuples = [...normativeResult.matchAll(
            /`([^`]+)` → `([^`]+)` → `([^`]+)`/gu,
          )].map((match) => ({
            operation: match[1],
            target: match[2],
            status: match[3],
          }));
          expect(entry.expected, `expected tuple drift for ${entry.id}`).toStrictEqual(normativeTuples);
        } else if (failureMapClass === "A") {
          expect(entry.expected, `admission tuples drift for ${entry.id}`).toStrictEqual([]);
        } else {
          const lifecycleId = entry.id as keyof typeof normativeLifecycleStructures;
          expect(entry, `lifecycle structure drift for ${entry.id}`).toMatchObject(
            normativeLifecycleStructures[lifecycleId],
          );
        }
      }
    });

    it("contains exactly one fresh and resume ledger key for every case", async () => {
      const ledger = JSON.parse(await readFile(ledgerUrl, "utf8")) as {
        executions: Record<string, string>;
      };
      const expectedKeys = CONTAINMENT_CASE_IDS.flatMap((caseId) =>
        EXECUTION_MODES.map((mode) => `${caseId}:${mode}`)
      );
      expect(Object.keys(ledger.executions)).toStrictEqual(expectedKeys);
      expect(Object.keys(ledger.executions)).toHaveLength(42);
      expect(new Set(Object.values(ledger.executions))).toEqual(new Set([
        "admission-verified",
        "projection-only",
        "not-verified",
      ]));
    });

    it("binds every referenced fixture variable and keeps host TMPDIR distinct", async () => {
      const spec = await readFile(specUrl, "utf8");
      const referenced = [...new Set(spec.match(/\$[A-Z][A-Z0-9_]*/gu) ?? [])].sort();
      expect(referenced).toStrictEqual([...BOUND_FIXTURE_VARIABLES].sort());
      expect(spec).toContain("`$GIT_COMMON` to");
      expect(spec).toContain("`$CLAUDE_CONFIG_DIR` to");
      expect(spec).toContain("`$CREDENTIALS` to that same directory");
      expect(spec).toContain("`$LINK_SWAP` is");
      expect(spec).toContain("`$HOST_TEMP_TARGET` is literally");
      expect(spec).toContain("`$TMPDIR` is the `TMPDIR`");

      const privateTemp = await mkdtemp(join(tmpdir(), "fabric-containment-private-temp-"));
      cleanup.push(async () => await rm(privateTemp, { recursive: true, force: true }));
      expect(() => assertDistinctTempRoots(tmpdir(), privateTemp)).not.toThrow();
      expect(() => assertDistinctTempRoots(privateTemp, privateTemp)).toThrow(
        "$TMPDIR and $PRIVATE_TEMP must have different canonical paths",
      );
    });

    it("matches unresolved verdicts to the separate committed golden", async () => {
      const ledger = JSON.parse(await readFile(ledgerUrl, "utf8")) as {
        executions: Record<string, string>;
      };
      const golden = JSON.parse(await readFile(unresolvedUrl, "utf8")) as {
        notVerified: string[];
      };
      const keysWith = (verdict: string): string[] => Object.entries(ledger.executions)
        .filter(([, value]) => value === verdict)
        .map(([key]) => key);
      expect(keysWith("not-verified")).toStrictEqual(golden.notVerified);
    });

    it("asserts exact ledger keys and verdicts per channel", async () => {
      const ledger = JSON.parse(await readFile(ledgerUrl, "utf8")) as {
        executions: Record<string, string>;
      };
      // Both fresh admission cases are proved by equally strong tests below:
      // each asserts CAPABILITY_FORBIDDEN before adapter invocation and that no
      // journal was written. Their resume halves stay unverified because resume
      // ancestry is not proved, which is a property of the mode, not the case.
      const expectedAdmissionVerified = [
        "admission-rejects-external-effects:fresh",
        "admission-rejects-raw-controls:fresh",
      ];
      const admissionVerified = Object.entries(ledger.executions)
        .filter(([, value]) => value === "admission-verified")
        .map(([key]) => key)
        .sort();
      expect(admissionVerified).toStrictEqual(expectedAdmissionVerified);
    });
  });

  describe("Channel B projection-only (no syscall or provider claim)", () => {
    it("treats a filesystem-root grant as unadmitted settings, not as a denied write", () => {
      const ownedWrite = {
        operation: "create",
        target: "$PILOT/owned.txt",
        status: "succeeded",
      };

      // Whether the settings are the ones we meant to request.
      expect(admitsOnlyPilotWrites(["/pilot"])).toBe(true);
      for (const wider of [["/"], ["//"], ["/./"], ["/.."], [""], ["/pilot", "/"], []]) {
        expect(admitsOnlyPilotWrites(wider), `${JSON.stringify(wider)} is wider than the pilot`)
          .toBe(false);
      }

      // Whether the vendor would deny the write. A root grant permits it, so the
      // projection must not report a denial: scoring the widest possible grant
      // as contained is exactly the false assurance this matrix exists to avoid.
      expect(wouldDenyClaude({
        sandbox: { filesystem: { allowWrite: ["/"] } },
      }, ownedWrite)).toBe(false);
      expect(wouldDenyCodex({
        sandboxPolicy: { writableRoots: ["/"] },
      }, ownedWrite)).toBe(false);
      expect(wouldDenyClaude({
        sandbox: { filesystem: { allowWrite: ["/pilot", "/"] } },
      }, ownedWrite)).toBe(false);

      // An empty allow-list grants nothing, so the write really is denied.
      expect(wouldDenyClaude({ sandbox: { filesystem: { allowWrite: [] } } }, ownedWrite))
        .toBe(true);
      expect(wouldDenyCodex({ sandboxPolicy: { writableRoots: [] } }, ownedWrite)).toBe(true);
    });

    it("requests exactly the admitted pilot write root from both vendors", async () => {
      const directory = await mkdtemp(join(tmpdir(), "fabric-containment-admitted-"));
      cleanup.push(async () => await rm(directory, { recursive: true, force: true }));
      await mkdir(join(directory, "src"), { recursive: true });
      const payload = compiledWritePayload(await realpath(directory));

      const claude = claudeProviderOptions(payload) as {
        sandbox?: { filesystem?: { allowWrite?: string[] } };
      };
      expect(admitsOnlyPilotWrites(claude.sandbox?.filesystem?.allowWrite ?? [])).toBe(true);

      const codex = codexTurnContainment(payload) as {
        sandboxPolicy?: { writableRoots?: string[] };
      };
      expect(admitsOnlyPilotWrites(codex.sandboxPolicy?.writableRoots ?? [])).toBe(true);
    });

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
        const claudeResults = entry.expected.map((tuple) => wouldDenyClaude(claude, tuple));
        if (entry.id === "filesystem-symlink-escapes" || entry.id === "filesystem-symlink-swap") {
          expect(claudeResults, `Claude symlink enforcement is not projected ${entry.id}:${mode}`)
            .toEqual(entry.expected.map(() => false));
        } else if (entry.id === "denied-path-and-credential-reads") {
          expect(claudeResults, `Claude linked and hardlinked reads are not projected ${entry.id}:${mode}`)
            .toEqual([true, false, true, true, false]);
        } else {
          expect(claudeResults, `Claude projection-only ${entry.id}:${mode}`).toEqual(expectedDenials);
        }
        const codexResults = entry.expected.map((tuple) => wouldDenyCodex(codex, tuple));
        if (
          entry.id === "filesystem-symlink-escapes" ||
          entry.id === "filesystem-symlink-swap" ||
          entry.id === "denied-path-and-credential-reads"
        ) {
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
    it.each(EXECUTION_MODES)(
      "rejects raw controls before %s adapter invocation (resume ancestry remains unverified)",
      async (mode) => {
      const fixture = await fabricFixture();
      await expect(dispatch(fixture, mode, `raw-controls:${mode}`, {
        sandbox: "danger-full-access",
        approvalPolicy: "on-request",
        allowedTools: ["Bash", "WebFetch"],
        environments: [{ inherit: true }],
      })).rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
      expect(existsSync(fixture.journalPath)).toBe(false);
      },
    );

    it.each(EXECUTION_MODES)(
      "rejects external-effect write authority before %s adapter invocation (resume ancestry remains unverified)",
      async (mode) => {
      const fixture = await fabricFixture({ externalEffects: true });
      await expect(dispatch(fixture, mode, `external-effects:${mode}`, {}))
        .rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
      expect(existsSync(fixture.journalPath)).toBe(false);
      },
    );

    it("proves revoked-principal admission without claiming a matrix fresh/resume execution", async () => {
      const fixture = await fabricFixture();
      await setupLifecycle(fixture, "fresh");
      await fixture.chair.revokeCapability({
        agentId: "worker",
        commandId: "lifecycle-revoked:mechanism:revoke",
      });
      await expect(dispatch(fixture, "resume", "lifecycle-revoked:mechanism", {}))
        .rejects.toMatchObject({ code: "AUTHENTICATION_FAILED" });
      expect(existsSync(fixture.journalPath)).toBe(false);
    });

    it("proves expired-principal admission without claiming a matrix fresh/resume execution", async () => {
      const fixture = await fabricFixture({ expiringWorker: true });
      await setupLifecycle(fixture, "fresh");
      fixture.clock.advance(1_001);
      await expect(dispatch(fixture, "resume", "lifecycle-expired:mechanism", {}))
        .rejects.toMatchObject({ code: "AUTHENTICATION_FAILED" });
      expect(existsSync(fixture.journalPath)).toBe(false);
    });

    // Named for what it asserts. It exercises the owner-recovery mechanism —
    // revoke, prove, recover, generation increments — and does NOT prove that a
    // stale owner is rejected: `recordTaskOwnerRecoveryProof` requires the
    // predecessor capability to be revoked first, so any later rejection cannot
    // be attributed to staleness rather than to that revocation. The stale-owner
    // claim stays unverified in the ledger rather than resting on this test.
    it("proves owner recovery without claiming a matrix fresh/resume execution", async () => {
      const fixture = await fabricFixture();
      await setupLifecycle(fixture, "fresh");
      await fixture.chair.revokeCapability({
        agentId: "worker",
        commandId: "lifecycle-owner:mechanism:revoke",
      });
      const proof = await fixture.chair.recordTaskOwnerRecoveryProof({
        taskId: fixture.activeTask.taskId,
        ownerLeaseGeneration: fixture.activeTask.ownerLeaseGeneration,
        kind: "predecessor-terminal",
        detail: { agentId: "worker" },
        commandId: "lifecycle-owner:mechanism:proof",
      });
      const recovered = await fixture.chair.recoverTaskOwner({
        taskId: fixture.activeTask.taskId,
        expectedRevision: fixture.activeTask.revision,
        expectedOwnerLeaseGeneration: fixture.activeTask.ownerLeaseGeneration,
        successorAgentId: "successor",
        proofId: proof.proofId,
        commandId: "lifecycle-owner:mechanism:recover",
      });
      expect(recovered.ownerLeaseGeneration).toBe(fixture.activeTask.ownerLeaseGeneration + 1);
    });
  });

  describe("case 19 shipped read-only downgrade", () => {
    it("pins the mechanism without claiming a matrix fresh/resume execution", async () => {
      const fixture = await fabricFixture();
      await setupLifecycle(fixture, "fresh");
      await fixture.chair.releaseWriteLease({
        leaseId: fixture.lease.leaseId,
        expectedGeneration: fixture.lease.generation,
        commandId: "lifecycle-lease-removed:mechanism:release",
      });
      const actionId = "lifecycle-lease-removed:mechanism";
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
