import type {
  ActivityNarrativeGroup,
  ArtifactContentTransformation,
  ArtifactLineFragment,
  ArtifactMediaType,
  ArtifactRef,
  CoordinationRunId,
  EvidenceKind,
  EvidenceSourceKind,
  GitRepositoryProjection,
  MessageBodyReadResult,
  OperatorDetail,
  ProjectSessionId,
  RunProjectionTarget,
  Sha256Digest,
  TaskId,
  Timestamp,
} from "@local/agent-fabric-protocol";

import type { FabricView, Revision } from "./model.js";
import type { ConsoleRunProjection } from "./run-projection-adapter.js";

export type ConsoleArtifactContentPage = Readonly<{
  pageIndex: number;
  lineFragment: ArtifactLineFragment;
  pageContentDigest: Sha256Digest;
  bytes: number;
}>;

export type ConsoleArtifactContentResult = Readonly<{
  artifactRef: ArtifactRef;
  evidenceRevision: number;
  evidenceKind: EvidenceKind;
  sourceKind: EvidenceSourceKind;
  publisherKind: "agent" | "operator" | "fabric" | "project" | "migration";
  publisherRef: string;
  projectSessionId: ProjectSessionId | null;
  coordinationRunId: CoordinationRunId | null;
  taskId: TaskId | null;
  createdAt: Timestamp;
  mediaType: ArtifactMediaType;
  content: string;
  totalBytes: number;
  totalLines: number;
  renderedTotalBytes: number;
  renderedTotalLines: number;
  renderedArtifactDigest: Sha256Digest;
  transformation: ArtifactContentTransformation;
  terminalNeutralised: true;
  capabilityValuesRedacted: true;
  credentialValuesRedacted: true;
  pages: readonly ConsoleArtifactContentPage[];
  coverage: Readonly<{
    complete: true;
    verified: true;
    pageCount: number;
  }>;
  reviewDisposition: "eligible" | "confirm-terminal-neutralised" | "blocked-redacted";
}>;

export type ConsoleInspectionBinding = Readonly<{
  view: FabricView;
  itemId: string;
  itemRevision: Revision;
  projectionRevision: Revision;
  projectSessionId?: ProjectSessionId;
  runTarget?: RunProjectionTarget;
}>;

export type ConsoleReadInspection =
  | Readonly<{
      kind: "run";
      state: "current";
      binding: ConsoleInspectionBinding;
      result: ConsoleRunProjection;
    }>
  | Readonly<{
      kind: "run";
      state: "unavailable";
      binding: ConsoleInspectionBinding;
      reason:
        | "feature-unavailable"
        | "projection-changed"
        | "contract-invalid"
        | "transport-failure";
    }>
  | Readonly<{
      kind: "activity";
      state: "current";
      binding: ConsoleInspectionBinding;
      readTransactionId: string;
      detail: Extract<
        OperatorDetail,
        { kind: "activity"; group: ActivityNarrativeGroup }
      >;
      messages: readonly (
        | Readonly<{
            eventId: string;
            state: "current";
            result: Extract<MessageBodyReadResult, { available: true }>;
          }>
        | Readonly<{
            eventId: string;
            state: "unavailable";
            reason:
              | "feature-unavailable"
              | "message-not-found"
              | "message-forbidden"
              | "message-expired"
              | "projection-changed"
              | "contract-invalid"
              | "transport-failure";
          }>
      )[];
    }>
  | Readonly<{
      kind: "activity";
      state: "unavailable";
      binding: ConsoleInspectionBinding;
      reason:
        | "feature-unavailable"
        | "projection-changed"
        | "detail-unavailable"
        | "detail-conflict"
        | "detail-invalid"
        | "contract-invalid"
        | "transport-failure";
    }>
  | Readonly<{
      kind: "message";
      state: "current";
      binding: ConsoleInspectionBinding;
      result: Extract<MessageBodyReadResult, { available: true }>;
    }>
  | Readonly<{
      kind: "message";
      state: "unavailable";
      binding: ConsoleInspectionBinding;
      reason:
        | "feature-unavailable"
        | "message-not-found"
        | "message-forbidden"
        | "message-expired"
        | "projection-changed"
        | "contract-invalid"
        | "transport-failure";
    }>
  | Readonly<{
      kind: "repository";
      state: "current";
      binding: ConsoleInspectionBinding;
      readTransactionId: string;
      repository: GitRepositoryProjection;
    }>
  | Readonly<{
      kind: "repository";
      state: "unavailable";
      binding: ConsoleInspectionBinding;
      reason:
        | "feature-unavailable"
        | "projection-changed"
        | "detail-unavailable"
        | "detail-conflict"
        | "detail-invalid"
        | "repository-resnapshot-required"
        | "contract-invalid"
        | "transport-failure";
    }>
  | Readonly<{
      kind: "artifact";
      state: "current";
      binding: ConsoleInspectionBinding;
      readTransactionId: string;
      result: ConsoleArtifactContentResult;
    }>
  | Readonly<{
      kind: "artifact";
      state: "unavailable";
      binding: ConsoleInspectionBinding;
      reason:
        | "feature-unavailable"
        | "projection-changed"
        | "detail-unavailable"
        | "detail-conflict"
        | "detail-invalid"
        | "artifact-not-found"
        | "artifact-forbidden"
        | "artifact-unsupported-media"
        | "artifact-unsafe-content"
        | "artifact-stale"
        | "artifact-oversized"
        | "contract-invalid"
        | "transport-failure";
    }>;
