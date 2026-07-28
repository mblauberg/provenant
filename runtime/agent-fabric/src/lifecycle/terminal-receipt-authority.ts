import { FabricError } from "../errors.js";
import { canonicalJson, integer, row, text } from "../persistence/row-codec.js";
import type {
  LifecycleAuthenticatedScopeCheckpoint,
  LifecycleDigest,
  LifecycleIntegrityReceiptAuthorityPort,
  LifecycleReceiptLookup,
  LifecycleReceiptRecord,
} from "./receipt-authority.js";
import { lifecycleDigest } from "./custody-codec.js";

const PAGE_LIMIT = 256;
const MAX_SCOPE_RECEIPTS = 65_536;

export type PreparedTerminalAuthorityCandidate = Readonly<{
  projectSessionId: string;
  runId: string;
  agentId: string;
  finalRevision: number;
  ownerRefDigest: string;
  subject: Readonly<Record<string, unknown>>;
  subjectJson: string;
  subjectDigest: string;
  intentDigest: string;
  review: Readonly<{
    subject: Readonly<Record<string, unknown>>;
    subjectJson: string;
    subjectDigest: string;
    intentDigest: string;
  }> | null;
}>;

function candidateKind(subject: Readonly<Record<string, unknown>>): LifecycleReceiptLookup["kind"] {
  const kind = text(subject, "kind");
  if (kind !== "custody-terminal" && kind !== "generation-loss-terminal" &&
      kind !== "review-adoption-decision") {
    throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "lifecycle terminal owner kind is invalid");
  }
  return kind;
}

async function recoverCandidate(
  authority: LifecycleIntegrityReceiptAuthorityPort,
  prepared: PreparedTerminalAuthorityCandidate,
  candidate: Readonly<{
    subject: Readonly<Record<string, unknown>>;
    subjectJson: string;
    subjectDigest: string;
    intentDigest: string;
  }>,
): Promise<LifecycleReceiptRecord> {
  const kind = candidateKind(candidate.subject);
  const lookup: LifecycleReceiptLookup = {
    kind,
    projectSessionId: prepared.projectSessionId,
    runId: prepared.runId,
    agentId: prepared.agentId,
    ownerRefDigest: prepared.ownerRefDigest as LifecycleDigest,
    ownerRevision: prepared.finalRevision,
  };
  let record: LifecycleReceiptRecord | null;
  try {
    record = await authority.readReceipt(lookup);
  } catch (error: unknown) {
    throw new FabricError("CAPABILITY_UNAVAILABLE", "lifecycle receipt authority read failed", { cause: error });
  }
  if (record === null) {
    try {
      await authority.appendReceipt(candidate.intentDigest as LifecycleDigest, candidate.subject);
    } catch {
      // The authoritative point read below reconciles a lost append response.
    }
    try {
      record = await authority.readReceipt(lookup);
    } catch (error: unknown) {
      throw new FabricError("CAPABILITY_UNAVAILABLE", "lifecycle receipt authority reconciliation failed", {
        cause: error,
      });
    }
  }
  if (record === null) {
    throw new FabricError("CAPABILITY_UNAVAILABLE", "lifecycle authority receipt remains pending");
  }
  if (
    canonicalJson(record.subject) !== candidate.subjectJson ||
    record.receipt.kind !== kind ||
    record.receipt.intentDigest !== candidate.intentDigest ||
    record.receipt.subjectDigest !== candidate.subjectDigest ||
    !await authority.verifyReceipt(candidate.subject, record.receipt)
  ) {
    throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "lifecycle receipt authority returned crossed evidence");
  }
  return record;
}

function receiptSetMember(record: LifecycleReceiptRecord): readonly [
  string, LifecycleDigest, LifecycleDigest, string, string, string, string, string,
] {
  const owner = row(record.subject.ownerRef, "lifecycle receipt owner");
  let ownerKind: string;
  let ownerId: string;
  let ownerRevision: number;
  if (owner.kind === "custody") {
    const custody = row(owner.custodyRef, "lifecycle receipt custody owner");
    ownerKind = "custody";
    ownerId = text(custody, "custodyId");
    ownerRevision = integer(custody, "custodyRevision");
  } else if (owner.kind === "generation-loss") {
    const loss = row(owner.generationLossRef, "lifecycle receipt generation-loss owner");
    ownerKind = "generation-loss";
    ownerId = text(loss, "generationLossId");
    ownerRevision = integer(loss, "generationLossRevision");
  } else if (owner.kind === "recovery-retirement") {
    const retirement = row(owner.retirementRef, "lifecycle receipt recovery-retirement owner");
    ownerKind = "recovery-retirement";
    ownerId = text(retirement, "retirementId");
    const revisionDec = text(retirement, "revisionDec");
    const parsedRevision = Number(revisionDec);
    if (!Number.isSafeInteger(parsedRevision) || parsedRevision < 1) {
      throw new FabricError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "lifecycle receipt recovery-retirement revision is invalid",
      );
    }
    ownerRevision = parsedRevision;
  } else {
    throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "lifecycle terminal owner reference is invalid");
  }
  return [
    String(record.receipt.authoritySequence),
    record.receipt.receiptDigest,
    record.receipt.intentDigest,
    text(record.subject, "kind"),
    text(record.subject, "agentId"),
    ownerKind,
    ownerId,
    String(ownerRevision),
  ];
}

export async function recoverTerminalAuthorityReceipt(
  authority: LifecycleIntegrityReceiptAuthorityPort,
  prepared: PreparedTerminalAuthorityCandidate,
): Promise<Readonly<{
  record: LifecycleReceiptRecord;
  reviewRecord: LifecycleReceiptRecord | null;
  checkpoint: LifecycleAuthenticatedScopeCheckpoint;
}>> {
  const record = await recoverCandidate(authority, prepared, prepared);
  const reviewRecord = prepared.review === null
    ? null
    : await recoverCandidate(authority, prepared, prepared.review);
  const checkpoint = await authority.readScopeCheckpoint(prepared.projectSessionId, prepared.runId);
  const pinnedCheckpoint = await authority.readScopeCheckpointAt(checkpoint.checkpointDigest);
  if (
    canonicalJson(checkpoint) !== canonicalJson(pinnedCheckpoint) ||
    !await authority.verifyScopeCheckpoint(checkpoint) ||
    checkpoint.authorityId !== record.receipt.authorityId ||
    (reviewRecord !== null && checkpoint.authorityId !== reviewRecord.receipt.authorityId) ||
    checkpoint.receiptCount < record.receipt.authoritySequence ||
    checkpoint.headReceiptDigest === null
  ) {
    throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "lifecycle receipt checkpoint is invalid");
  }
  let after = 0;
  let receiptCovered = false;
  let reviewReceiptCovered = false;
  let previousReceiptDigest: LifecycleDigest | null = null;
  const orderedRecordSet: Array<ReturnType<typeof receiptSetMember>> = [];
  do {
    const page = await authority.readScopePageAt(checkpoint.checkpointDigest, after, PAGE_LIMIT);
    if (
      page.orderedRecords.length > PAGE_LIMIT ||
      orderedRecordSet.length + page.orderedRecords.length > MAX_SCOPE_RECEIPTS
    ) {
      throw new FabricError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "lifecycle receipt checkpoint exceeds its bounded scan",
      );
    }
    for (const candidate of page.orderedRecords) {
      const expectedSequence = orderedRecordSet.length + 1;
      if (
        candidate.receipt.authorityId !== checkpoint.authorityId ||
        candidate.receipt.authoritySequence !== expectedSequence ||
        candidate.receipt.previousReceiptDigest !== previousReceiptDigest ||
        candidate.subject.projectSessionId !== prepared.projectSessionId ||
        candidate.subject.runId !== prepared.runId ||
        candidate.subject.kind !== candidate.receipt.kind ||
        !await authority.verifyReceipt(candidate.subject, candidate.receipt)
      ) {
        throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "lifecycle receipt checkpoint page is crossed");
      }
      orderedRecordSet.push(receiptSetMember(candidate));
      previousReceiptDigest = candidate.receipt.receiptDigest;
      receiptCovered ||= candidate.receipt.receiptDigest === record.receipt.receiptDigest &&
        canonicalJson(candidate.subject) === prepared.subjectJson;
      reviewReceiptCovered ||= reviewRecord !== null &&
        candidate.receipt.receiptDigest === reviewRecord.receipt.receiptDigest &&
        canonicalJson(candidate.subject) === prepared.review?.subjectJson;
    }
    if (page.nextAfter === null) {
      if (orderedRecordSet.length !== checkpoint.receiptCount) {
        throw new FabricError(
          "LIFECYCLE_PRECONDITION_FAILED",
          "lifecycle receipt checkpoint ended before its authenticated count",
        );
      }
      break;
    }
    if (
      page.orderedRecords.length === 0 ||
      page.nextAfter !== orderedRecordSet.length ||
      page.nextAfter <= after ||
      page.nextAfter > checkpoint.receiptCount
    ) {
      throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "lifecycle receipt checkpoint pagination crossed");
    }
    after = page.nextAfter;
  } while (true);
  if (
    !receiptCovered ||
    (prepared.review !== null && !reviewReceiptCovered) ||
    previousReceiptDigest !== checkpoint.headReceiptDigest ||
    checkpoint.orderedRecordSetDigest !== lifecycleDigest("scope-record-set", orderedRecordSet)
  ) {
    throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "lifecycle receipt is absent from its pinned checkpoint");
  }
  return { record, reviewRecord, checkpoint };
}
