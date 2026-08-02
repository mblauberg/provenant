import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE,
} from "@local/agent-fabric-protocol";

vi.mock("node:net", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:net")>();
  const { Duplex } = await import("node:stream");
    const { FABRIC_PROTOCOL_LIMITS } = await import("../../src/transport/bounded-ndjson.ts");
    const { EXECUTABLE_RESOLUTION_VERSION } = await import("../../src/transport/daemon-rpc-contract.ts");

  class LegacyPrivateDaemonSocket extends Duplex {
    constructor() {
      super();
      queueMicrotask(() => this.emit("connect"));
    }

    override _read(): void {}

    override _write(
      chunk: Buffer,
      _encoding: BufferEncoding,
      callback: (error?: Error | null) => void,
    ): void {
      for (const line of chunk.toString("utf8").trim().split("\n")) {
        const request = JSON.parse(line) as { id: string; method: string };
        if (request.method !== "initialize") continue;
        this.push(`${JSON.stringify({
          id: request.id,
          result: {
            protocolVersion: 2,
            daemonVersion: "legacy-private-daemon",
            capabilities: ["rpc"],
            limits: FABRIC_PROTOCOL_LIMITS,
            activeAdapters: [],
            executableResolutionVersion: EXECUTABLE_RESOLUTION_VERSION,
          },
        })}\n`);
      }
      callback();
    }

    override _final(callback: (error?: Error | null) => void): void {
      this.push(null);
      callback();
    }
  }

  return {
    ...actual,
    connect: () => new LegacyPrivateDaemonSocket(),
  };
});

import { startFabricDaemon } from "../../src/daemon/client.ts";

const roots: string[] = [];

afterEach(async () => {
  await Promise.all(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

describe("production daemon bootstrap contract", () => {
  it("rejects an incumbent missing the bootstrap credential result-shape feature", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-daemon-start-contract-"));
    roots.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const databasePath = join(stateDirectory, "fabric.sqlite3");
    const actionId = "legacy-private-daemon";
    const bootstrapCapability = `afb_${"a".repeat(43)}`;
    const bootstrapCapabilityHash = createHash("sha256").update(bootstrapCapability).digest("hex");
    await Promise.all([
      mkdir(stateDirectory, { mode: 0o700 }),
      mkdir(runtimeDirectory, { mode: 0o700 }),
    ]);
    await Promise.all([
      writeFile(join(runtimeDirectory, "fabric-v1.discovery.json"), `${JSON.stringify({
        schemaVersion: 1,
        socketPath,
        pid: process.pid,
        bootstrapCapability,
        lifecycleReceiptAuthorityId: null,
      })}\n`, { mode: 0o600 }),
      writeFile(join(runtimeDirectory, "fabric-v1.discovery-owner.json"), `${JSON.stringify({
        schemaVersion: 1,
        state: "active",
        actionId,
        electionGeneration: 1,
        daemonInstanceGeneration: 1,
        socketPath,
        pid: process.pid,
        bootstrapCapabilityHash,
        updatedAt: 1,
        exitCode: null,
        signal: null,
      })}\n`, { mode: 0o600 }),
      writeFile(join(runtimeDirectory, "daemon-election.lease.json"), `${JSON.stringify({
        schemaVersion: 1,
        actionId,
        electionGeneration: 1,
        status: "succeeded",
        acquiredAt: 1,
        terminalAt: 2,
        code: "BOOTSTRAP_READY",
        message: "legacy daemon reached ready",
      })}\n`, { mode: 0o600 }),
      writeFile(join(runtimeDirectory, "daemon-election.ready.json"), `${JSON.stringify({
        schemaVersion: 1,
        actionId,
        electionGeneration: 1,
        daemonInstanceGeneration: 1,
        socketPath,
        protocolVersion: 2,
        features: ["rpc"],
        readyAt: 2,
        evidence: {
          databaseOwned: true,
          migrationsComplete: true,
          recoveryComplete: true,
          socketBound: true,
        },
      })}\n`, { mode: 0o600 }),
    ]);

    const result = await startFabricDaemon({
      databasePath,
      stateDirectory,
      runtimeDirectory,
      socketPath,
      workspaceRoots: [root],
    }).then((handle) => {
      handle.release();
      return { status: "attached" as const };
    }, (error: unknown) => ({ status: "rejected" as const, error }));

    expect(result).toMatchObject({
      status: "rejected",
      error: {
        code: "BOOTSTRAP_INCOMPATIBLE_INCUMBENT",
        message: expect.stringContaining(MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE),
      },
    });
  });
});
