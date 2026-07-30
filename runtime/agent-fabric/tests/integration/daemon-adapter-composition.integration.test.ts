import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it, vi } from "vitest";

import { connectFabricDaemon, startFabricDaemon } from "../../src/index.ts";
import { DeadlineTimeoutError, waitUntil } from "../shared/deadline-wait.ts";
import { DAEMON_ROOT_AUTHORITY } from "../support/daemon-testkit.ts";
import { createCurrentSessionRun } from "../support/current-session-testkit.ts";
import { callTool, spawnMcpProxy } from "../support/mcp-testkit.ts";

const fakeAdapter = fileURLToPath(new URL("../support/daemon-fake-adapter.ts", import.meta.url));

describe("daemon adapter composition", () => {
  it("validates enabled GitHub hosted checks in the authoritative daemon process", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-daemon-github-"));
    const stateDirectory = join(directory, "state");
    const runtimeDirectory = join(directory, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const projectRoot = join(directory, "project");
    const executable = join(directory, "gh-fixture");
    const body = "#!/bin/sh\nexit 0\n";
    await mkdir(projectRoot);
    await writeFile(executable, body, { encoding: "utf8", mode: 0o700 });
    await chmod(executable, 0o700);
    let unexpected: Awaited<ReturnType<typeof startFabricDaemon>> | undefined;
    let rejected = false;
    try {
      try {
        unexpected = await startFabricDaemon({
          databasePath: join(stateDirectory, "fabric.sqlite3"),
          stateDirectory,
          runtimeDirectory,
          socketPath,
          workspaceRoots: [projectRoot],
          githubHostedChecks: {
            enabled: true,
            executable,
            executableDigest: `sha256:${"0".repeat(64)}`,
            hostname: "github.com",
            repository: "example/project",
            canonicalRepositoryRoot: projectRoot,
          },
        });
      } catch {
        rejected = true;
      }
      expect(rejected).toBe(true);
      expect(createHash("sha256").update(body).digest("hex")).not.toBe("0".repeat(64));
    } finally {
      if (unexpected !== undefined) await unexpected.stop();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("passes an explicitly activated adapter into the authoritative daemon and journals a real fake-process action", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-daemon-adapter-"));
    const stateDirectory = join(directory, "state");
    const runtimeDirectory = join(directory, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const databasePath = join(stateDirectory, "fabric.sqlite3");
    const daemon = await startFabricDaemon({
      databasePath, stateDirectory, runtimeDirectory, socketPath,
      adapters: { fake: { command: [process.execPath, "--import", "tsx", fakeAdapter], environment: { FAKE_ADAPTER_JOURNAL: join(directory, "fake-journal.json") } } },
    });
    const bootstrap = await connectFabricDaemon({ socketPath, capability: daemon.bootstrapCapability });
    try {
      const run = await createCurrentSessionRun({
        databasePath,
        workspaceRoot: directory,
        runId: "run-daemon-adapter",
        chair: { agentId: "chair", authority: { ...DAEMON_ROOT_AUTHORITY, disclosure: { level: "scoped", scopes: ["local", "approved-provider"] } as const } },
      });
      const chair = await connectFabricDaemon({ socketPath, capability: run.chairCapability });
      try {
        const action = await chair.dispatchProviderAction({ certifyingReview: null, adapterId: "fake", actionId: "daemon-adapter:1", operation: "steer", payload: { instruction: "bounded review" }, commandId: "daemon-adapter:dispatch:1" });
        expect(action).toMatchObject({ actionId: "daemon-adapter:1", status: "terminal", executionCount: 1, effectCount: 1 });
      } finally {
        await chair.close();
      }
    } finally {
      await bootstrap.close();
      await daemon.stop();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("runs a task-bound account-default provider review through the generated MCP surface", async () => {
    vi.stubEnv("USER", "fabric-provider-user");
    vi.stubEnv("LOGNAME", "fabric-provider-login");
    const directory = await mkdtemp(join(tmpdir(), "af-ephemeral-"));
    const stateDirectory = join(directory, "state");
    const runtimeDirectory = join(directory, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const databasePath = join(stateDirectory, "fabric.sqlite3");
    const daemon = await startFabricDaemon({
      databasePath,
      stateDirectory,
      runtimeDirectory,
      socketPath,
      adapters: {
        fake: {
          command: [process.execPath, "--import", "tsx", fakeAdapter],
          environment: {
            FAKE_ADAPTER_JOURNAL: join(directory, "fake-journal.json"),
            FAKE_ADAPTER_EPHEMERAL_SPAWN: "1",
            FAKE_ADAPTER_ACCOUNT_DEFAULT_MODEL: "1",
            FAKE_ADAPTER_REPORT_LOGIN_IDENTITY: "1",
            FAKE_ADAPTER_EPHEMERAL_SPAWN_DELAY_MS: "2000",
          },
          modelPolicy: {
            allowedFamilies: ["fake"],
            allowedModelPatterns: ["fake-reviewer-*"],
            requiresExplicitModel: false,
          },
        },
      },
    });
    const bootstrap = await connectFabricDaemon({ socketPath, capability: daemon.bootstrapCapability });
    let chairProxy: Awaited<ReturnType<typeof spawnMcpProxy>> | undefined;
    try {
      const rootAuthority = {
        ...DAEMON_ROOT_AUTHORITY,
        disclosure: { level: "scoped", scopes: ["local", "approved-provider"] } as const,
      };
      const run = await createCurrentSessionRun({
        databasePath,
        workspaceRoot: directory,
        runId: "run-daemon-ephemeral-review",
        chair: { agentId: "chair", authority: rootAuthority },
      });
      const chair = await connectFabricDaemon({ socketPath, capability: run.chairCapability });
      try {
        const reviewAuthority = await chair.delegateAuthority({
          parentAuthorityId: run.chairAuthorityId,
          authority: {
            ...rootAuthority,
            budget: { turns: 1, "cost:USD": 1 },
          },
          commandId: "daemon-ephemeral-review:authority",
        });
        chairProxy = await spawnMcpProxy({
          socketPath,
          capability: run.chairCapability,
          label: "daemon-ephemeral-review-chair",
        });
        const task = await callTool(chairProxy.client, "fabric_task_create", {
          taskId: "daemon-ephemeral-review",
          authorityId: reviewAuthority.authorityId,
          eligibleAgentIds: ["chair"],
          participantAgentIds: ["chair"],
          objective: "review the current implementation",
          baseRevision: "daemon-ephemeral-review-base",
          commandId: "daemon-ephemeral-review:task",
        });
        expect(task.isError, task.text).toBe(false);
        const dispatchInput = {
          adapterId: "fake",
          actionId: "daemon-ephemeral-review:spawn",
          operation: "spawn",
          taskId: "daemon-ephemeral-review",
          authorityId: reviewAuthority.authorityId,
          payload: {
            modelFamily: "fake",
            prompt: "Review the current implementation read-only.",
            cwd: "src",
          },
          commandId: "daemon-ephemeral-review:dispatch",
          certifyingReview: null,
        };
        const dispatchStartedAt = Date.now();
        const outcome = await callTool(chairProxy.client, "fabric_provider_action_dispatch", dispatchInput);
        expect(outcome.isError, outcome.text).toBe(false);
        expect(Date.now() - dispatchStartedAt).toBeLessThan(750);
        expect(outcome.structured).toMatchObject({
          actionRef: { adapterId: "fake", actionId: "daemon-ephemeral-review:spawn" },
          status: "prepared",
          executionCount: 0,
          effectCount: 0,
        });
        expect(outcome.structured).not.toHaveProperty("providerAnswer");

        const exactReplay = await callTool(chairProxy.client, "fabric_provider_action_dispatch", dispatchInput);
        expect(exactReplay.isError, exactReplay.text).toBe(false);
        expect(exactReplay.structured).toEqual(outcome.structured);
        const changedAction = await callTool(chairProxy.client, "fabric_provider_action_dispatch", {
          ...dispatchInput,
          actionId: "daemon-ephemeral-review:changed-action",
        });
        expect(changedAction.isError).toBe(true);
        expect(changedAction.structured).toMatchObject({ code: "DEDUPE_CONFLICT" });
        const liveReconcile = await callTool(chairProxy.client, "fabric_provider_action_reconcile", {
          adapterId: "fake",
          actionId: "daemon-ephemeral-review:spawn",
          expectedActionKind: "non-review",
          commandId: "daemon-ephemeral-review:live-reconcile",
        });
        expect(liveReconcile.isError, liveReconcile.text).toBe(false);
        expect(liveReconcile.structured.status).toBe("dispatched");

        await chairProxy.close();
        const reconnectedChairProxy = await spawnMcpProxy({
          socketPath,
          capability: run.chairCapability,
          label: "daemon-ephemeral-review-chair-reconnected",
        });
        chairProxy = reconnectedChairProxy;

        const timeoutMs = 10_000;
        const description = "Action terminal status";
        let lastStatus = "not observed";
        let terminal: Awaited<ReturnType<typeof callTool>> | undefined;
        try {
          await waitUntil(async () => {
            const observed = await callTool(reconnectedChairProxy.client, "fabric_provider_action_read", {
              adapterId: "fake",
              actionId: "daemon-ephemeral-review:spawn",
              expectedActionKind: "non-review",
            });
            expect(observed.isError, observed.text).toBe(false);
            lastStatus = String(observed.structured.status);
            if (observed.structured.status !== "terminal") return false;
            terminal = observed;
            return true;
          }, timeoutMs, description);
        } catch (error: unknown) {
          if (error instanceof DeadlineTimeoutError) {
            throw new Error(
              `Action did not reach terminal status (awaited ${String(timeoutMs)}ms), ` +
              `last status: ${lastStatus}`,
            );
          }
          throw error;
        }
        expect(terminal?.structured).toMatchObject({
          actionRef: { adapterId: "fake", actionId: "daemon-ephemeral-review:spawn" },
          status: "terminal",
          executionCount: 1,
          effectCount: 1,
          providerAnswer: "review:fake:account-default:fabric-provider-user:fabric-provider-login",
          resultDigest: expect.stringMatching(/^sha256:[a-f0-9]{64}$/u),
        });
      } finally {
        await chair.close();
      }
    } finally {
      await Promise.allSettled([chairProxy?.close(), bootstrap.close()]);
      await daemon.stop();
      await rm(directory, { recursive: true, force: true });
      vi.unstubAllEnvs();
    }
  });
});
