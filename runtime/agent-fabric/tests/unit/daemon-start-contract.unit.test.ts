import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE } from "@local/agent-fabric-protocol";

vi.mock("node:net", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:net")>();
  const { Duplex } = await import("node:stream");
  const { FABRIC_PROTOCOL_LIMITS } = await import("../../src/transport/bounded-ndjson.ts");

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
              protocolVersion: 1,
              daemonVersion: "legacy-private-daemon",
              capabilities: process.env.AGENT_FABRIC_TEST_HANDSHAKE_WITH_BOOTSTRAP === "1"
                ? ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE]
                : ["rpc"],
              ...(process.env.AGENT_FABRIC_TEST_HANDSHAKE_WITHOUT_EXECUTABLE_REVISION === "1"
                ? {}
                : { executableResolutionVersion: 2 }),
              limits: FABRIC_PROTOCOL_LIMITS,
              activeAdapters: [],
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

import { FabricDaemonClient, startFabricDaemon } from "../../src/daemon/client.ts";

const roots: string[] = [];

afterEach(async () => {
  vi.unstubAllEnvs();
  await Promise.all(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

describe("production daemon bootstrap contract", () => {
  it("rejects a direct attach whose handshake cannot prove executable resolution revision 2", async () => {
    vi.stubEnv("AGENT_FABRIC_TEST_HANDSHAKE_WITH_BOOTSTRAP", "1");
    vi.stubEnv("AGENT_FABRIC_TEST_HANDSHAKE_WITHOUT_EXECUTABLE_REVISION", "1");

    await expect(
      FabricDaemonClient.connect("/fixture/fabric.sock", `afb_${"a".repeat(43)}`),
    ).rejects.toMatchObject({
      code: "DAEMON_PROTOCOL_MISMATCH",
      message: expect.stringContaining("executable resolution revision 2"),
    });
  });

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
        protocolVersion: 1,
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
        message: expect.stringContaining(MCP_BOOTSTRAP_CREDENTIALS_FEATURE),
      },
    });
  });

  it("rejects an incumbent whose handshake cannot prove executable resolution revision 2", async () => {
    vi.stubEnv("AGENT_FABRIC_TEST_HANDSHAKE_WITH_BOOTSTRAP", "1");
    vi.stubEnv("AGENT_FABRIC_TEST_HANDSHAKE_WITHOUT_EXECUTABLE_REVISION", "1");
    const root = await mkdtemp(join(tmpdir(), "fabric-daemon-stale-resolution-"));
    roots.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    const databasePath = join(stateDirectory, "fabric.sqlite3");
    const actionId = "stale-resolution-daemon";
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
        message: "stale daemon reached ready",
      })}\n`, { mode: 0o600 }),
      writeFile(join(runtimeDirectory, "daemon-election.ready.json"), `${JSON.stringify({
        schemaVersion: 1,
        actionId,
        electionGeneration: 1,
        daemonInstanceGeneration: 1,
        socketPath,
        protocolVersion: 1,
        features: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
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
        message: expect.stringContaining("executable resolution"),
      },
    });
  });
});
