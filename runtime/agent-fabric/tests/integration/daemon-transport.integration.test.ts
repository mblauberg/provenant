import { Duplex } from "node:stream";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

import { afterEach, describe, expect, it, vi } from "vitest";

import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE } from "@local/agent-fabric-protocol";
import { FabricRemoteError, TimedNdjsonTransport } from "../../src/transport/ndjson-rpc.ts";
import { daemonInitializeResult } from "../../src/transport/daemon-rpc-contract.ts";

const cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  await Promise.allSettled(cleanup.splice(0).map((close) => close()));
});

class NeverConnectingSocket extends Duplex {
  override _read(): void {}
  override _write(_chunk: Buffer, _encoding: BufferEncoding, callback: (error?: Error | null) => void): void {
    callback();
  }
}

class FixtureDaemonSocket extends Duplex {
  readonly methods: string[] = [];
  readonly #daemonVersion: string;
  readonly #capabilities: string[];
  readonly #legacyCredentialResult: Record<string, unknown>;

  constructor(input: {
    daemonVersion: string;
    capabilities: string[];
    legacyCredentialResult: Record<string, unknown>;
  }) {
    super();
    this.#daemonVersion = input.daemonVersion;
    this.#capabilities = input.capabilities;
    this.#legacyCredentialResult = input.legacyCredentialResult;
    queueMicrotask(() => this.emit("connect"));
  }

  override _read(): void {}

  override _write(chunk: Buffer, _encoding: BufferEncoding, callback: (error?: Error | null) => void): void {
    const request = JSON.parse(chunk.toString("utf8")) as { id: string; method: string };
    this.methods.push(request.method);
    const result = request.method === "initialize"
      ? {
          protocolVersion: 1,
          daemonVersion: this.#daemonVersion,
          capabilities: this.#capabilities,
          activeAdapters: [],
          limits: {
            maximumFrameBytes: 1_048_576,
            maximumConnections: 32,
            maximumInFlightPerConnection: 16,
            maximumTotalInFlight: 128,
            maximumClientPending: 32,
            maximumAdapterInFlight: 8,
            idleTimeoutMs: 300_000,
          },
        }
      : this.#legacyCredentialResult;
    this.push(`${JSON.stringify({ id: request.id, result })}\n`);
    callback();
  }

  override _final(callback: (error?: Error | null) => void): void {
    this.push(null);
    this.destroy();
    callback();
  }
}

describe("timed daemon NDJSON transport", () => {
  it("advertises the current MCP bootstrap credential result shape", () => {
    expect(daemonInitializeResult([]).capabilities).toEqual([
      "rpc",
      MCP_BOOTSTRAP_CREDENTIALS_FEATURE,
    ]);
  });

  it("rejects the legacy MCP bootstrap credential result before bootstrap or seat rotation", async () => {
    const legacyCredentialResult = {
      expectedPreviousGeneration: null,
      generation: "a".repeat(64),
      projectSessionId: "session_legacy",
      sessionRevision: 1,
      sessionGeneration: 1,
      runId: "run_legacy",
      runRevision: 1,
      chairAgentId: "agent_chair",
      chairGeneration: 1,
      chairLeaseId: "lease_chair",
      expiresAt: "2026-07-27T00:00:00.000Z",
      credentials: [{
        seat: "codex",
        agentId: "agent_chair",
        expectedPrincipalGeneration: 1,
        capability: `afc_${"a".repeat(43)}`,
      }],
    };
    const socket = new FixtureDaemonSocket({
      daemonVersion: "pre-0636854",
      capabilities: ["rpc"],
      legacyCredentialResult,
    });

    let transport: TimedNdjsonTransport | undefined;
    let failure: unknown;
    try {
      transport = await TimedNdjsonTransport.connect({
        socketPath: "/fixture/fabric.sock",
        capability: "afb_test",
        connectTimeoutMs: 200,
        requestTimeoutMs: 200,
        requiredCapabilities: [MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
      }, {
        connect: () => socket as unknown as Socket,
      });
    } catch (error: unknown) {
      failure = error;
    } finally {
      await transport?.close();
    }

    expect(failure).toMatchObject({
      name: "ProtocolResultShapeFeatureError",
      code: "PROTOCOL_INCOMPATIBLE",
    });
    expect(socket.methods).toEqual(["initialize"]);
  });

  it("attaches builds with unrelated source commits when result-shape tokens match", async () => {
    const legacyCredentialResult = {};
    for (const daemonVersion of [
      "0.1.0+source-commit-aaaaaaaa",
      "0.1.0+source-commit-bbbbbbbb",
    ]) {
      const socket = new FixtureDaemonSocket({
        daemonVersion,
        capabilities: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
        legacyCredentialResult,
      });
      const transport = await TimedNdjsonTransport.connect({
        socketPath: "/fixture/fabric.sock",
        capability: "afb_test",
        requiredCapabilities: [MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
        connectTimeoutMs: 200,
        requestTimeoutMs: 200,
      }, {
        connect: () => socket as unknown as Socket,
      });
      expect(transport.initializeResult).toMatchObject({
        daemonVersion,
        capabilities: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
      });
      await transport.close();
      expect(socket.methods).toEqual(["initialize"]);
    }
  });

  it("bounds connection setup and destroys a socket that never connects", async () => {
    const socket = new NeverConnectingSocket();

    await expect(
      TimedNdjsonTransport.connect(
        { socketPath: "/unused.sock", capability: "capability", connectTimeoutMs: 20, requestTimeoutMs: 100 },
        { connect: () => socket as unknown as Socket },
      ),
    ).rejects.toMatchObject({ code: "DAEMON_CONNECT_TIMEOUT" } satisfies Partial<FabricRemoteError>);
    expect(socket.destroyed).toBe(true);
  });

  it("times out and removes an unanswered request while preserving later wire-compatible calls", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-transport-"));
    const socketPath = join(directory, "fabric.sock");
    const requests: Array<{ id: string; capability: string; method: string; params: Record<string, unknown> }> = [];
    const server = createServer((socket) => {
      const lines = createInterface({ input: socket, crlfDelay: Infinity });
      lines.on("line", (line) => {
        const request = JSON.parse(line) as { id: string; capability: string; method: string; params: Record<string, unknown> };
        requests.push(request);
        if (request.method === "initialize") {
          socket.write(`${JSON.stringify({
            id: request.id,
            result: {
              protocolVersion: 1,
              daemonVersion: "0.1.0",
              capabilities: ["rpc"],
              activeAdapters: [],
              limits: {
                maximumFrameBytes: 1_048_576,
                maximumConnections: 32,
                maximumInFlightPerConnection: 16,
                maximumTotalInFlight: 128,
                maximumClientPending: 32,
                maximumAdapterInFlight: 8,
                idleTimeoutMs: 300_000,
              },
            },
          })}\n`);
          return;
        }
        if (request.method === "second") {
          socket.write(`${JSON.stringify({ id: request.id, result: { ok: true } })}\n`);
        }
      });
    });
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(socketPath, resolve);
    });
    cleanup.push(async () => {
      await new Promise<void>((resolve) => server.close(() => resolve()));
      await rm(directory, { recursive: true, force: true });
    });
    const transport = await TimedNdjsonTransport.connect({
      socketPath,
      capability: "afb_test",
      connectTimeoutMs: 200,
      requestTimeoutMs: 25,
    });
    cleanup.unshift(() => transport.close());

    await expect(transport.call("first", { sequence: 1 })).rejects.toMatchObject({
      code: "DAEMON_REQUEST_TIMEOUT",
    } satisfies Partial<FabricRemoteError>);
    await expect(transport.call("second", { sequence: 2 })).resolves.toEqual({ ok: true });
    expect(requests.map(({ capability, method, params }) => ({ capability, method, params }))).toEqual([
      {
        capability: "afb_test",
        method: "initialize",
        params: { protocolVersion: 1, client: { name: "agent-fabric", version: "0.1.0" }, capabilities: ["rpc"] },
      },
      { capability: "afb_test", method: "first", params: { sequence: 1 } },
      { capability: "afb_test", method: "second", params: { sequence: 2 } },
    ]);
  });

  it("rejects a thirty-third pending client call without writing it", async () => {
    const directory = await mkdtemp(join(tmpdir(), "fabric-transport-pending-"));
    const socketPath = join(directory, "fabric.sock");
    let ordinaryRequests = 0;
    const server = createServer((socket) => {
      const lines = createInterface({ input: socket, crlfDelay: Infinity });
      lines.on("line", (line) => {
        const request = JSON.parse(line) as { id: string; method: string };
        if (request.method === "initialize") {
          socket.write(`${JSON.stringify({
            id: request.id,
            result: {
              protocolVersion: 1,
              daemonVersion: "0.1.0",
              capabilities: ["rpc"],
              activeAdapters: [],
              limits: {
                maximumFrameBytes: 1_048_576,
                maximumConnections: 32,
                maximumInFlightPerConnection: 16,
                maximumTotalInFlight: 128,
                maximumClientPending: 32,
                maximumAdapterInFlight: 8,
                idleTimeoutMs: 300_000,
              },
            },
          })}\n`);
        } else {
          ordinaryRequests += 1;
        }
      });
    });
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(socketPath, resolve);
    });
    cleanup.push(async () => {
      await new Promise<void>((resolve) => server.close(() => resolve()));
      await rm(directory, { recursive: true, force: true });
    });
    const transport = await TimedNdjsonTransport.connect({
      socketPath,
      capability: "afb_test",
      connectTimeoutMs: 200,
      requestTimeoutMs: 5_000,
    });
    cleanup.unshift(() => transport.close());
    const pending = Array.from(
      { length: 32 },
      (_, index) => transport.call("hold", { index }).catch((error: unknown) => error),
    );

    await expect(transport.call("overflow", {})).rejects.toMatchObject({ code: "DAEMON_CLIENT_OVERLOADED" });
    await vi.waitFor(() => expect(ordinaryRequests).toBe(32));
    await transport.close();
    await Promise.allSettled(pending);
  });
});
