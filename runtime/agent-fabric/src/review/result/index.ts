import {
  digestCanonical,
  sha256Digest,
  type Sha256Digest,
} from "../canonical/index.js";
import {
  supportsCertifyingAnswerBearingLeg,
  type ProviderIdentityAssurance,
} from "../../adapters/provider-identity.js";

export interface ProviderActionRef {
  adapterId: string;
  actionId: string;
}

export type ProviderFailureCode =
  | "max-turns-exhausted"
  | "provider-rejected"
  | "terminal-no-answer"
  | "adapter-terminal-failure";

export type TerminalResultIdentity =
  | { schemaVersion: 1; actionRef: ProviderActionRef; terminalSequence: number; terminalKind: "safe-answer";
      providerAnswerDigest: Sha256Digest; reviewResultDigest: Sha256Digest; answerSafety: "safe";
      readCoverageDigest: Sha256Digest; coverageSummaryDigest: Sha256Digest }
  | { schemaVersion: 1; actionRef: ProviderActionRef; terminalSequence: number; terminalKind: "unusable-answer";
      providerAnswerDigest: Sha256Digest; reviewResultDigest: null; answerSafety: "unusable";
      readCoverageDigest: Sha256Digest; coverageSummaryDigest: Sha256Digest }
  | { schemaVersion: 1; actionRef: ProviderActionRef; terminalSequence: number;
      terminalKind: "provider-terminal-failure"; providerFailureCode: ProviderFailureCode;
      providerFailureDigest: Sha256Digest }
  | { schemaVersion: 1; actionRef: ProviderActionRef; terminalSequence: number;
      terminalKind: "terminal-no-effect"; noEffectEvidenceDigest: Sha256Digest }
  | { schemaVersion: 1; actionRef: ProviderActionRef; terminalSequence: number;
      terminalKind: "integrity-terminal"; integrityEvidenceDigest: Sha256Digest }
  | { schemaVersion: 1; actionRef: ProviderActionRef; terminalSequence: number;
      terminalKind: "retired-unknown"; retirementEvidenceDigest: Sha256Digest };

export interface ParsedReviewResult {
  schemaVersion: 1;
  targetGeneration: number;
  coverageDigest: Sha256Digest;
  findingWindowMode: "normal" | "resolution-only";
  verdict: "CLEAN" | "FINDINGS";
  resolvedFindingDigests: readonly Sha256Digest[];
  findings: readonly {
    findingId: string;
    severity: "P0" | "P1" | "P2";
    summary: string;
    evidence: string;
    repairKind: "repository-source" | "registered-evidence" | "mixed";
    evidenceRefs: readonly string[];
  }[];
}

const DIGEST_PATTERN = /^sha256:[0-9a-f]{64}$/u;
const FAILURE_CODES = new Set<ProviderFailureCode>([
  "max-turns-exhausted",
  "provider-rejected",
  "terminal-no-answer",
  "adapter-terminal-failure",
]);

function record(value: unknown, label: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError(`${label} must be an object`);
  }
  return value as Record<string, unknown>;
}

function exactKeys(value: Record<string, unknown>, expected: readonly string[], label: string): void {
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (actual.length !== wanted.length || actual.some((key, index) => key !== wanted[index])) {
    throw new TypeError(`${label} contains an invalid field set`);
  }
}

function assertDigest(value: unknown, label: string): asserts value is Sha256Digest {
  if (typeof value !== "string" || !DIGEST_PATTERN.test(value)) throw new TypeError(`${label} is invalid`);
}

function assertActionRef(value: unknown): asserts value is ProviderActionRef {
  const candidate = record(value, "actionRef");
  exactKeys(candidate, ["adapterId", "actionId"], "actionRef");
  if (typeof candidate.adapterId !== "string" || candidate.adapterId.length === 0
    || typeof candidate.actionId !== "string" || candidate.actionId.length === 0) {
    throw new TypeError("actionRef IDs are invalid");
  }
}

export function digestTerminalResult(value: unknown): Readonly<{
  identity: TerminalResultIdentity;
  terminalResultDigest: Sha256Digest;
}> {
  const candidate = record(value, "terminal result identity");
  if (candidate.schemaVersion !== 1 || !Number.isSafeInteger(candidate.terminalSequence)
    || (candidate.terminalSequence as number) < 1 || typeof candidate.terminalKind !== "string") {
    throw new TypeError("terminal result common fields are invalid");
  }
  assertActionRef(candidate.actionRef);
  const common = ["schemaVersion", "actionRef", "terminalSequence", "terminalKind"];
  switch (candidate.terminalKind) {
    case "safe-answer":
      exactKeys(candidate, [...common, "providerAnswerDigest", "reviewResultDigest", "answerSafety",
        "readCoverageDigest", "coverageSummaryDigest"], "safe-answer terminal");
      if (candidate.answerSafety !== "safe") throw new TypeError("safe-answer safety is invalid");
      assertDigest(candidate.providerAnswerDigest, "provider answer digest");
      assertDigest(candidate.reviewResultDigest, "review result digest");
      assertDigest(candidate.readCoverageDigest, "read coverage digest");
      assertDigest(candidate.coverageSummaryDigest, "coverage summary digest");
      break;
    case "unusable-answer":
      exactKeys(candidate, [...common, "providerAnswerDigest", "reviewResultDigest", "answerSafety",
        "readCoverageDigest", "coverageSummaryDigest"], "unusable-answer terminal");
      if (candidate.answerSafety !== "unusable" || candidate.reviewResultDigest !== null) {
        throw new TypeError("unusable-answer safety/result is invalid");
      }
      assertDigest(candidate.providerAnswerDigest, "provider answer digest");
      assertDigest(candidate.readCoverageDigest, "read coverage digest");
      assertDigest(candidate.coverageSummaryDigest, "coverage summary digest");
      break;
    case "provider-terminal-failure":
      exactKeys(candidate, [...common, "providerFailureCode", "providerFailureDigest"], "provider failure terminal");
      if (!FAILURE_CODES.has(candidate.providerFailureCode as ProviderFailureCode)) {
        throw new TypeError("provider failure code is invalid");
      }
      assertDigest(candidate.providerFailureDigest, "provider failure digest");
      break;
    case "terminal-no-effect":
      exactKeys(candidate, [...common, "noEffectEvidenceDigest"], "no-effect terminal");
      assertDigest(candidate.noEffectEvidenceDigest, "no-effect evidence digest");
      break;
    case "integrity-terminal":
      exactKeys(candidate, [...common, "integrityEvidenceDigest"], "integrity terminal");
      assertDigest(candidate.integrityEvidenceDigest, "integrity evidence digest");
      break;
    case "retired-unknown":
      exactKeys(candidate, [...common, "retirementEvidenceDigest"], "retired terminal");
      assertDigest(candidate.retirementEvidenceDigest, "retirement evidence digest");
      break;
    default:
      throw new TypeError("terminal result kind is invalid");
  }
  const identity = candidate as unknown as TerminalResultIdentity;
  return { identity, terminalResultDigest: digestCanonical(identity) };
}

class DuplicateAwareJsonParser {
  #index = 0;
  constructor(private readonly text: string) {}

  parse(): unknown {
    const result = this.#value();
    this.#space();
    if (this.#index !== this.text.length) throw new SyntaxError("JSON has trailing bytes");
    return result;
  }

  #space(): void {
    while (/[\t\n\r ]/u.test(this.text[this.#index] ?? "")) this.#index += 1;
  }

  #value(): unknown {
    this.#space();
    const character = this.text[this.#index];
    if (character === "{") return this.#object();
    if (character === "[") return this.#array();
    if (character === '"') return this.#string();
    for (const [token, value] of [["true", true], ["false", false], ["null", null]] as const) {
      if (this.text.startsWith(token, this.#index)) {
        this.#index += token.length;
        return value;
      }
    }
    const match = /^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?/u.exec(this.text.slice(this.#index));
    if (match === null) throw new SyntaxError("invalid JSON value");
    this.#index += match[0].length;
    const value = Number(match[0]);
    if (!Number.isFinite(value)) throw new SyntaxError("JSON number is not finite");
    return value;
  }

  #string(): string {
    const start = this.#index;
    this.#index += 1;
    let escaped = false;
    while (this.#index < this.text.length) {
      const character = this.text[this.#index]!;
      this.#index += 1;
      if (!escaped && character === '"') return JSON.parse(this.text.slice(start, this.#index)) as string;
      if (!escaped && character.charCodeAt(0) < 0x20) throw new SyntaxError("JSON string contains control text");
      if (!escaped && character === "\\") escaped = true;
      else escaped = false;
    }
    throw new SyntaxError("unterminated JSON string");
  }

  #object(): Record<string, unknown> {
    this.#index += 1;
    const result: Record<string, unknown> = {};
    const keys = new Set<string>();
    this.#space();
    if (this.text[this.#index] === "}") {
      this.#index += 1;
      return result;
    }
    while (true) {
      this.#space();
      if (this.text[this.#index] !== '"') throw new SyntaxError("JSON object key is invalid");
      const key = this.#string();
      if (keys.has(key)) throw new SyntaxError(`duplicate JSON key: ${key}`);
      keys.add(key);
      this.#space();
      if (this.text[this.#index] !== ":") throw new SyntaxError("JSON object colon is missing");
      this.#index += 1;
      result[key] = this.#value();
      this.#space();
      const separator = this.text[this.#index];
      this.#index += 1;
      if (separator === "}") return result;
      if (separator !== ",") throw new SyntaxError("JSON object separator is invalid");
    }
  }

  #array(): unknown[] {
    this.#index += 1;
    const result: unknown[] = [];
    this.#space();
    if (this.text[this.#index] === "]") {
      this.#index += 1;
      return result;
    }
    while (true) {
      result.push(this.#value());
      this.#space();
      const separator = this.text[this.#index];
      this.#index += 1;
      if (separator === "]") return result;
      if (separator !== ",") throw new SyntaxError("JSON array separator is invalid");
    }
  }
}

function isSortedUnique(values: readonly string[]): boolean {
  return values.every((value, index) => index === 0 || values[index - 1]! < value);
}

function safeText(value: unknown, maximumBytes: number): value is string {
  if (typeof value !== "string" || value.length === 0 || Buffer.byteLength(value, "utf8") > maximumBytes) return false;
  return [...value].every((character) => {
    const codePoint = character.codePointAt(0)!;
    return codePoint >= 0x20 && !(codePoint >= 0x7f && codePoint <= 0x9f)
      && !(codePoint >= 0x202a && codePoint <= 0x202e)
      && !(codePoint >= 0x2066 && codePoint <= 0x2069);
  });
}

export function parseReviewAnswer(
  answer: Uint8Array,
  context: Readonly<{
    targetGeneration: number;
    coverageDigest: Sha256Digest;
    findingWindowMode: "normal" | "resolution-only";
    priorOpenFindingDigests: readonly Sha256Digest[];
    allowedEvidenceRefs: readonly string[];
    secretValues?: readonly string[];
  }>,
): Readonly<
  | { kind: "safe-answer"; providerAnswerDigest: Sha256Digest; reviewResult: ParsedReviewResult; reviewResultDigest: Sha256Digest }
  | { kind: "unusable-answer"; providerAnswerDigest: Sha256Digest; reviewResult: null; reviewResultDigest: null; reasons: readonly string[] }
> {
  const providerAnswerDigest = sha256Digest(answer);
  const unusable = (...reasons: string[]) => ({
    kind: "unusable-answer" as const,
    providerAnswerDigest,
    reviewResult: null,
    reviewResultDigest: null,
    reasons,
  });
  if (answer.byteLength === 0 || answer.byteLength > 65_536) return unusable("answer-byte-bound");
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true }).decode(answer);
  } catch {
    return unusable("invalid-utf8");
  }
  if ((context.secretValues ?? []).some((secret) => secret.length > 0 && text.includes(secret))) {
    return unusable("secret-value");
  }
  let parsed: unknown;
  try {
    parsed = new DuplicateAwareJsonParser(text).parse();
  } catch {
    return unusable("invalid-json");
  }
  try {
    const candidate = record(parsed, "review result");
    exactKeys(candidate, ["schemaVersion", "targetGeneration", "coverageDigest", "findingWindowMode",
      "verdict", "resolvedFindingDigests", "findings"], "review result");
    if (candidate.schemaVersion !== 1 || candidate.targetGeneration !== context.targetGeneration
      || candidate.coverageDigest !== context.coverageDigest
      || candidate.findingWindowMode !== context.findingWindowMode
      || (candidate.verdict !== "CLEAN" && candidate.verdict !== "FINDINGS")
      || !Array.isArray(candidate.resolvedFindingDigests) || !Array.isArray(candidate.findings)) {
      return unusable("review-binding");
    }
    const resolved = candidate.resolvedFindingDigests;
    if (!resolved.every((value): value is Sha256Digest => typeof value === "string" && DIGEST_PATTERN.test(value))
      || !isSortedUnique(resolved) || resolved.some((value) => !context.priorOpenFindingDigests.includes(value))) {
      return unusable("resolved-finding-set");
    }
    const findingIds = new Set<string>();
    const findings: ParsedReviewResult["findings"][number][] = [];
    for (const rawFinding of candidate.findings) {
      const item = record(rawFinding, "review finding");
      exactKeys(item, ["findingId", "severity", "summary", "evidence", "repairKind", "evidenceRefs"], "review finding");
      if (typeof item.findingId !== "string" || !/^[A-Za-z0-9._:-]{1,64}$/u.test(item.findingId)
        || findingIds.has(item.findingId) || !["P0", "P1", "P2"].includes(String(item.severity))
        || !safeText(item.summary, 256) || !safeText(item.evidence, 768)
        || !["repository-source", "registered-evidence", "mixed"].includes(String(item.repairKind))
        || !Array.isArray(item.evidenceRefs)
        || !item.evidenceRefs.every((value): value is string => typeof value === "string"
          && context.allowedEvidenceRefs.includes(value))
        || !isSortedUnique(item.evidenceRefs)) return unusable("review-finding");
      if ((item.repairKind === "repository-source" && item.evidenceRefs.length !== 0)
        || (item.repairKind !== "repository-source" && item.evidenceRefs.length === 0)) {
        return unusable("review-repair-kind");
      }
      findingIds.add(item.findingId);
      findings.push({
        findingId: item.findingId,
        severity: item.severity as "P0" | "P1" | "P2",
        summary: item.summary,
        evidence: item.evidence,
        repairKind: item.repairKind as "repository-source" | "registered-evidence" | "mixed",
        evidenceRefs: item.evidenceRefs,
      });
    }
    const prior = [...context.priorOpenFindingDigests].sort();
    if (candidate.verdict === "CLEAN" && (findings.length !== 0
      || resolved.length !== prior.length || resolved.some((value, index) => value !== prior[index]))) {
      return unusable("clean-contract");
    }
    if (candidate.verdict === "FINDINGS" && (findings.length < 1 || findings.length > 32)) {
      return unusable("findings-contract");
    }
    if (context.findingWindowMode === "resolution-only"
      && (candidate.verdict !== "CLEAN" || findings.length !== 0 || resolved.length > 32)) {
      return unusable("resolution-only-contract");
    }
    const reviewResult: ParsedReviewResult = {
      schemaVersion: 1,
      targetGeneration: context.targetGeneration,
      coverageDigest: context.coverageDigest,
      findingWindowMode: context.findingWindowMode,
      verdict: candidate.verdict,
      resolvedFindingDigests: resolved,
      findings,
    };
    return { kind: "safe-answer", providerAnswerDigest, reviewResult, reviewResultDigest: digestCanonical(reviewResult) };
  } catch {
    return unusable("invalid-review-shape");
  }
}

export function reduceTerminalEvidenceEffect(input: Readonly<{
  terminal: TerminalResultIdentity;
  priorHeadGeneration: number;
  priorEvidenceId: string | null;
  priorOpenFindingSetDigest: Sha256Digest;
  priorRepairRequiredSetDigest: Sha256Digest;
  reportedResolvedFindingDigests: readonly Sha256Digest[];
  certifyingInputsCurrent: boolean;
  mandatoryReadsSatisfied: boolean;
  actualRouteProvedEqual: boolean;
  providerAssurance: ProviderIdentityAssurance;
  findingWindowMode: "normal" | "resolution-only";
  reviewVerdict: "CLEAN" | "FINDINGS" | null;
  parsedFindingDigests: readonly Sha256Digest[];
}>): Readonly<{
  closesAttempt: true;
  createEvidence: boolean;
  advanceHead: boolean;
  priorHeadGeneration: number;
  newHeadGeneration: number;
  priorEvidenceId: string | null;
  acceptedResolvedFindingDigests: readonly Sha256Digest[];
  unchangedOpenFindingSetDigest: Sha256Digest | null;
  unchangedRepairRequiredSetDigest: Sha256Digest | null;
  publicTerminalKind: TerminalResultIdentity["terminalKind"];
  effectiveTerminal: TerminalResultIdentity;
  terminalResultDigest: Sha256Digest;
  certifying: boolean;
  retainedAdverseFindingDigests: readonly Sha256Digest[];
}> {
  if (!Number.isSafeInteger(input.priorHeadGeneration) || input.priorHeadGeneration < 0) {
    throw new TypeError("prior review head generation is invalid");
  }
  const createsEvidence = input.terminal.terminalKind === "safe-answer"
    || input.terminal.terminalKind === "unusable-answer";
  const safeAnswer = input.terminal.terminalKind === "safe-answer";
  if (safeAnswer !== (input.reviewVerdict !== null)) {
    throw new TypeError("safe terminal and parsed review verdict must be present together");
  }
  input.parsedFindingDigests.forEach((value) => assertDigest(value, "parsed finding digest"));
  const insufficientClean = safeAnswer && input.reviewVerdict === "CLEAN" && !input.mandatoryReadsSatisfied;
  const eligibleForResolution = safeAnswer && !insufficientClean
    && input.certifyingInputsCurrent && input.mandatoryReadsSatisfied && input.actualRouteProvedEqual
    && supportsCertifyingAnswerBearingLeg(input.providerAssurance);
  const accepted = eligibleForResolution
    ? [...input.reportedResolvedFindingDigests]
    : [];
  const retainedAdverseFindingDigests = safeAnswer && input.reviewVerdict === "FINDINGS"
    ? [...input.parsedFindingDigests]
    : [];
  const effectiveTerminal: TerminalResultIdentity = insufficientClean && input.terminal.terminalKind === "safe-answer"
    ? {
        schemaVersion: 1,
        actionRef: input.terminal.actionRef,
        terminalSequence: input.terminal.terminalSequence,
        terminalKind: "unusable-answer",
        providerAnswerDigest: input.terminal.providerAnswerDigest,
        reviewResultDigest: null,
        answerSafety: "unusable",
        readCoverageDigest: input.terminal.readCoverageDigest,
        coverageSummaryDigest: input.terminal.coverageSummaryDigest,
      }
    : input.terminal;
  const terminalResultDigest = digestTerminalResult(effectiveTerminal).terminalResultDigest;
  return {
    closesAttempt: true,
    createEvidence: createsEvidence,
    advanceHead: createsEvidence,
    priorHeadGeneration: input.priorHeadGeneration,
    newHeadGeneration: createsEvidence ? input.priorHeadGeneration + 1 : input.priorHeadGeneration,
    priorEvidenceId: input.priorEvidenceId,
    acceptedResolvedFindingDigests: accepted,
    unchangedOpenFindingSetDigest: createsEvidence ? null : input.priorOpenFindingSetDigest,
    unchangedRepairRequiredSetDigest: createsEvidence ? null : input.priorRepairRequiredSetDigest,
    publicTerminalKind: effectiveTerminal.terminalKind,
    effectiveTerminal,
    terminalResultDigest,
    certifying: eligibleForResolution && input.findingWindowMode === "normal",
    retainedAdverseFindingDigests,
  };
}
