import { Duplex } from "node:stream";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  MCP_BOOTSTRAP_CREDENTIALS_FEATURE,
  MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE,
} from "@local/agent-fabric-protocol";
import type {
  BootstrapMcpSeatInput,
  CurrentMcpSeatBindingInput,
} from "../../src/core/contracts.ts";
import { FabricDaemonClient } from "../../src/daemon/rpc-client.ts";
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
          protocolVersion: 2,
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
          executableResolutionVersion: 2,
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

const bootstrapInput: BootstrapMcpSeatInput = {
  canonicalRoot: "/project-one",
  trustRecordDigest: `sha256:${"b".repeat(64)}`,
  seat: "codex",
  expiresAt: "2099-01-01T00:00:00.000Z",
};

const currentBindingInput: CurrentMcpSeatBindingInput = {
  canonicalRoot: "/project-one",
  expectedPreviousGeneration: null,
  generation: "a".repeat(64),
  projectSessionId: "session-one",
  expectedSessionRevision: 1,
  expectedSessionGeneration: 1,
  runId: "run-one",
  expectedRunRevision: 1,
  chairAgentId: "agent-chair",
  expectedChairGeneration: 1,
  chairLeaseId: "lease-one",
  expiresAt: bootstrapInput.expiresAt,
  bindings: [{
    seat: "codex",
    agentId: "agent-chair",
    expectedPrincipalGeneration: 1,
  }],
};

function completeCurrentBindingResult(): Record<string, unknown> {
  return {
    expectedPreviousGeneration: null,
    generation: currentBindingInput.generation,
    projectSessionId: currentBindingInput.projectSessionId,
    sessionRevision: 1,
    sessionGeneration: 1,
    runId: currentBindingInput.runId,
    runRevision: 1,
    chairAgentId: currentBindingInput.chairAgentId,
    chairGeneration: 1,
    chairLeaseId: currentBindingInput.chairLeaseId,
    expiresAt: currentBindingInput.expiresAt,
    credentials: [{
      seat: "codex",
      agentId: "agent-chair",
      expectedPrincipalGeneration: 1,
      capability: "capability-one",
    }],
  };
}

function completeBootstrapResult(): Record<string, unknown> {
  return {
    ...completeCurrentBindingResult(),
    projectId: "project-one",
    canonicalRoot: bootstrapInput.canonicalRoot,
    bootstrapRunDirectory: ".agent-run/bootstrap-one",
    custodyMutated: true,
    credentials: [{
      seat: "codex",
      agentId: "agent-chair",
      expectedPrincipalGeneration: 1,
      capability: "capability-one",
      authorityId: "authority-one",
    }],
  };
}

async function connectFixtureClient(result: Record<string, unknown>): Promise<FabricDaemonClient> {
  const socket = new FixtureDaemonSocket({
    daemonVersion: "0.1.0",
    capabilities: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE, MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE],
    legacyCredentialResult: result,
  });
  return await FabricDaemonClient.connect(
    "/fixture/fabric.sock",
    "afb_test",
    [MCP_BOOTSTRAP_CREDENTIALS_FEATURE, MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE],
    { connect: () => socket as unknown as Socket },
  );
}

describe("timed daemon NDJSON transport", () => {
  it("advertises the current MCP bootstrap credential result shape", () => {
    expect(daemonInitializeResult([]).capabilities).toEqual([
      "rpc",
      MCP_BOOTSTRAP_CREDENTIALS_FEATURE,
      MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE,
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
        requiredCapabilities: [MCP_BOOTSTRAP_CREDENTIALS_FEATURE, MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE],
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
        capabilities: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE, MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE],
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
        capabilities: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE, MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE],
      });
      await transport.close();
      expect(socket.methods).toEqual(["initialize"]);
    }
  });

  it("classifies an invalid bootstrap result as a protocol ambiguity", async () => {
    const socket = new FixtureDaemonSocket({
      daemonVersion: "0.1.0",
      capabilities: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE, MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE],
      legacyCredentialResult: {
        projectId: "project-one",
        canonicalRoot: "/project-one",
        bootstrapRunDirectory: ".agent-run/bootstrap-one",
        generation: "a".repeat(64),
        credentials: [],
      },
    });
    const client = await FabricDaemonClient.connect(
      "/fixture/fabric.sock",
      "afb_test",
      [MCP_BOOTSTRAP_CREDENTIALS_FEATURE, MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE],
      { connect: () => socket as unknown as Socket },
    );

    await expect(client.bootstrapMcpSeat({
      canonicalRoot: "/project-one",
      trustRecordDigest: `sha256:${"b".repeat(64)}`,
      seat: "codex",
      expiresAt: "2099-01-01T00:00:00.000Z",
    })).rejects.toMatchObject({
      code: "DAEMON_PROTOCOL_INVALID",
    });
    await client.close();
  });

  it("rejects a bootstrap result missing its inherited expiry before callers can publish metadata", async () => {
    const result = completeBootstrapResult();
    delete result.expiresAt;
    const client = await connectFixtureClient(result);

    await expect(client.bootstrapMcpSeat(bootstrapInput)).rejects.toMatchObject({
      code: "DAEMON_PROTOCOL_INVALID",
    });
    await client.close();
  });

  it("accepts a complete current MCP seat binding result", async () => {
    const client = await connectFixtureClient(completeCurrentBindingResult());

    await expect(client.bindCurrentMcpSeats(currentBindingInput)).resolves.toMatchObject({
      generation: currentBindingInput.generation,
      projectSessionId: currentBindingInput.projectSessionId,
      runId: currentBindingInput.runId,
      chairLeaseId: currentBindingInput.chairLeaseId,
    });
    await client.close();
  });

  it("accepts a complete bootstrap result with the inherited binding fields", async () => {
    const client = await connectFixtureClient(completeBootstrapResult());

    await expect(client.bootstrapMcpSeat(bootstrapInput)).resolves.toMatchObject({
      projectId: "project-one",
      generation: currentBindingInput.generation,
      credentials: [{ authorityId: "authority-one" }],
    });
    await client.close();
  });

  it.each(["generation", "projectSessionId", "runId", "chairLeaseId"])(
    "rejects a bootstrap result missing its inherited %s field",
    async (field) => {
      const result = completeBootstrapResult();
      delete result[field];
      const client = await connectFixtureClient(result);

      await expect(client.bootstrapMcpSeat(bootstrapInput)).rejects.toMatchObject({
        code: "DAEMON_PROTOCOL_INVALID",
      });
      await client.close();
    },
  );

  it.each([
    ["generation", { generation: "not-a-full-generation-hash" }],
    ["expectedPreviousGeneration", { expectedPreviousGeneration: "not-a-full-generation-hash" }],
    ["projectSessionId", { projectSessionId: "" }],
    ["sessionRevision", { sessionRevision: 0 }],
    ["sessionGeneration", { sessionGeneration: 0 }],
    ["runId", { runId: "" }],
    ["runRevision", { runRevision: 0 }],
    ["chairAgentId", { chairAgentId: "" }],
    ["chairGeneration", { chairGeneration: 0 }],
    ["chairLeaseId", { chairLeaseId: "" }],
    ["expiresAt", { expiresAt: "not-a-timestamp" }],
    ["expiresAt", { expiresAt: "2020-01-01T00:00:00.000Z" }],
  ])("rejects a bootstrap result with an invalid inherited %s field", async (_field, override) => {
    const client = await connectFixtureClient({ ...completeBootstrapResult(), ...override });

    await expect(client.bootstrapMcpSeat(bootstrapInput)).rejects.toMatchObject({
      code: "DAEMON_PROTOCOL_INVALID",
    });
    await client.close();
  });

  it("applies the shared inherited-result validation to current seat binding", async () => {
    const result = completeCurrentBindingResult();
    result.generation = "not-a-full-generation-hash";
    const client = await connectFixtureClient(result);

    await expect(client.bindCurrentMcpSeats(currentBindingInput)).rejects.toMatchObject({
      code: "DAEMON_PROTOCOL_INVALID",
    });
    await client.close();
  });

  it.each([
    ["seat", { seat: "" }],
    ["agentId", { agentId: "" }],
    ["principal generation", { expectedPrincipalGeneration: 0 }],
    ["capability", { capability: "" }],
    ["authorityId", { authorityId: "" }],
  ])("rejects bootstrap results with malformed credential %s", async (_field, override) => {
    const result = completeBootstrapResult();
    result.credentials = [{
      ...(result.credentials as Array<Record<string, unknown>>)[0],
      ...override,
    }];
    const client = await connectFixtureClient(result);

    await expect(client.bootstrapMcpSeat(bootstrapInput)).rejects.toMatchObject({
      code: "DAEMON_PROTOCOL_INVALID",
    });
    await client.close();
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
              protocolVersion: 2,
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
              executableResolutionVersion: 2,
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
        params: { protocolVersion: 2, client: { name: "agent-fabric", version: "0.1.0" }, capabilities: ["rpc"] },
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
              protocolVersion: 2,
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
              executableResolutionVersion: 2,
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
