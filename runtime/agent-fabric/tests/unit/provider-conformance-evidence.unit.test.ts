import { readFile } from "node:fs/promises";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

import {
  providerIdentityAssuranceForPolicy,
  supportsCertifyingAnswerBearingLeg,
  type ProviderIdentityAssurance,
} from "../../src/adapters/provider-identity.ts";
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

  it("preserves lockfile install attestation in the conformance projection", () => {
    const observation: ProviderConformanceObservation = {
      identity: {
        adapterId: "lockfile-provider",
        canonicalPath: "/provider/lockfile-provider",
        regularFile: true,
        ownerUid: 501,
        mode: 0o755,
        sha256: "b".repeat(64),
        assurance: "lockfile-install-attestation",
        signing: [],
      },
      interface: {
        adapterId: "lockfile-provider",
        conformant: true,
        probe: "bounded-help-version",
        version: "observed-current-version",
      },
    };

    expect(providerConformanceEvidence(observation).assurance).toBe("lockfile-install-attestation");
  });

  it.each([
    ["apple-designated", "full-vendor-identity"],
    ["cursor-partial-signed-helpers", "partial-signed-helpers"],
    ["owner-controlled-install-root", "owner-controlled-install-root"],
    ["lockfile-install-attestation", "lockfile-install-attestation"],
  ] as const)("keeps %s policy and %s assurance in parity", (policy, assurance) => {
    expect(providerIdentityAssuranceForPolicy(policy)).toBe(assurance);
  });

  it("does not map an unknown policy into an assurance", () => {
    expect(providerIdentityAssuranceForPolicy("unknown-policy")).toBeUndefined();
  });

  it.each([
    ["full-vendor-identity", true],
    ["lockfile-install-attestation", true],
    ["partial-signed-helpers", false],
    ["owner-controlled-install-root", false],
  ] as const)("%s certification eligibility is %s", (assurance: ProviderIdentityAssurance, eligible) => {
    expect(supportsCertifyingAnswerBearingLeg(assurance)).toBe(eligible);
  });

  it("makes the read-only smoke consume and emit the shared conformance observation", async () => {
    const source = await readFile(resolve(import.meta.dirname, "../../smoke/provider-adapter-readonly.mjs"), "utf8");

    expect(source).toContain("const providerConformance = await verifyProviderConformance({");
    expect(source).toContain("providerConformance: providerConformanceEvidence(providerConformance)");
    expect(source).toContain('"opencode-acp": "adapters/providers/optional/opencode-acp.ts"');
    expect(source).toContain('adapterId === "opencode-acp"');
    expect(source).toContain('providerConfig: "unchanged"');
    expect(source).toContain('fabricCapability: "not-provided"');
    expect(source.indexOf("const providerConfigBefore")).toBeLessThan(source.indexOf("await verifyProviderConformance"));
    expect(source).not.toContain("executableSha256");
  });
});
