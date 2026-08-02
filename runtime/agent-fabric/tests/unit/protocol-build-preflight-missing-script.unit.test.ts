import { vi, describe, expect, it } from "vitest";

const { accessMock } = vi.hoisted(() => ({ accessMock: vi.fn() }));

vi.mock("node:fs/promises", async (importOriginal) => {
  const actual = await importOriginal<typeof import("node:fs/promises")>();
  return { ...actual, access: accessMock };
});

import {
  preflightProtocolBuild,
  ProtocolBuildPreflightError,
} from "../../src/daemon/protocol-build-preflight.js";

describe("protocol build preflight missing mechanism", () => {
  it("refuses when the preflight script is missing", async () => {
    const missing = Object.assign(new Error("missing preflight"), { code: "ENOENT" });
    accessMock.mockRejectedValueOnce(missing);

    const failure = preflightProtocolBuild();

    await expect(failure).rejects.toBeInstanceOf(ProtocolBuildPreflightError);
    await expect(failure).rejects.toMatchObject({
      code: "AGENT_FABRIC_PREFLIGHT_INCOMPLETE",
    });
    await expect(failure).rejects.toThrow(
      "missing preflight script at",
    );
    await expect(failure).rejects.toThrow(
      "repair: git -C ",
    );
    await expect(failure).rejects.toThrow(
      "restore --source=HEAD -- scripts/agent-fabric-protocol-preflight",
    );
  });
});
