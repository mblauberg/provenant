import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";
import Database from "better-sqlite3";

import { provisionObserverCredential } from "../../../src/cli/observer-provision.ts";
import type { FabricPaths } from "../../../src/cli/paths.ts";
import {
  privateDiscoveryPaths,
  publishPrivateDiscovery,
  readPrivateDiscovery,
} from "../../../src/daemon/private-discovery.ts";
import { openFabric } from "../../../src/index.ts";
import {
  canonicalProjectPath,
  installSeatGeneration,
  projectKey,
} from "../../../src/cli/seat-store.ts";
import { ROOT_AUTHORITY } from "../../support/stage1-fixture.ts";
import { createCurrentSessionRun } from "../../support/current-session-testkit.ts";

const roots: string[] = [];

afterEach(async () => {
  await Promise.allSettled(roots.splice(0).map(async (root) => await rm(root, { recursive: true, force: true })));
});

describe("observer provisioning freshness gate", () => {
  it("blocks a live stale incumbent before authority or agent registration effects", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-observer-freshness-"));
    roots.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    await Promise.all([
      mkdir(stateDirectory, { recursive: true, mode: 0o700 }),
      mkdir(runtimeDirectory, { recursive: true, mode: 0o700 }),
    ]);
    const paths: FabricPaths = {
      stateDirectory,
      runtimeDirectory,
      databasePath: join(stateDirectory, "fabric.sqlite3"),
      socketPath: join(runtimeDirectory, "fabric.sock"),
    };
    const fabric = await openFabric({ databasePath: paths.databasePath, workspaceRoots: [root] });
    await fabric.close();
    const run = await createCurrentSessionRun({
      databasePath: paths.databasePath,
      workspaceRoot: root,
      runId: "observer-freshness-run",
      chair: { agentId: "chair", authority: ROOT_AUTHORITY },
    });
    const staleIdentity = `sha256:${"0".repeat(64)}`;
    const bootstrapCapability = `afb_${"A".repeat(43)}`;
    const actionId = "observer-freshness-action";
    await publishPrivateDiscovery({
      paths: privateDiscoveryPaths(runtimeDirectory),
      actionId,
      electionGeneration: 1,
      daemonInstanceGeneration: 1,
      socketPath: paths.socketPath,
      pid: process.pid,
      bootstrapCapability,
      lifecycleReceiptAuthorityId: null,
      protocolVersion: 2,
      features: ["rpc", "mcp-bootstrap-credentials.v2", "mcp-bootstrap-result-shape.v1"],
      runtimeBuildIdentity: staleIdentity,
    });
    await Promise.all([
      (async () => {
        await writeFile(join(runtimeDirectory, "daemon-election.lease.json"), `${JSON.stringify({
          schemaVersion: 1,
          actionId,
          electionGeneration: 1,
          status: "succeeded",
          acquiredAt: 1,
          terminalAt: 2,
          code: "BOOTSTRAP_READY",
          message: "fixture incumbent reached ready",
        })}\n`, { mode: 0o600 });
        await writeFile(join(runtimeDirectory, "daemon-election.ready.json"), `${JSON.stringify({
          schemaVersion: 1,
          actionId,
          electionGeneration: 1,
          daemonInstanceGeneration: 1,
          socketPath: paths.socketPath,
          protocolVersion: 2,
          features: ["rpc", "mcp-bootstrap-credentials.v2", "mcp-bootstrap-result-shape.v1"],
          readyAt: 2,
          evidence: { databaseOwned: true, migrationsComplete: true, recoveryComplete: true, socketBound: true },
        })}\n`, { mode: 0o600 });
      })(),
    ]);
    const generation = "1".repeat(64);
    const canonicalRoot = await canonicalProjectPath(root);
    await installSeatGeneration({
      stateDirectory,
      projectPath: canonicalRoot,
      generation,
      expectedPreviousGeneration: null,
      seats: [{
        metadata: {
          schemaVersion: 1,
          projectKey: projectKey(canonicalRoot),
          projectPath: canonicalRoot,
          generation,
          previousGeneration: null,
          projectSessionId: run.projectSessionId,
          sessionRevision: run.sessionRevision,
          sessionGeneration: run.sessionGeneration,
          runId: run.runId,
          runRevision: run.runRevision,
          chairAgentId: run.chairAgentId,
          chairGeneration: run.chairGeneration,
          chairLeaseId: run.chairLeaseId,
          seat: "claude",
          agentId: run.chairAgentId,
          principalGeneration: 1,
          role: "chair",
          expiresAt: ROOT_AUTHORITY.expiresAt,
        },
        credential: run.chairCapability,
      }],
    });

    const beforeDatabase = new Database(paths.databasePath, { readonly: true, fileMustExist: true });
    let authorityCountBefore: { count: number };
    try {
      authorityCountBefore = beforeDatabase.prepare("SELECT COUNT(*) AS count FROM authorities").get() as {
        count: number;
      };
    } finally {
      beforeDatabase.close();
    }

    await expect(provisionObserverCredential({ project: root, paths })).rejects.toMatchObject({
      code: "DAEMON_STALE_BUILD",
    });

    const discovery = await readPrivateDiscovery(privateDiscoveryPaths(runtimeDirectory), paths.socketPath);
    expect(discovery.status).toBe("active");
    if (discovery.status !== "active") throw new Error("live stale incumbent discovery was replaced");
    expect(discovery.owner).toMatchObject({
      state: "active",
      actionId,
      electionGeneration: 1,
      daemonInstanceGeneration: 1,
      pid: process.pid,
    });
    expect(discovery.receipt.pid).toBe(process.pid);
    expect(() => process.kill(discovery.receipt.pid, 0)).not.toThrow();

    const database = new Database(paths.databasePath, { readonly: true, fileMustExist: true });
    try {
      expect(database.prepare("SELECT COUNT(*) AS count FROM authorities").get()).toEqual(authorityCountBefore);
      expect(database.prepare("SELECT COUNT(*) AS count FROM agents WHERE agent_id='fabric-observer'").get())
        .toEqual({ count: 0 });
    } finally {
      database.close();
    }
  });
});
