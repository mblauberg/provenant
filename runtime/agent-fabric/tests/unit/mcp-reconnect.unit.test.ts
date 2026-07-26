import {
  FABRIC_OPERATIONS,
  ProtocolResultShapeFeatureError,
  ProtocolTransportError,
} from "@local/agent-fabric-protocol";
import { describe, expect, it } from "vitest";

import {
  errorPayload,
  isRecoverableProtocolInterruption,
  retryRecoveredProtocolCall,
} from "../../src/mcp/server.ts";

describe("MCP recovered protocol retry", () => {
  it("exposes only the typed result-shape incompatibility before generic TypeError handling", () => {
    expect(errorPayload(
      new ProtocolResultShapeFeatureError(["mcp-bootstrap-credentials.v2"]),
    )).toMatchObject({
      code: "PROTOCOL_INCOMPATIBLE",
      message: expect.stringContaining("mcp-bootstrap-credentials.v2"),
    });

    const unrelated = new TypeError("internal argument detail") as TypeError & { code: string };
    unrelated.code = "ERR_INTERNAL_ARGUMENT";
    expect(errorPayload(unrelated)).toEqual({
      code: "MCP_INPUT_INVALID",
      message: "internal argument detail",
    });
  });

  it("recognizes a queued saturation timeout as proven unsubmitted while the transport remains open", () => {
    const timeout = new ProtocolTransportError(
      "PROTOCOL_TIMEOUT",
      "queued protocol request timed out: fabric.v1.message.send",
      { requestState: "queued" },
    );

    expect(isRecoverableProtocolInterruption(timeout, false)).toBe(true);
    expect(isRecoverableProtocolInterruption(new ProtocolTransportError(
      "PROTOCOL_TIMEOUT",
      "in-flight protocol request timed out: fabric.v1.message.send",
      { requestState: "in-flight" },
    ), false)).toBe(false);
  });

  it("turns a second timeout into actionable reconnect guidance", async () => {
    await expect(retryRecoveredProtocolCall(
      async () => {
        throw new ProtocolTransportError("PROTOCOL_TIMEOUT", "in-flight retry timed out");
      },
      FABRIC_OPERATIONS.receiveMessages,
      { limit: 10, visibilityTimeoutMs: 30_000 },
    )).rejects.toMatchObject({
      code: "RECONNECT_REQUIRED",
      action: "The fabric_message_receive outcome is unknown and no delivery was acknowledged. Wait at least 30000 ms (the requested visibilityTimeoutMs) before retrying fabric_message_receive.",
    });
  });

  it("preserves a non-disconnect transport error from the retried operation", async () => {
    for (const code of [
      "PROTOCOL_RESULT_INVALID",
      "PROTOCOL_FEATURE_UNAVAILABLE",
      "PROTOCOL_OVERLOADED",
    ] as const) {
      const failure = new ProtocolTransportError(code, `injected ${code}`);

      await expect(retryRecoveredProtocolCall(async () => {
        throw failure;
      })).rejects.toBe(failure);
      expect(errorPayload(failure)).toEqual({
        code,
        message: "Agent Fabric protocol request failed",
      });
    }
  });
});
