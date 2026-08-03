import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import { providerConformanceEvidence, type ProviderConformanceObservation } from "../../src/adapters/provider-conformance.ts";

describe("provider conformance smoke evidence", () => {
  it("projects the activation observation without turning version or digest into policy", () => {
    const observation: ProviderConformanceObservation = {
      identity: {
        adapterId: "agy",
        canonicalPath: "/provider/agy",
        regularFile: true,
        ownerUid: 501,
        mode: 0o755,
        sha256: "a".repeat(64),
        assurance: "full-vendor-identity",
        signing: [{ path: "/provider/agy", teamId: "TEAM", identifier: "agy" }],
      },
      interface: {
        adapterId: "agy",
        conformant: true,
        probe: "bounded-help-version",
        version: "observed-current-version",
      },
    };

    expect(providerConformanceEvidence(observation)).toEqual({
      canonicalPath: "/provider/agy",
      assurance: "full-vendor-identity",
      signingIdentities: [{ path: "/provider/agy", teamId: "TEAM", identifier: "agy" }],
      observedVersion: "observed-current-version",
      observedDigest: "a".repeat(64),
    });
  });

  it("makes the read-only smoke consume and emit the shared conformance observation", async () => {
    const source = await readFile(resolve(import.meta.dirname, "../../smoke/provider-adapter-readonly.mjs"), "utf8");

    expect(source).toContain("const providerConformance = await verifyProviderConformance({");
    expect(source).toContain("providerConformance: providerConformanceEvidence(providerConformance)");
    expect(source).toContain('"opencode-acp": "adapters/providers/optional/opencode-acp.ts"');
    expect(source).toContain('adapterId === "opencode-acp"');
    expect(source).toContain('providerConfig: "unchanged"');
    expect(source).toContain('fabricCapability: "not-provided"');
    expect(source).toContain('fileURLToPath(new URL("../../../", import.meta.url))');
    expect(source).toContain("executableOverride: implementation.executable_override");
    expect(source).toContain("activeAdapters.includes(adapterId)");
    expect(source.indexOf("const providerConfigBefore")).toBeLessThan(source.indexOf("await verifyProviderConformance"));
    expect(source).not.toContain("executableSha256");
  });
});
