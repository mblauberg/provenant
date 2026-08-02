import { FABRIC_OPERATIONS } from "../operations.js";
import {
  REVIEW_TARGET_PREPARATION_ACCEPTED_V1_CODEC,
  REVIEW_TARGET_PREPARATION_READ_REQUEST_V1_CODEC,
  REVIEW_TARGET_PREPARATION_READ_ERROR_V1_CODEC,
  REVIEW_TARGET_PREPARATION_READ_V1_CODEC,
  REVIEW_TARGET_PREPARE_V1_CODEC,
  REVIEW_TARGET_REBIND_RECEIPT_V1_CODEC,
  REVIEW_TARGET_REBIND_V1_CODEC,
} from "../provider-review-core.js";
import {
  REVIEW_EVIDENCE_ANNOTATION_APPEND_REQUEST_V1_CODEC,
  REVIEW_EVIDENCE_ANNOTATION_CURRENT_READ_REQUEST_V1_CODEC,
  REVIEW_EVIDENCE_ANNOTATION_CURRENT_READ_RESULT_V1_CODEC,
  REVIEW_EVIDENCE_ANNOTATION_V1_CODEC,
  REVIEW_EVIDENCE_LIST_REQUEST_V1_CODEC,
  REVIEW_EVIDENCE_LIST_RESULT_V1_CODEC,
  REVIEW_EVIDENCE_READ_REQUEST_V1_CODEC,
  REVIEW_EVIDENCE_READ_V1_CODEC,
  REVIEW_FINDING_PAGE_READ_REQUEST_V1_CODEC,
  REVIEW_FINDING_PAGE_READ_RESULT_V1_CODEC,
  REVIEW_READ_ERROR_V1_CODEC,
} from "../provider-review-evidence.js";
import { REVIEW_COMPLETION_READ_REQUEST_V1_CODEC, REVIEW_COMPLETION_V1_CODEC } from "../provider-review-completion.js";
import {
  PROVIDER_ROUTE_INTEGRITY_RECOVERY_PROJECTION_V1_CODEC,
  PROVIDER_ROUTE_INTEGRITY_RECOVERY_READ_ERROR_V1_CODEC,
  PROVIDER_ROUTE_INTEGRITY_RECOVERY_READ_REQUEST_V1_CODEC,
} from "../provider-review-recovery.js";
import { PROVIDER_CONTEXT_PRESSURE_READ_REQUEST_V1_CODEC, PROVIDER_CONTEXT_PRESSURE_READ_V1_CODEC } from "../route-lineage.js";
import { unionOf } from "../codec.js";
import { object, type OperationCodecFragment, type OperationShapeFragment } from "./common.js";

export const PROVIDER_REVIEW_INPUT_SHAPES = {
  [FABRIC_OPERATIONS.reviewTargetPrepare]: object(["schemaVersion", "commandId", "taskId", "expectedTargetGeneration", "deliveryManifestRef"]),
  [FABRIC_OPERATIONS.reviewTargetPreparationRead]: object(["schemaVersion", "projectSessionId", "coordinationRunId", "preparationId"]),
  [FABRIC_OPERATIONS.reviewTargetRebind]: object(["schemaVersion", "commandId", "targetGeneration", "expectedChairBindingGeneration", "lifecycleCustodyRef"]),
  [FABRIC_OPERATIONS.reviewEvidenceRead]: object(["schemaVersion", "projectSessionId", "coordinationRunId", "evidenceId"]),
  [FABRIC_OPERATIONS.reviewEvidenceList]: object(["schemaVersion", "projectSessionId", "coordinationRunId", "targetGeneration", "slot", "pageSize", "cursor"]),
  [FABRIC_OPERATIONS.reviewEvidenceAnnotate]: object(["schemaVersion", "commandId", "projectSessionId", "coordinationRunId", "evidenceId", "expectedResultDigest", "expectedHeadGeneration", "expectedAnnotationRevision", "disposition", "note"]),
  [FABRIC_OPERATIONS.reviewEvidenceAnnotationCurrentRead]: object(["schemaVersion", "projectSessionId", "coordinationRunId", "evidenceId"]),
  [FABRIC_OPERATIONS.reviewFindingPageRead]: object(["schemaVersion", "projectSessionId", "coordinationRunId", "findingSetDigest", "pageDigest"]),
  [FABRIC_OPERATIONS.reviewCompletionRead]: object(["schemaVersion", "projectSessionId", "coordinationRunId"]),
  [FABRIC_OPERATIONS.providerRouteIntegrityRecoveryRead]: object(["schemaVersion", "projectSessionId", "coordinationRunId", "actionRef"]),
  [FABRIC_OPERATIONS.providerContextPressureRead]: object(["schemaVersion", "projectSessionId", "coordinationRunId", "agentId"]),
} as const satisfies OperationShapeFragment;

export const PROVIDER_REVIEW_RESULT_SHAPES = {
  [FABRIC_OPERATIONS.reviewTargetPrepare]: object(["schemaVersion", "preparationId", "ownerCommandId", "inputDigest", "projectSessionId", "coordinationRunId", "taskId", "expectedTargetGeneration", "reservedTargetGeneration", "reservedBundleGeneration", "deliveryManifestRef", "state", "acceptedReceiptDigest"]),
  [FABRIC_OPERATIONS.reviewTargetPreparationRead]: object(["schemaVersion", "accepted", "revision", "state", "phase", "progress", "terminal"]),
  [FABRIC_OPERATIONS.reviewTargetRebind]: object(["schemaVersion", "status", "targetGeneration", "reviewSubjectDigest", "priorBindingGeneration", "newBindingGeneration", "priorBindingDigest", "newBindingDigest", "lifecycleAdoptionDigest", "bundleDigest", "profileDigest", "slotHeadSetDigest", "openAndRepairFindingSetDigest", "rebindReceiptDigest"]),
  [FABRIC_OPERATIONS.reviewEvidenceRead]: object(["schemaVersion", "record", "currency", "annotation"]),
  [FABRIC_OPERATIONS.reviewEvidenceList]: object(["schemaVersion", "entries", "nextCursor"]),
  [FABRIC_OPERATIONS.reviewEvidenceAnnotate]: object(["schemaVersion", "evidenceId", "annotationRevision", "priorAnnotationRevision", "commandId", "chairBindingGeneration", "disposition", "note", "noteDigest", "annotationDigest"]),
  [FABRIC_OPERATIONS.reviewEvidenceAnnotationCurrentRead]: object(["schemaVersion", "evidenceId", "annotation"]),
  [FABRIC_OPERATIONS.reviewFindingPageRead]: object(["schemaVersion", "findingSetDigest", "pageDigest", "members", "nextPageDigest"]),
  [FABRIC_OPERATIONS.reviewCompletionRead]: object(["schemaVersion", "blockers", "targetGeneration", "targetChair", "reviewedArtifactRef", "publicationLineageDigest", "bundleDigest", "manifestRootDigest", "coverageDigest", "riskReadMapDigest", "mandatoryReadSetDigest", "profileDigest", "unavailableSlots", "slots", "finalReviewComplete"]),
  [FABRIC_OPERATIONS.providerRouteIntegrityRecoveryRead]: object(["schemaVersion", "projectSessionId", "coordinationRunId", "taskId", "actionRef", "targetGeneration", "slot", "attemptGeneration", "recoveryGeneration", "state", "reason", "reservationDigest", "routeState", "routeReceiptDigest", "lookupState", "lookupEvidenceDigest", "disposition", "settlementDigest", "recoveryEvidenceDigest", "retirementEligible"]),
  [FABRIC_OPERATIONS.providerContextPressureRead]: object(["schemaVersion", "currency", "pressure", "readAt", "ageSeconds"]),
} as const satisfies OperationShapeFragment;

export const providerReviewOperationCodecFragment = {
  [FABRIC_OPERATIONS.reviewTargetPrepare]: { input: REVIEW_TARGET_PREPARE_V1_CODEC, result: REVIEW_TARGET_PREPARATION_ACCEPTED_V1_CODEC },
  [FABRIC_OPERATIONS.reviewTargetPreparationRead]: { input: REVIEW_TARGET_PREPARATION_READ_REQUEST_V1_CODEC, result: unionOf([REVIEW_TARGET_PREPARATION_READ_V1_CODEC, REVIEW_TARGET_PREPARATION_READ_ERROR_V1_CODEC]) },
  [FABRIC_OPERATIONS.reviewTargetRebind]: { input: REVIEW_TARGET_REBIND_V1_CODEC, result: REVIEW_TARGET_REBIND_RECEIPT_V1_CODEC },
  [FABRIC_OPERATIONS.reviewEvidenceRead]: { input: REVIEW_EVIDENCE_READ_REQUEST_V1_CODEC, result: unionOf([REVIEW_EVIDENCE_READ_V1_CODEC, REVIEW_READ_ERROR_V1_CODEC]) },
  [FABRIC_OPERATIONS.reviewEvidenceList]: { input: REVIEW_EVIDENCE_LIST_REQUEST_V1_CODEC, result: unionOf([REVIEW_EVIDENCE_LIST_RESULT_V1_CODEC, REVIEW_READ_ERROR_V1_CODEC]) },
  [FABRIC_OPERATIONS.reviewEvidenceAnnotate]: { input: REVIEW_EVIDENCE_ANNOTATION_APPEND_REQUEST_V1_CODEC, result: unionOf([REVIEW_EVIDENCE_ANNOTATION_V1_CODEC, REVIEW_READ_ERROR_V1_CODEC]) },
  [FABRIC_OPERATIONS.reviewEvidenceAnnotationCurrentRead]: { input: REVIEW_EVIDENCE_ANNOTATION_CURRENT_READ_REQUEST_V1_CODEC, result: unionOf([REVIEW_EVIDENCE_ANNOTATION_CURRENT_READ_RESULT_V1_CODEC, REVIEW_READ_ERROR_V1_CODEC]) },
  [FABRIC_OPERATIONS.reviewFindingPageRead]: { input: REVIEW_FINDING_PAGE_READ_REQUEST_V1_CODEC, result: unionOf([REVIEW_FINDING_PAGE_READ_RESULT_V1_CODEC, REVIEW_READ_ERROR_V1_CODEC]) },
  [FABRIC_OPERATIONS.reviewCompletionRead]: { input: REVIEW_COMPLETION_READ_REQUEST_V1_CODEC, result: unionOf([REVIEW_COMPLETION_V1_CODEC, REVIEW_READ_ERROR_V1_CODEC]) },
  [FABRIC_OPERATIONS.providerRouteIntegrityRecoveryRead]: { input: PROVIDER_ROUTE_INTEGRITY_RECOVERY_READ_REQUEST_V1_CODEC, result: unionOf([PROVIDER_ROUTE_INTEGRITY_RECOVERY_PROJECTION_V1_CODEC, PROVIDER_ROUTE_INTEGRITY_RECOVERY_READ_ERROR_V1_CODEC]) },
  [FABRIC_OPERATIONS.providerContextPressureRead]: { input: PROVIDER_CONTEXT_PRESSURE_READ_REQUEST_V1_CODEC, result: PROVIDER_CONTEXT_PRESSURE_READ_V1_CODEC },
} satisfies OperationCodecFragment;
