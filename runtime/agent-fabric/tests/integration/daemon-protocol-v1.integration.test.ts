import { connect, type Socket } from "node:net";
import { createInterface } from "node:readline";

import { afterEach, describe, expect, it } from "vitest";

import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE } from "@local/agent-fabric-protocol";
import { FABRIC_PROTOCOL_LIMITS } from "../../src/transport/bounded-ndjson.ts";
import { createDaemonFixture } from "../support/daemon-testkit.ts";

const cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  await Promise.allSettled(cleanup.splice(0).map((close) => close()));
});

async function rawConnection(socketPath: string): Promise<{
  socket: Socket;
  request(value: unknown): Promise<Record<string, unknown>>;
}> {
  const socket = connect(socketPath);
  await new Promise<void>((resolve, reject) => {
    socket.once("connect", resolve);
    socket.once("error", reject);
  });
  const lines = createInterface({ input: socket, crlfDelay: Infinity });
  const responses: Array<(value: Record<string, unknown>) => void> = [];
  lines.on("line", (line) => responses.shift()?.(JSON.parse(line) as Record<string, unknown>));
  cleanup.unshift(async () => {
    lines.close();
    socket.destroy();
  });
  return {
    socket,
    async request(value: unknown) {
      const response = new Promise<Record<string, unknown>>((resolve) => responses.push(resolve));
      socket.write(`${JSON.stringify(value)}\n`);
      return await response;
    },
  };
}

describe("daemon protocol v1 negotiation", () => {
  it("fails closed before initialization and on unsupported versions, then binds capability", async () => {
    const fixture = await createDaemonFixture("run-protocol-v1");
    cleanup.push(fixture.cleanup);
    const raw = await rawConnection(fixture.socketPath);

    await expect(raw.request({
      id: "pre-init",
      capability: fixture.daemon.bootstrapCapability,
      method: "getRun",
      params: { runId: fixture.run.runId },
    })).resolves.toMatchObject({ error: { code: "DAEMON_NOT_INITIALIZED" } });

    await expect(raw.request({
      id: "wrong-version",
      capability: fixture.daemon.bootstrapCapability,
      method: "initialize",
      params: { protocolVersion: 2, client: { name: "test", version: "1" }, capabilities: ["rpc"] },
    })).resolves.toMatchObject({ error: { code: "DAEMON_PROTOCOL_UNSUPPORTED" } });

    await expect(raw.request({
      id: "initialize",
      capability: fixture.daemon.bootstrapCapability,
      method: "initialize",
      params: { protocolVersion: 1, client: { name: "test", version: "1" }, capabilities: ["rpc"] },
    })).resolves.toMatchObject({
      result: {
        protocolVersion: 1,
        daemonVersion: "0.1.0",
        capabilities: ["rpc", MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
        activeAdapters: [],
        limits: FABRIC_PROTOCOL_LIMITS,
      },
    });

    await expect(raw.request({
      id: "switched-capability",
      capability: fixture.peerCapability,
      method: "getRun",
      params: { runId: fixture.run.runId },
    })).resolves.toMatchObject({ error: { code: "DAEMON_CAPABILITY_MISMATCH" } });

    await expect(raw.request({
      id: "bootstrap-scope",
      capability: fixture.daemon.bootstrapCapability,
      method: "getRun",
      params: { runId: fixture.run.runId },
    })).resolves.toMatchObject({ error: { code: "BOOTSTRAP_SCOPE_VIOLATION" } });
  });

  it("returns a typed error instead of accepting a thirty-third connection", async () => {
    const fixture = await createDaemonFixture("run-connection-limit");
    cleanup.push(fixture.cleanup);
    const held = await Promise.all(Array.from({ length: 29 }, () => rawConnection(fixture.socketPath)));
    expect(held).toHaveLength(29);

    const overflow = connect(fixture.socketPath);
    cleanup.unshift(async () => {
      overflow.destroy();
    });
    const line = await new Promise<Record<string, unknown>>((resolve, reject) => {
      const lines = createInterface({ input: overflow, crlfDelay: Infinity });
      overflow.once("error", reject);
      lines.once("line", (value) => resolve(JSON.parse(value) as Record<string, unknown>));
    });
    expect(line).toMatchObject({ id: "connection", error: { code: "DAEMON_CONNECTION_LIMIT" } });
  });
});
