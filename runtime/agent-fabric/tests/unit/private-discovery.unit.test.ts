import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { readDiscoveryReceipt } from "../../src/cli/mcp-provision.ts";
import type { FabricPaths } from "../../src/cli/paths.ts";
import {
  privateDiscoveryPaths,
  readPrivateDiscovery,
} from "../../src/daemon/private-discovery.ts";

const roots: string[] = [];

afterEach(async () => {
  await Promise.all(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

describe("private daemon discovery receipt", () => {
  it("uses the shared parser for the current CLI discovery receipt", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-cli-discovery-"));
    roots.push(root);
    const runtimeDirectory = join(root, "runtime");
    await mkdir(runtimeDirectory, { mode: 0o700 });
    const paths: FabricPaths = {
      stateDirectory: join(root, "state"),
      runtimeDirectory,
      databasePath: join(root, "state", "fabric.sqlite3"),
      socketPath: join(runtimeDirectory, "fabric.sock"),
    };
    await writeFile(join(runtimeDirectory, "fabric-v1.discovery.json"), `${JSON.stringify({
      schemaVersion: 1,
      socketPath: paths.socketPath,
      pid: process.pid,
      bootstrapCapability: `afb_${"a".repeat(43)}`,
      lifecycleReceiptAuthorityId: null,
      protocolVersion: 1,
      features: ["rpc", "mcp-bootstrap-credentials.v2"],
      futureCompatibleEvidence: { schemaVersion: 1 },
    })}\n`, { mode: 0o600 });

    await expect(readDiscoveryReceipt(paths)).resolves.toMatchObject({
      protocolVersion: 1,
      features: ["rpc", "mcp-bootstrap-credentials.v2"],
    });
  });

  it("accepts unknown receipt fields from a newer compatible writer", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-private-discovery-"));
    roots.push(root);
    const runtimeDirectory = join(root, "runtime");
    await mkdir(runtimeDirectory, { mode: 0o700 });
    const paths = privateDiscoveryPaths(runtimeDirectory);
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const bootstrapCapability = `afb_${"a".repeat(43)}`;
    await Promise.all([
      writeFile(paths.receiptPath, `${JSON.stringify({
        schemaVersion: 1,
        socketPath,
        pid: process.pid,
        bootstrapCapability,
        lifecycleReceiptAuthorityId: null,
        protocolVersion: 1,
        features: ["rpc", "mcp-bootstrap-credentials.v2"],
        futureCompatibleEvidence: { schemaVersion: 1 },
      })}\n`, { mode: 0o600 }),
      writeFile(paths.ownerPath, `${JSON.stringify({
        schemaVersion: 1,
        state: "active",
        actionId: "future-compatible-receipt",
        electionGeneration: 1,
        daemonInstanceGeneration: 1,
        socketPath,
        pid: process.pid,
        bootstrapCapabilityHash: createHash("sha256").update(bootstrapCapability).digest("hex"),
        updatedAt: 1,
        exitCode: null,
        signal: null,
      })}\n`, { mode: 0o600 }),
    ]);

    await expect(readPrivateDiscovery(paths, socketPath)).resolves.toMatchObject({
      status: "active",
      receipt: {
        protocolVersion: 1,
        features: ["rpc", "mcp-bootstrap-credentials.v2"],
      },
    });
  });
});
