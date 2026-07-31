import { describe, expect, it, vi } from "vitest";

import { CodexJsonRpcConnection } from "../../src/adapters/providers/codex-json-rpc.ts";
import { settle } from "../shared/deadline-wait.ts";

const FAKE_SERVER = String.raw`
const readline = require("node:readline");
const lines = readline.createInterface({ input: process.stdin });
let nextServerRequestId = 100;
lines.on("line", (line) => {
  const message = JSON.parse(line);
  if (message.method === "initialize") {
    process.stdout.write(JSON.stringify({ id: message.id, result: {} }) + "\n");
  } else if (message.method === "emit") {
    process.stdout.write(JSON.stringify({ id: message.id, result: {} }) + "\n");
    process.stdout.write(JSON.stringify({ method: "thread/compacted", params: { threadId: "thread-1", sequence: 1 } }) + "\n");
    process.stdout.write(JSON.stringify({ method: "thread/compacted", params: { threadId: "thread-1", sequence: 2 } }) + "\n");
  } else if (message.method === "exit") {
    process.stdout.write(JSON.stringify({ id: message.id, result: {} }) + "\n", () => process.exit(7));
  } else if (message.method === "invoke-client-tool") {
    const id = nextServerRequestId++;
    process.stdout.write(JSON.stringify({ id: message.id, result: {} }) + "\n");
    process.stdout.write(JSON.stringify({ id, method: "item/tool/call", params: {
      arguments: {}, callId: "call-1", threadId: "thread-1", tool: "attest", turnId: "turn-1"
    } }) + "\n");
  } else if (message.method === "flood-client-tools") {
    process.stdout.write(JSON.stringify({ id: message.id, result: {} }) + "\n");
    for (let index = 0; index < 17; index += 1) {
      process.stdout.write(JSON.stringify({ id: 200 + index, method: "item/tool/call", params: {
        arguments: {}, callId: "flood-" + index, threadId: "thread-1", tool: "attest", turnId: "turn-1"
      } }) + "\n");
    }
  } else if (message.method === "duplicate-active-client-tool") {
    process.stdout.write(JSON.stringify({ id: message.id, result: {} }) + "\n");
    const request = { id: 500, method: "item/tool/call", params: {
      arguments: {}, callId: "duplicate-active", threadId: "thread-1", tool: "attest", turnId: "turn-1"
    } };
    process.stdout.write(JSON.stringify(request) + "\n");
    process.stdout.write(JSON.stringify(request) + "\n");
    setTimeout(() => process.exit(0), 50);
  } else if (message.method === "duplicate-completed-client-tool") {
    process.stdout.write(JSON.stringify({ id: message.id, result: {} }) + "\n");
    process.stdout.write(JSON.stringify({ id: 600, method: "item/tool/call", params: {
      arguments: {}, callId: "duplicate-completed", threadId: "thread-1", tool: "attest", turnId: "turn-1"
    } }) + "\n");
  } else if (message.id >= 100 && (message.result || message.error)) {
    process.stdout.write(JSON.stringify({ method: "client-tool/result", params: { response: message } }) + "\n");
    if (message.id === 600) {
      process.stdout.write(JSON.stringify({ id: 600, method: "item/tool/call", params: {
        arguments: {}, callId: "duplicate-completed", threadId: "thread-1", tool: "attest", turnId: "turn-1"
      } }) + "\n");
      setTimeout(() => process.exit(0), 50);
    }
  }
});
`;

describe("Codex JSON-RPC notification consumption", () => {
  it("consumes each buffered notification exactly once", async () => {
    const connection = new CodexJsonRpcConnection([process.execPath, "-e", FAKE_SERVER]);
    try {
      await connection.initialize();
      await connection.request("emit", {});
      await settle(20, "fake server buffers both notifications before the first consume");

      await expect(connection.waitForNotification("thread/compacted", () => true, 100))
        .resolves.toMatchObject({ sequence: 1 });
      await expect(connection.waitForNotification("thread/compacted", () => true, 100))
        .resolves.toMatchObject({ sequence: 2 });
      await expect(connection.waitForNotification("thread/compacted", () => true, 20))
        .rejects.toMatchObject({ code: "PROVIDER_RESPONSE_TIMEOUT" });
    } finally {
      await connection.close();
    }
  });

  it("rejects notification waits immediately when the provider exits", async () => {
    const connection = new CodexJsonRpcConnection([process.execPath, "-e", FAKE_SERVER]);
    await connection.initialize();
    const waiting = connection.waitForNotification("turn/completed", () => true, 10_000);
    await connection.request("exit", {});
    await expect(waiting).rejects.toMatchObject({ code: "PROVIDER_EXITED" });
    await connection.close();
  });

  it("attributes a server-requested dynamic tool call and returns its response", async () => {
    const connection = new CodexJsonRpcConnection([process.execPath, "-e", FAKE_SERVER]);
    try {
      await connection.initialize();
      const handler = vi.fn(async (params: Record<string, unknown>) => ({
        contentItems: [{ type: "inputText", text: String(params.callId) }],
        success: true,
      }));
      connection.setServerRequestHandler("item/tool/call", handler);
      await connection.request("invoke-client-tool", {});

      await expect(connection.waitForNotification("client-tool/result", () => true, 100))
        .resolves.toMatchObject({
          response: {
            id: 100,
            result: { contentItems: [{ type: "inputText", text: "call-1" }], success: true },
          },
        });
      expect(handler).toHaveBeenCalledWith({
        arguments: {}, callId: "call-1", threadId: "thread-1", tool: "attest", turnId: "turn-1",
      });
    } finally {
      await connection.close();
    }
  });

  it("bounds provider-originated server requests at sixteen concurrent calls", async () => {
    const connection = new CodexJsonRpcConnection([process.execPath, "-e", FAKE_SERVER]);
    let releaseHandlers: (() => void) | undefined;
    const blocked = new Promise<void>((resolve) => {
      releaseHandlers = resolve;
    });
    try {
      await connection.initialize();
      const handler = vi.fn(async () => {
        await blocked;
        return { contentItems: [], success: true };
      });
      connection.setServerRequestHandler("item/tool/call", handler);
      await connection.request("flood-client-tools", {});

      await expect(connection.waitForNotification(
        "client-tool/result",
        (params) => (params.response as { id?: number }).id === 216,
        100,
      )).resolves.toMatchObject({
        response: {
          id: 216,
          error: { code: -32000, message: "agent-fabric server request capacity exceeded" },
        },
      });
      expect(handler).toHaveBeenCalledTimes(16);
    } finally {
      releaseHandlers?.();
      await connection.close();
    }
  });

  it.each([
    ["active", "duplicate-active-client-tool", true],
    ["completed", "duplicate-completed-client-tool", false],
  ] as const)("fails the provider connection for a duplicate %s server-request ID", async (_state, method, blockHandler) => {
    const connection = new CodexJsonRpcConnection([process.execPath, "-e", FAKE_SERVER]);
    const never = new Promise<never>(() => undefined);
    await connection.initialize();
    const handler = vi.fn(async () => {
      if (blockHandler) return await never;
      return { contentItems: [], success: true };
    });
    connection.setServerRequestHandler("item/tool/call", handler);
    const failed = connection.waitForNotification("never", () => true, 10_000);
    await connection.request(method, {});

    await expect(failed).rejects.toMatchObject({ code: "PROVIDER_PROTOCOL_INVALID" });
    expect(handler).toHaveBeenCalledOnce();
    await connection.close();
  });
});
