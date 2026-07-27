import type {
  ArtifactRef,
  MessageId,
  ProjectSessionId,
  Sha256Digest,
  TaskId,
  Timestamp,
} from "./primitives.js";

export type MessageBodyRef = {
  projectSessionId: ProjectSessionId;
  messageId: MessageId;
  expectedRevision: number;
};

export const ACTIVITY_NARRATIVE_GROUP_KINDS = [
  "task",
  "message",
  "gate",
  "provider-action",
  "artifact",
  "event",
] as const;

export type ActivityNarrativeGroupKind =
  (typeof ACTIVITY_NARRATIVE_GROUP_KINDS)[number];

export type ActivityNarrativeTarget = {
  kind: Exclude<ActivityNarrativeGroupKind, "event"> | "agent";
  id: string;
};

export type ActivityNarrativeMember = {
  ordinal: number;
  eventId: string;
  eventKind: string;
  actorId: string | null;
  target: ActivityNarrativeTarget | null;
  occurredAt: Timestamp;
  sourceRevision: number;
  messageBodyRef?: MessageBodyRef;
  detailAvailability: "available" | "unavailable";
  evidenceLinkCount: number;
  evidenceLinksDigest: Sha256Digest;
};

export type ActivityNarrativeGroup = {
  groupId: string;
  ordinal: number;
  kind: ActivityNarrativeGroupKind;
  actorIds: readonly string[];
  target: ActivityNarrativeTarget | null;
  eventKinds: readonly string[];
  occurredAtRange: { first: Timestamp; last: Timestamp };
  sourceRange: { first: number; last: number };
  count: number;
  evidenceLinkCount: number;
  evidenceLinksDigest: Sha256Digest;
  evidenceLinksTruncated: boolean;
  evidenceLinks: readonly ArtifactRef[];
  members: readonly [ActivityNarrativeMember, ...ActivityNarrativeMember[]];
};

export type ActivityNarrativeMemberDetailRef = {
  kind: "activity";
  groupId: string;
  eventId: string;
  expectedRevision: number;
  contentOffset: number;
};

export type ActivityNarrativeMemberDetail =
  | {
      eventId: string;
      status: "available";
      content: string;
      transformation: "none" | "terminal-neutralised" | "capability-redacted" | "credential-redacted" | "combined";
    }
  | {
      eventId: string;
      status: "referenced";
      contentBytes: number;
      contentDigest: Sha256Digest;
      detailRef: ActivityNarrativeMemberDetailRef;
    }
  | { eventId: string; status: "unavailable"; reason: string };

type ActivityViewItemBase = {
  eventId: string;
  actorId: string | null;
  taskId: TaskId | null;
  summary: string;
  occurredAt: Timestamp;
  sourceRevision: number;
  group?: ActivityNarrativeGroup;
};

export type ActivityViewItem = ActivityViewItemBase & (
  | { kind: "message"; messageBodyRef: MessageBodyRef }
  | { kind: "decision" | "lifecycle" | "operation"; messageBodyRef?: never }
);

export type ActivityViewSummary =
  | ({
      kind: "activity";
      summary: string;
      occurredAt: Timestamp;
    } & (
      | { activityKind: "message"; messageBodyRef: MessageBodyRef }
      | { activityKind: "decision" | "lifecycle" | "operation"; messageBodyRef?: never }
    ))
  | {
      kind: "activity";
      summary: string;
      occurredAt: Timestamp;
      group: ActivityNarrativeGroup;
    };

export type ActivityDetail =
  | (Exclude<ActivityViewSummary, { group: ActivityNarrativeGroup }> & {
      eventId: string;
    })
  | {
      kind: "activity";
      group: ActivityNarrativeGroup;
      memberDetails: readonly [ActivityNarrativeMemberDetail, ...ActivityNarrativeMemberDetail[]];
    }
  | {
      kind: "activity";
      groupId: string;
      eventId: string;
      sourceRevision: number;
      contentOffset: number;
      content: string;
      contentBytes: number;
      contentDigest: Sha256Digest;
      transformation: "none" | "terminal-neutralised" | "capability-redacted" | "credential-redacted" | "combined";
      nextDetailRef: ActivityNarrativeMemberDetailRef | null;
    };
