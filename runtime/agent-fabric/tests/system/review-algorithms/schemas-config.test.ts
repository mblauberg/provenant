import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";

import { Ajv2020 } from "ajv/dist/2020.js";
import { describe, expect, it } from "vitest";
import { REVIEW_RISK_RULES_DIGEST } from "../../../src/review/bundle/index.ts";
import { sha256Digest } from "../../../src/review/canonical/index.ts";
import { buildReviewDiffSet } from "../../../src/review/diff/index.ts";
import { createSafeFinding } from "../../../src/review/findings/index.ts";
import { digestTerminalResult, reduceTerminalEvidenceEffect } from "../../../src/review/result/index.ts";

const root = resolve(import.meta.dirname, "../../../../..");
const read = (relative: string): any => JSON.parse(readFileSync(resolve(root, relative), "utf8"));

describe("Agent Fabric review schemas and checked-in catalogues", () => {
  it("validates the exact four-slot catalogue and rejects crossed effort", () => {
    const ajv = new Ajv2020({ allErrors: true, strict: true });
    const schema = read("runtime/agent-fabric/schemas/review-profile.v1.schema.json");
    const validate = ajv.compile(schema);
    const profile = read("config/review-profiles/certifying-review-four-slot-v1.json");
    expect(REVIEW_RISK_RULES_DIGEST).toBe("sha256:c8a4e424157f52c9e09165017ae717fa02798e4edab80e6388fa101816ef004d");
    expect(schema.$defs.slot.properties.riskReadMapDigest).toEqual({ const: REVIEW_RISK_RULES_DIGEST });
    expect(profile.chairProfiles.flatMap((chair: any) => chair.slots)
      .every((slot: any) => slot.riskReadMapDigest === REVIEW_RISK_RULES_DIGEST)).toBe(true);
    expect(validate(profile), JSON.stringify(validate.errors)).toBe(true);
    const crossed = structuredClone(profile);
    crossed.chairProfiles[0].slots[2].requestedEffort = "high";
    expect(validate(crossed)).toBe(false);
    const crossedBudget = structuredClone(profile);
    crossedBudget.chairProfiles[0].slots[0].providerTurnCeiling = 16;
    expect(validate(crossedBudget)).toBe(false);
    const crossedRisk = structuredClone(profile);
    crossedRisk.chairProfiles[0].slots[0].riskReadMapDigest = sha256Digest("stale-risk-rules");
    expect(validate(crossedRisk)).toBe(false);
  });

  it("dates every pinned identity against the observer its family actually has", () => {
    const ajv = new Ajv2020({ allErrors: true, strict: true });
    const validate = ajv.compile(read("runtime/agent-fabric/schemas/review-profile.v1.schema.json"));
    const profile = read("config/review-profiles/certifying-review-four-slot-v1.json");
    const slots = profile.chairProfiles.flatMap((chair: any) => chair.slots);
    const pinned = new Set(slots.map((slot: any) => `${slot.providerFamily} ${slot.model}`));
    const observed = profile.pinObservations.map((entry: any) => `${entry.providerFamily} ${entry.model}`);
    // Exactly one dated record per distinct pin: no pin is silent, none is orphaned.
    expect(new Set(observed)).toStrictEqual(pinned);
    expect(observed).toHaveLength(pinned.size);
    for (const entry of profile.pinObservations) {
      const declaring = slots.filter((slot: any) =>
        slot.providerFamily === entry.providerFamily && slot.model === entry.model);
      expect(declaring.every((slot: any) => slot.routeAliases.includes(entry.routeAlias)), entry.model).toBe(true);
    }
    expect(profile.pinObservations.map((entry: any) => [entry.providerFamily, entry.observedVia])).toStrictEqual([
      ["openai", "codex-model-catalogue"],
      ["anthropic", "claude-subscription-canary"],
      ["xai", "manual-attestation"],
      ["google", "manual-attestation"],
    ]);
    expect(validate(profile), JSON.stringify(validate.errors)).toBe(true);

    // A family with an observer cannot be downgraded to a manual attestation to
    // silence its own drift comparison, and no pin may go undated at all.
    const downgraded = structuredClone(profile);
    downgraded.pinObservations[1].observedVia = "manual-attestation";
    expect(validate(downgraded)).toBe(false);
    const undated = structuredClone(profile);
    delete undated.pinObservations;
    expect(validate(undated)).toBe(false);
    const malformedDate = structuredClone(profile);
    malformedDate.pinObservations[0].observedOn = "26 July 2026";
    expect(validate(malformedDate)).toBe(false);
  });

  it("catalogues every Agent Fabric v1.13 acceptance ID under the current semantic review gates", () => {
    const ajv = new Ajv2020({ allErrors: true, strict: true });
    const validate = ajv.compile(read("runtime/agent-fabric/schemas/console-acceptance-delivery-requirements.v1.schema.json"));
    const catalogue = read("config/console-acceptance-delivery-requirements.v1.json");
    const selectorRegistry = read("config/console-acceptance-evidence-selector-registry.v1.json");
    const validateSelectorRegistry = ajv.compile(read("runtime/agent-fabric/schemas/console-acceptance-evidence-selector-registry.v1.schema.json"));
    expect(validateSelectorRegistry(selectorRegistry), JSON.stringify(validateSelectorRegistry.errors)).toBe(true);
    const registeredSelectors = new Map(selectorRegistry.selectors.map((selector: any) => [selector.selectorId, selector]));
    const registeredSources = new Map(selectorRegistry.sources.map((source: any) => [source.sourceRef, source]));
    expect(registeredSelectors.size).toBe(selectorRegistry.selectors.length);
    expect(selectorRegistry.sources).toEqual([{
      sourceRef: "console-acceptance-v1.13",
      sourceRole: "spec",
      artifactRef: "docs/specs/console/acceptance.md",
      version: "1.13",
    }]);
    expect(validate(catalogue), JSON.stringify(validate.errors)).toBe(true);
    expect(catalogue.entries.map((entry: any) => entry.requirementId)).toEqual(
      Array.from({ length: 45 }, (_, index) => `CONSOLE-AC-${String(index + 1).padStart(3, "0")}`),
    );
    const allRequirementIds = catalogue.entries.map((entry: any) => entry.requirementId);
    expect(catalogue.phases).toEqual({
      preReviewRequirementIds: allRequirementIds.filter((requirementId: string) => requirementId !== "CONSOLE-AC-033"),
      postReviewAcceptanceRequirementIds: ["CONSOLE-AC-033"],
    });
    const finalReview = catalogue.entries.find((entry: any) => entry.requirementId === "CONSOLE-AC-033");
    expect(finalReview.evidenceSelectors).toEqual([{
      role: "post-review-acceptance",
      registryKind: "post-review-acceptance-decision.v1",
      selectorId: "console-acceptance-post-review-council-four-family-adjudication-v1",
      cardinality: "complete-nonempty-current-set",
      requiredStatus: "approved",
    }]);
    const kindForRole: Record<string, readonly string[]> = { test: ["test-result.v1"], evaluation: ["evaluation-result.v1"],
      load: ["load-result.v1"], migration: ["migration-result.v1"], "generated-contract": ["generated-contract.v1"],
      "gate-decision": ["human-gate-decision.v1", "review-gate-decision.v1"],
      "post-review-acceptance": ["post-review-acceptance-decision.v1"] };
    for (const entry of catalogue.entries) {
      const source = registeredSources.get(entry.sourceRef) as any;
      expect(source, entry.sourceRef).toBeDefined();
      expect(source.sourceRole).toBe(entry.sourceRole);
      expect(existsSync(resolve(root, source.artifactRef)), source.artifactRef).toBe(true);
      for (const selector of entry.evidenceSelectors) {
        const registration = registeredSelectors.get(selector.selectorId) as any;
        expect(registration, selector.selectorId).toBeDefined();
        expect(kindForRole[selector.role]).toContain(selector.registryKind);
        expect(registration.requirementId).toBe(entry.requirementId);
        if (registration.artifactRef !== null) expect(existsSync(resolve(root, registration.artifactRef))).toBe(true);
      }
    }
    const expectedProofs = [
      ["CONSOLE-AC-001", "console-protocol-inside-outside-herdr", "agent-fabric-console"],
      ["CONSOLE-AC-002", "console-restart-work-noninterference", "agent-fabric-console"],
      ["CONSOLE-AC-003", "timed-console-orientation", "llm-council"],
      ["CONSOLE-AC-004", "consequential-gate-projection-latency", "agent-fabric-runtime-console"],
      ["CONSOLE-AC-005", "authenticated-gate-input-and-confirmation", "agent-fabric-runtime-console"],
      ["CONSOLE-AC-006", "routine-vs-substantial-session-escalation", "agent-fabric-runtime"],
      ["CONSOLE-AC-007", "dynamic-topology-authority", "agent-fabric-runtime"],
      ["CONSOLE-AC-008", "persisted-gate-enforcement-and-dependencies", "agent-fabric-runtime"],
      ["CONSOLE-AC-009", "command-idempotency-and-stale-diff", "agent-fabric-runtime"],
      ["CONSOLE-AC-010", "writer-scope-and-worktree-exclusion", "agent-fabric-runtime"],
      ["CONSOLE-AC-011", "git-authority-and-consequential-gates", "agent-fabric-runtime"],
      ["CONSOLE-AC-012", "github-absence-outage-freshness", "agent-fabric-runtime-console"],
      ["CONSOLE-AC-013", "provider-pane-fabric-identity-reconciliation", "agent-fabric-runtime"],
      ["CONSOLE-AC-014", "operator-action-audit-journal", "agent-fabric-runtime"],
      ["CONSOLE-AC-015", "safe-full-message-rendering", "agent-fabric-console"],
      ["CONSOLE-AC-016", "console-mutation-capability-matrix", "agent-fabric-runtime-console"],
      ["CONSOLE-AC-017", "conversational-approval-provenance", "agent-fabric-runtime-console"],
      ["CONSOLE-AC-018", "daemon-start-attach-project-concurrency", "agent-fabric-runtime"],
      ["CONSOLE-AC-019", "control-lifecycle-restart-reconciliation", "agent-fabric-runtime"],
      ["CONSOLE-AC-020", "notification-dedup-delivery-and-focus", "agent-fabric-runtime-console"],
      ["CONSOLE-AC-021", "timed-keyboard-mouse-resize-usability", "llm-council"],
      ["CONSOLE-AC-022", "project-session-lifecycle-and-closure", "agent-fabric-runtime"],
      ["CONSOLE-AC-023", "promotion-authority-and-artifact-binding", "agent-fabric-runtime"],
      ["CONSOLE-AC-024", "lifecycle-skill-trigger-boundary-portability", "skill-orchestration-evaluation"],
      ["CONSOLE-AC-025", "paired-result-transactional-outbox", "agent-fabric-runtime"],
      ["CONSOLE-AC-026", "safe-result-delivery-and-restart", "agent-fabric-runtime"],
      ["CONSOLE-AC-027", "result-deadline-retry-and-reassignment", "agent-fabric-runtime"],
      ["CONSOLE-AC-028", "herdr-fire-and-forget-and-degraded-path", "agent-fabric-runtime"],
      ["CONSOLE-AC-029", "chair-loss-authorized-takeover", "agent-fabric-runtime"],
      ["CONSOLE-AC-030", "scoping-intake-restart-and-compaction", "agent-fabric-runtime-console"],
      ["CONSOLE-AC-031", "harness-pairing-single-chair-stage-owner", "harness-governance"],
      ["CONSOLE-AC-032", "project-budget-atomicity-and-reconciliation", "agent-fabric-runtime-console"],
      ["CONSOLE-AC-033", "current-four-family-clean-review", "llm-council"],
      ["CONSOLE-AC-034", "evidence-content-pagination-and-acceptance-safety", "agent-fabric-runtime-console"],
      ["CONSOLE-AC-035", "project-multi-session-selection-and-projection", "agent-fabric-runtime-console"],
      ["CONSOLE-AC-036", "review-routing-replay-singleflight-admission", "agent-fabric-runtime"],
      ["CONSOLE-AC-037", "terminal-evidence-linear-head-findings", "agent-fabric-runtime"],
      ["CONSOLE-AC-038", "terminal-classification-sequence-certification-cut", "agent-fabric-runtime"],
      ["CONSOLE-AC-039", "lifecycle-continuity-routing-and-recovery", "agent-fabric-runtime"],
      ["CONSOLE-AC-040", "review-target-preparation-crash-cas", "agent-fabric-runtime"],
      ["CONSOLE-AC-041", "chair-adoption-review-certification-cut", "agent-fabric-runtime"],
      ["CONSOLE-AC-042", "four-family-portal-confinement-helper-custody", "agent-fabric-runtime"],
      ["CONSOLE-AC-043", "route-discovery-capability-effective-configuration-lineage", "agent-fabric-runtime"],
      ["CONSOLE-AC-044", "route-projection-display-actual-identity", "agent-fabric-console"],
      ["CONSOLE-AC-045", "topology-wave-context-pressure-projection", "agent-fabric-runtime-console"],
    ] as const;
    expect(expectedProofs).toHaveLength(45);
    for (const [requirementId, proofDomain, owner] of expectedProofs) {
      const registrations = selectorRegistry.selectors.filter((selector: any) => selector.requirementId === requirementId);
      expect(registrations.map((selector: any) => [selector.proofDomain, selector.owner]), requirementId)
        .toEqual([[proofDomain, owner]]);
    }
    expect((registeredSelectors.get("console-acceptance-review-gate-console-10s-orientation-v1") as any).state).toBe("gate-pending");
    expect((registeredSelectors.get("console-acceptance-review-gate-console-usability-resize-v1") as any).state).toBe("gate-pending");
    expect(selectorRegistry.selectors.some((selector: any) =>
      selector.owner === "human-acceptance" || selector.state === "human-gate" || selector.selectorId.includes("human-gate"),
    )).toBe(false);
  });

  it("compiles the closed bundle, diff, finding and terminal-result schemas", () => {
    const ajv = new Ajv2020({ allErrors: true, strict: true });
    for (const name of ["review-bundle.v1", "review-diff.v1", "review-finding.v1", "review-terminal-result.v1"] as const) {
      expect(() => ajv.compile(read(`runtime/agent-fabric/schemas/${name}.schema.json`))).not.toThrow();
    }
  });

  it("rejects extra and cross-arm fields while accepting exact runtime outputs", () => {
    const ajv = new Ajv2020({ allErrors: true, strict: true });
    const digest = (value: string) => sha256Digest(value);
    const diff = buildReviewDiffSet({ objectFormat: "sha1", baseObjectId: "1".repeat(40), headObjectId: "2".repeat(40),
      codecDigest: digest("codec"), rulesDigest: digest("rules"), before: [],
      after: [{ path: "a.ts", mode: "100644", bytes: new TextEncoder().encode("a\n") }] });
    const validateDiff = ajv.compile(read("runtime/agent-fabric/schemas/review-diff.v1.schema.json"));
    expect(validateDiff(diff), JSON.stringify(validateDiff.errors)).toBe(true);
    expect(validateDiff({ ...diff, entries: [{ ...diff.entries[0], extra: true }] })).toBe(false);

    const finding = createSafeFinding({ findingId: "F-1", severity: "P1", summary: "summary", evidence: "evidence",
      originTargetGeneration: 1, originActionRef: { adapterId: "adapter", actionId: "action" }, originResultDigest: digest("result"),
      originDeliveryManifest: { artifactRef: "manifest", artifactRevision: 1 }, originDeliveryReviewBasisDigest: digest("basis"),
      originBundleDigest: digest("bundle"), repairCurrency: { kind: "repository-source", originRepositorySourceStateDigest: digest("source"), evidenceRefs: [] } });
    const validateFinding = ajv.compile(read("runtime/agent-fabric/schemas/review-finding.v1.schema.json"));
    expect(validateFinding(finding), JSON.stringify(validateFinding.errors)).toBe(true);
    expect(validateFinding({ ...finding, repairKind: "repository-source" })).toBe(false);

    const terminal = digestTerminalResult({ schemaVersion: 1, actionRef: { adapterId: "adapter", actionId: "action" },
      terminalSequence: 1, terminalKind: "provider-terminal-failure", providerFailureCode: "provider-rejected", providerFailureDigest: digest("failure") }).identity;
    const validateTerminal = ajv.compile(read("runtime/agent-fabric/schemas/review-terminal-result.v1.schema.json"));
    expect(validateTerminal(terminal), JSON.stringify(validateTerminal.errors)).toBe(true);
    expect(validateTerminal({ ...terminal, providerAnswerDigest: digest("answer") })).toBe(false);
    const safe = digestTerminalResult({ schemaVersion: 1, actionRef: { adapterId: "adapter", actionId: "action" },
      terminalSequence: 2, terminalKind: "safe-answer", providerAnswerDigest: digest("answer"), reviewResultDigest: digest("review"),
      answerSafety: "safe", readCoverageDigest: digest("reads"), coverageSummaryDigest: digest("coverage-summary") }).identity;
    const reclassified = reduceTerminalEvidenceEffect({ terminal: safe, priorHeadGeneration: 0, priorEvidenceId: null,
      priorOpenFindingSetDigest: digest("open"), priorRepairRequiredSetDigest: digest("repair"), reportedResolvedFindingDigests: [],
      certifyingInputsCurrent: true, mandatoryReadsSatisfied: false, actualRouteProvedEqual: true,
      providerAssurance: "full-vendor-identity",
      findingWindowMode: "normal", reviewVerdict: "CLEAN", parsedFindingDigests: [] });
    expect(validateTerminal(reclassified.effectiveTerminal), JSON.stringify(validateTerminal.errors)).toBe(true);
    expect(digestTerminalResult(reclassified.effectiveTerminal).terminalResultDigest).toBe(reclassified.terminalResultDigest);
    for (const [terminalKind, field] of [["terminal-no-effect", "noEffectEvidenceDigest"], ["integrity-terminal", "integrityEvidenceDigest"], ["retired-unknown", "retirementEvidenceDigest"]] as const) {
      const arm = { schemaVersion: 1, actionRef: { adapterId: "adapter", actionId: "action" }, terminalSequence: 1,
        terminalKind, [field]: digest(field) };
      expect(validateTerminal(arm), `${terminalKind} exact`).toBe(true);
      expect(validateTerminal({ ...arm, providerFailureDigest: digest("cross") }), `${terminalKind} crossed`).toBe(false);
    }

    const bundleSchema = read("runtime/agent-fabric/schemas/review-bundle.v1.schema.json");
    expect(bundleSchema.$defs.body.additionalProperties).toBe(false);
    expect(bundleSchema.$defs.delivery.additionalProperties).toBe(false);
    for (const name of ["changedFile", "evidence", "findingPageRef", "objectRecord"] as const) {
      expect(bundleSchema.$defs[name].additionalProperties, name).toBe(false);
    }
  });
});
