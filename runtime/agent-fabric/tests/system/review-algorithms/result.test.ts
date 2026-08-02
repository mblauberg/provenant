import { describe, expect, it } from "vitest";

import { sha256Digest } from "../../../src/review/canonical/index.ts";
import {
  digestTerminalResult,
  parseReviewAnswer,
  reduceTerminalEvidenceEffect,
  type TerminalResultIdentity,
} from "../../../src/review/result/index.ts";

const digest = (value: string) => sha256Digest(value);
const actionRef = { adapterId: "codex-app-server", actionId: "review-1" };

function arms(): TerminalResultIdentity[] {
  const common = { schemaVersion: 1 as const, actionRef, terminalSequence: 7 };
  return [
    { ...common, terminalKind: "safe-answer", providerAnswerDigest: digest("answer"),
      reviewResultDigest: digest("result"), answerSafety: "safe", readCoverageDigest: digest("reads"),
      coverageSummaryDigest: digest("summary") },
    { ...common, terminalKind: "unusable-answer", providerAnswerDigest: digest("bad-answer"),
      reviewResultDigest: null, answerSafety: "unusable", readCoverageDigest: digest("bad-reads"),
      coverageSummaryDigest: digest("bad-summary") },
    { ...common, terminalKind: "provider-terminal-failure", providerFailureCode: "provider-rejected",
      providerFailureDigest: digest("failure") },
    { ...common, terminalKind: "terminal-no-effect", noEffectEvidenceDigest: digest("no-effect") },
    { ...common, terminalKind: "integrity-terminal", integrityEvidenceDigest: digest("integrity") },
    { ...common, terminalKind: "retired-unknown", retirementEvidenceDigest: digest("retired") },
  ];
}

describe("closed provider review terminal results", () => {
  it("digests exactly six disjoint arms and rejects crossed or extra fields", () => {
    const results = arms().map((arm) => digestTerminalResult(arm));
    expect(new Set(results.map((value) => value.terminalResultDigest)).size).toBe(6);
    expect(digestTerminalResult({ ...arms()[0] }).terminalResultDigest).toBe(results[0]!.terminalResultDigest);
    expect(() => digestTerminalResult({ ...arms()[2], providerAnswerDigest: digest("crossed") })).toThrow(/field/u);
    expect(() => digestTerminalResult({ ...arms()[2], extra: true })).toThrow(/field/u);
    expect(() => digestTerminalResult({ ...arms()[2], providerFailureCode: "future-code" })).toThrow(/failure code/u);
  });

  it("parses safe CLEAN/FINDINGS and converts malformed, unsafe or resolution-only new findings to UNUSABLE", () => {
    const prior = [digest("prior")];
    const clean = new TextEncoder().encode(JSON.stringify({
      schemaVersion: 1,
      targetGeneration: 4,
      coverageDigest: digest("coverage"),
      findingWindowMode: "normal",
      verdict: "CLEAN",
      resolvedFindingDigests: prior,
      findings: [],
    }));
    expect(parseReviewAnswer(clean, {
      targetGeneration: 4,
      coverageDigest: digest("coverage"),
      findingWindowMode: "normal",
      priorOpenFindingDigests: prior,
      allowedEvidenceRefs: ["test"],
    })).toMatchObject({ kind: "safe-answer", reviewResult: { verdict: "CLEAN" } });

    const findings = new TextEncoder().encode(JSON.stringify({
      schemaVersion: 1,
      targetGeneration: 4,
      coverageDigest: digest("coverage"),
      findingWindowMode: "normal",
      verdict: "FINDINGS",
      resolvedFindingDigests: [],
      findings: [{ findingId: "F-1", severity: "P1", summary: "bad boundary", evidence: "file.ts:1",
        repairKind: "registered-evidence", evidenceRefs: ["test"] }],
    }));
    expect(parseReviewAnswer(findings, {
      targetGeneration: 4,
      coverageDigest: digest("coverage"),
      findingWindowMode: "normal",
      priorOpenFindingDigests: prior,
      allowedEvidenceRefs: ["test"],
    })).toMatchObject({ kind: "safe-answer", reviewResult: { verdict: "FINDINGS" } });

    const duplicate = new TextEncoder().encode('{"schemaVersion":1,"schemaVersion":1}');
    expect(parseReviewAnswer(duplicate, {
      targetGeneration: 4, coverageDigest: digest("coverage"), findingWindowMode: "normal",
      priorOpenFindingDigests: prior, allowedEvidenceRefs: [],
    }).kind).toBe("unusable-answer");
    expect(parseReviewAnswer(findings, {
      targetGeneration: 4,
      coverageDigest: digest("coverage"),
      findingWindowMode: "resolution-only",
      priorOpenFindingDigests: prior,
      allowedEvidenceRefs: ["test"],
    }).kind).toBe("unusable-answer");
  });

  it("closes provider failure without creating evidence or advancing the head", () => {
    const failure = arms()[2]!;
    const failureDigest = digestTerminalResult(failure).terminalResultDigest;
    expect(reduceTerminalEvidenceEffect({
      terminal: failure,
      priorHeadGeneration: 8,
      priorEvidenceId: "evidence-8",
      priorOpenFindingSetDigest: digest("open"),
      priorRepairRequiredSetDigest: digest("repair"),
      reportedResolvedFindingDigests: [digest("prior")],
      certifyingInputsCurrent: true,
      mandatoryReadsSatisfied: true,
      actualRouteProvedEqual: true,
      providerAssurance: "full-vendor-identity",
      findingWindowMode: "normal",
      reviewVerdict: null,
      parsedFindingDigests: [],
    })).toStrictEqual({
      closesAttempt: true,
      createEvidence: false,
      advanceHead: false,
      priorHeadGeneration: 8,
      newHeadGeneration: 8,
      priorEvidenceId: "evidence-8",
      acceptedResolvedFindingDigests: [],
      unchangedOpenFindingSetDigest: digest("open"),
      unchangedRepairRequiredSetDigest: digest("repair"),
      publicTerminalKind: "provider-terminal-failure",
      certifying: false,
      retainedAdverseFindingDigests: [],
      effectiveTerminal: failure,
      terminalResultDigest: failureDigest,
    });
  });

  it("classifies coverage and actual-route blockers before evidence mutation", () => {
    const base = {
      terminal: arms()[0]!, priorHeadGeneration: 1, priorEvidenceId: "evidence-1",
      priorOpenFindingSetDigest: digest("open"), priorRepairRequiredSetDigest: digest("repair"),
      reportedResolvedFindingDigests: [digest("prior")], certifyingInputsCurrent: true,
      mandatoryReadsSatisfied: true, actualRouteProvedEqual: true,
      providerAssurance: "partial-signed-helpers" as const, findingWindowMode: "normal" as const,
      reviewVerdict: "CLEAN" as const, parsedFindingDigests: [] as const,
    };
    const insufficientClean = reduceTerminalEvidenceEffect({ ...base, mandatoryReadsSatisfied: false });
    expect(insufficientClean).toMatchObject({
      publicTerminalKind: "unusable-answer", certifying: false, acceptedResolvedFindingDigests: [],
      retainedAdverseFindingDigests: [], effectiveTerminal: { terminalKind: "unusable-answer", reviewResultDigest: null, answerSafety: "unusable" },
    });
    expect(digestTerminalResult(insufficientClean.effectiveTerminal).terminalResultDigest).toBe(insufficientClean.terminalResultDigest);
    const adverse = [digest("new-finding")];
    expect(reduceTerminalEvidenceEffect({ ...base, mandatoryReadsSatisfied: false,
      reviewVerdict: "FINDINGS", parsedFindingDigests: adverse })).toMatchObject({
      publicTerminalKind: "safe-answer", certifying: false, acceptedResolvedFindingDigests: [],
      retainedAdverseFindingDigests: adverse,
    });
    expect(reduceTerminalEvidenceEffect({ ...base, reviewVerdict: "CLEAN", parsedFindingDigests: [] }))
      .toMatchObject({ certifying: false });
    expect(reduceTerminalEvidenceEffect({ ...base, providerAssurance: "full-vendor-identity",
      reviewVerdict: "CLEAN", parsedFindingDigests: [] })).toMatchObject({ certifying: true });
    expect(reduceTerminalEvidenceEffect({ ...base, providerAssurance: "owner-controlled-install-root",
      reviewVerdict: "CLEAN", parsedFindingDigests: [] })).toMatchObject({ certifying: false });
    expect(reduceTerminalEvidenceEffect({ ...base, actualRouteProvedEqual: false,
      reviewVerdict: "FINDINGS", parsedFindingDigests: adverse })).toMatchObject({
      publicTerminalKind: "safe-answer", certifying: false, acceptedResolvedFindingDigests: [],
      retainedAdverseFindingDigests: adverse,
    });
  });
});
