import { createHash } from "node:crypto";

import type {
  ActivityNarrativeGroup,
  ActivityNarrativeGroupKind,
  ActivityNarrativeMemberDetail,
  ActivityNarrativeTarget,
  ArtifactRef,
  MessageBodyRef,
  Sha256Digest,
  Timestamp,
} from "@local/agent-fabric-protocol";

import { redactArtifactTextPreservingControls } from "./artifact-content-safety.js";

export type ActivityNarrativeEventInput = Readonly<{
  eventId: string;
  eventKind: string;
  actorId: string | null;
  payloadJson: string;
  occurredAt: Timestamp;
  sourceRevision: number;
  messageBodyRef: MessageBodyRef | null;
  messageTarget: ActivityNarrativeTarget | null;
}>;

export type ProjectedActivityNarrativeGroup = Readonly<{
  group: ActivityNarrativeGroup;
  memberDetails: readonly ActivityNarrativeMemberDetail[];
  memberContents: readonly ProjectedActivityNarrativeMemberContent[];
}>;

export type ProjectedActivityNarrativeMemberContent =
  | Readonly<{
      eventId: string;
      status: "available";
      content: string;
      transformation:
        | "none"
        | "terminal-neutralised"
        | "capability-redacted"
        | "credential-redacted"
        | "combined";
    }>
  | Readonly<{ eventId: string; status: "unavailable"; reason: string }>;

const MAXIMUM_GROUP_MEMBERS = 32;
const MAXIMUM_INLINE_EVIDENCE_LINKS = 1;

export function compareActivityIdentity(left: string, right: string): number {
  return left === right ? 0 : left < right ? -1 : 1;
}

function compareActivityTimestamp(left: Timestamp, right: Timestamp): number {
  return Date.parse(left) - Date.parse(right) ||
    compareActivityIdentity(left, right);
}

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function stringField(
  value: Record<string, unknown>,
  field: string,
): string | null {
  const candidate = value[field];
  return typeof candidate === "string" && candidate.length > 0
    ? candidate
    : null;
}

function targetFromPayload(
  payload: Record<string, unknown>,
): ActivityNarrativeTarget | null {
  const candidates = [
    ["task", "taskId"],
    ["gate", "gateId"],
    ["provider-action", "actionId"],
    ["artifact", "artifactId"],
    ["message", "messageId"],
    ["agent", "targetAgentId"],
    ["agent", "agentId"],
  ] as const;
  for (const [kind, field] of candidates) {
    const id = stringField(payload, field);
    if (id !== null) return { kind, id };
  }
  return null;
}

function artifactRef(value: unknown): ArtifactRef | null {
  const candidate = record(value);
  if (candidate === null) return null;
  const path = stringField(candidate, "path");
  const digest = stringField(candidate, "digest");
  return path !== null && digest !== null && /^sha256:[0-9a-f]{64}$/u.test(digest)
    ? {
        path: path as ArtifactRef["path"],
        digest: digest as ArtifactRef["digest"],
      }
    : null;
}

function evidenceLinks(payload: Record<string, unknown>): ArtifactRef[] {
  const values = [
    artifactRef(payload.artifactRef),
    ...(Array.isArray(payload.evidenceLinks)
      ? payload.evidenceLinks.map(artifactRef)
      : []),
  ].filter((value): value is ArtifactRef => value !== null);
  const byIdentity = new Map(
    values.map((value) => [`${value.path}\u0000${value.digest}`, value]),
  );
  return [...byIdentity.entries()]
    .sort(([left], [right]) => compareActivityIdentity(left, right))
    .map(([, value]) => value);
}

function evidenceLinksDigest(links: readonly ArtifactRef[]): Sha256Digest {
  return `sha256:${createHash("sha256")
    .update(JSON.stringify(links))
    .digest("hex")}` as Sha256Digest;
}

function detailProjection(
  input: ActivityNarrativeEventInput,
): ProjectedActivityNarrativeMemberContent {
  const redacted = redactArtifactTextPreservingControls(input.payloadJson);
  if (!redacted.safe) {
    return {
      eventId: input.eventId,
      status: "unavailable",
      reason: "unsafe-content",
    };
  }
  return {
    eventId: input.eventId,
    status: "available",
    content: redacted.content,
    transformation: redacted.transformation,
  };
}

function chunkActivityEvents<T>(ordered: readonly T[]): readonly T[][] {
  return Array.from(
    { length: Math.ceil(ordered.length / MAXIMUM_GROUP_MEMBERS) },
    (_, index) => ordered.slice(
      index * MAXIMUM_GROUP_MEMBERS,
      (index + 1) * MAXIMUM_GROUP_MEMBERS,
    ),
  );
}

function groupKind(
  target: ActivityNarrativeTarget | null,
): ActivityNarrativeGroupKind {
  if (target === null || target.kind === "agent") return "event";
  return target.kind;
}

function groupIdentity(
  runId: string,
  target: ActivityNarrativeTarget | null,
  eventId: string,
  chunkIndex = 0,
): string {
  const exact = target === null
    ? `event\u0000${eventId}`
    : `${target.kind}\u0000${target.id}`;
  const digest = createHash("sha256")
    .update(`${runId}\u0000${exact}\u0000${String(chunkIndex)}`)
    .digest("hex")
    .slice(0, 24);
  return `activity_group_${digest}`;
}

function eventTarget(
  input: ActivityNarrativeEventInput,
  payload: Record<string, unknown>,
): ActivityNarrativeTarget | null {
  return input.messageTarget ?? targetFromPayload(payload);
}

export function projectActivityNarrativeGroups(
  runId: string,
  inputs: readonly ActivityNarrativeEventInput[],
): readonly ProjectedActivityNarrativeGroup[] {
  const normalised = inputs.map((input) => {
    const parsed = record(JSON.parse(input.payloadJson));
    if (parsed === null) {
      throw new TypeError("activity event payload must be a JSON object");
    }
    const target = eventTarget(input, parsed);
    return {
      input,
      payload: parsed,
      target,
      groupId: groupIdentity(runId, target, input.eventId),
      evidenceLinks: evidenceLinks(parsed),
      detailProjection: detailProjection(input),
    };
  });
  const grouped = new Map<string, typeof normalised>();
  for (const event of normalised) {
    const values = grouped.get(event.groupId);
    if (values === undefined) grouped.set(event.groupId, [event]);
    else values.push(event);
  }

  const chunks = [...grouped.values()].flatMap((values) => {
    const ordered = [...values].sort((left, right) =>
      left.input.sourceRevision - right.input.sourceRevision ||
      compareActivityIdentity(left.input.eventId, right.input.eventId)
    );
    return chunkActivityEvents(ordered).map((chunk, chunkIndex) => ({
      ordered: chunk,
      chunkIndex,
    }));
  });
  const projected = chunks.map(({ ordered, chunkIndex }) => {
    const first = ordered[0];
    const last = ordered.at(-1);
    if (first === undefined || last === undefined) {
      throw new TypeError("activity narrative group cannot be empty");
    }
    const actors = [...new Set(
      ordered.flatMap(({ input }) =>
        input.actorId === null ? [] : [input.actorId]
      ),
    )].sort(compareActivityIdentity);
    const kinds = [...new Set(ordered.map(({ input }) => input.eventKind))];
    const linksByIdentity = new Map<string, ArtifactRef>();
    for (const value of ordered.flatMap((event) => event.evidenceLinks)) {
      linksByIdentity.set(`${value.path}\u0000${value.digest}`, value);
    }
    const links = [...linksByIdentity.entries()]
      .sort(([left], [right]) => compareActivityIdentity(left, right))
      .map(([, value]) => value);
    const projectedGroupId = groupIdentity(
      runId,
      first.target,
      first.input.eventId,
      chunkIndex,
    );
    const members = ordered.map((event, index) => ({
      ordinal: index + 1,
      eventId: event.input.eventId,
      eventKind: event.input.eventKind,
      actorId: event.input.actorId,
      target: event.target,
      occurredAt: event.input.occurredAt,
      sourceRevision: event.input.sourceRevision,
      ...(event.input.messageBodyRef === null
        ? {}
        : { messageBodyRef: event.input.messageBodyRef }),
      detailAvailability: event.detailProjection.status === "available"
        ? "available" as const
        : "unavailable" as const,
      evidenceLinkCount: event.evidenceLinks.length,
      evidenceLinksDigest: evidenceLinksDigest(event.evidenceLinks),
    }));
    const firstMember = members[0];
    if (firstMember === undefined) {
      throw new TypeError("activity narrative group cannot be empty");
    }
    const chronologicalMembers = [...members].sort((left, right) =>
      compareActivityTimestamp(left.occurredAt, right.occurredAt)
    );
    const chronologicalFirst = chronologicalMembers[0];
    const chronologicalLast = chronologicalMembers.at(-1);
    if (chronologicalFirst === undefined || chronologicalLast === undefined) {
      throw new TypeError("activity narrative group cannot be empty");
    }
    const memberDetails = ordered.map(({ input, detailProjection: detail }) =>
      detail.status === "available"
        ? {
            eventId: input.eventId,
            status: "referenced" as const,
            contentBytes: Buffer.byteLength(detail.content),
            contentDigest: `sha256:${createHash("sha256")
              .update(detail.content)
              .digest("hex")}` as Sha256Digest,
            detailRef: {
              kind: "activity" as const,
              groupId: projectedGroupId,
              eventId: input.eventId,
              expectedRevision: input.sourceRevision,
              contentOffset: 0,
            },
          }
        : detail
    );
    return {
      group: {
        groupId: projectedGroupId,
        ordinal: 0,
        kind: groupKind(first.target),
        actorIds: actors,
        target: first.target,
        eventKinds: kinds,
        occurredAtRange: {
          first: chronologicalFirst.occurredAt,
          last: chronologicalLast.occurredAt,
        },
        sourceRange: {
          first: first.input.sourceRevision,
          last: last.input.sourceRevision,
        },
        count: members.length,
        evidenceLinkCount: links.length,
        evidenceLinksDigest: evidenceLinksDigest(links),
        evidenceLinksTruncated:
          links.length > MAXIMUM_INLINE_EVIDENCE_LINKS,
        evidenceLinks: links.slice(0, MAXIMUM_INLINE_EVIDENCE_LINKS),
        members: [firstMember, ...members.slice(1)] as const,
      },
      memberDetails,
      memberContents: ordered.map(({ detailProjection: detail }) => detail),
    };
  });

  return projected
    .sort((left, right) =>
      right.group.sourceRange.last - left.group.sourceRange.last ||
      compareActivityIdentity(left.group.groupId, right.group.groupId)
    )
    .map((value, index) => ({
      ...value,
      group: { ...value.group, ordinal: index + 1 },
    }));
}
