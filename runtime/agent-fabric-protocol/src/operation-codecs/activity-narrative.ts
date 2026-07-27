import {
  arrayOf,
  boolean,
  boundedString,
  enumeration,
  identifier,
  integer,
  literal,
  nullable,
  objectCodec,
  parserBacked,
  sha256,
  timestamp,
  unionOf,
  type Codec,
} from "../codec.js";
import { positiveInteger, text } from "./common.js";

function compareIdentity(left: string, right: string): number {
  return left === right ? 0 : left < right ? -1 : 1;
}

function compareTimestamp(left: string, right: string): number {
  return Date.parse(left) - Date.parse(right) || compareIdentity(left, right);
}

function targetIdentity(value: unknown): string {
  if (value === null) return "null";
  const target = value as Record<string, unknown>;
  return `${String(target.kind)}\u0000${String(target.id)}`;
}

const ACTIVITY_MEMBER_CONTENT_PAGE_BYTES = 256 * 1024;
const MINIMUM_NONFINAL_ACTIVITY_MEMBER_CONTENT_PAGE_BYTES =
  ACTIVITY_MEMBER_CONTENT_PAGE_BYTES - 3;

export function createActivityNarrativeCodecs(dependencies: Readonly<{
  artifactRefCodec: Codec<unknown>;
  messageBodyRefCodec: Codec<unknown>;
}>) {
  const { artifactRefCodec, messageBodyRefCodec } = dependencies;
  const activityNarrativeTargetCodec = objectCodec({
    kind: enumeration(["task", "message", "gate", "provider-action", "artifact", "agent"]),
    id: identifier,
  });
  const activityNarrativeMemberCodec = objectCodec({
    ordinal: positiveInteger,
    eventId: identifier,
    eventKind: text,
    actorId: nullable(identifier),
    target: nullable(activityNarrativeTargetCodec),
    occurredAt: timestamp,
    sourceRevision: positiveInteger,
    detailAvailability: enumeration(["available", "unavailable"]),
    evidenceLinkCount: integer({ minimum: 0 }),
    evidenceLinksDigest: sha256,
  }, { messageBodyRef: messageBodyRefCodec });
  const activityNarrativeGroupBaseCodec = objectCodec({
    groupId: identifier,
    ordinal: positiveInteger,
    kind: enumeration(["task", "message", "gate", "provider-action", "artifact", "event"]),
    actorIds: arrayOf(identifier, { maximum: 32, unique: true }),
    target: nullable(activityNarrativeTargetCodec),
    eventKinds: arrayOf(text, { minimum: 1, maximum: 32, unique: true }),
    occurredAtRange: objectCodec({ first: timestamp, last: timestamp }),
    sourceRange: objectCodec({ first: positiveInteger, last: positiveInteger }),
    count: positiveInteger,
    evidenceLinkCount: integer({ minimum: 0 }),
    evidenceLinksDigest: sha256,
    evidenceLinksTruncated: boolean,
    evidenceLinks: arrayOf(artifactRefCodec, { maximum: 1 }),
    members: arrayOf(activityNarrativeMemberCodec, { minimum: 1, maximum: 32 }),
  });
  const activityNarrativeGroupCodec = parserBacked(
    activityNarrativeGroupBaseCodec,
    (value, path) => {
      const group = value as Record<string, unknown>;
      const members = group.members as ReadonlyArray<Record<string, unknown>>;
      const sourceRange = group.sourceRange as Record<string, number>;
      const occurredAtRange = group.occurredAtRange as Record<string, string>;
      if (group.count !== members.length) {
        throw new TypeError(`${path}.count must equal members.length`);
      }
      if (
        sourceRange.first === undefined ||
        sourceRange.last === undefined ||
        sourceRange.first > sourceRange.last
      ) {
        throw new TypeError(`${path}.sourceRange must be ascending`);
      }
      const eventIds = new Set<string>();
      for (const [index, member] of members.entries()) {
        if (member.ordinal !== index + 1) {
          throw new TypeError(`${path}.members must have contiguous ordinals`);
        }
        if (eventIds.has(member.eventId as string)) {
          throw new TypeError(`${path}.members must have unique eventId values`);
        }
        eventIds.add(member.eventId as string);
        if (
          index > 0 &&
          (members[index - 1]?.sourceRevision as number) >=
            (member.sourceRevision as number)
        ) {
          throw new TypeError(`${path}.members must be ordered by sourceRevision`);
        }
      }
      if (
        members[0]?.sourceRevision !== sourceRange.first ||
        members.at(-1)?.sourceRevision !== sourceRange.last
      ) {
        throw new TypeError(`${path}.sourceRange must bind the member range`);
      }
      if (
        occurredAtRange.first === undefined ||
        occurredAtRange.last === undefined ||
        compareTimestamp(occurredAtRange.first, occurredAtRange.last) > 0
      ) {
        throw new TypeError(`${path}.occurredAtRange must be ascending`);
      }
      const chronologicalMembers = [...members].sort((left, right) =>
        compareTimestamp(
          left.occurredAt as string,
          right.occurredAt as string,
        )
      );
      if (
        chronologicalMembers[0]?.occurredAt !== occurredAtRange.first ||
        chronologicalMembers.at(-1)?.occurredAt !== occurredAtRange.last
      ) {
        throw new TypeError(
          `${path}.occurredAtRange must bind the chronological member range`,
        );
      }
      const groupTarget = targetIdentity(group.target);
      if (members.some((member) => targetIdentity(member.target) !== groupTarget)) {
        throw new TypeError(`${path}.target must bind every group member`);
      }
      const target = group.target as Record<string, unknown> | null;
      const expectedKind = target === null || target.kind === "agent"
        ? "event"
        : target.kind;
      if (group.kind !== expectedKind) {
        throw new TypeError(`${path}.kind must be derived from the exact target`);
      }
      const expectedActors = [...new Set(
        members.flatMap((member) =>
          member.actorId === null ? [] : [member.actorId as string]
        ),
      )].sort(compareIdentity);
      if (JSON.stringify(group.actorIds) !== JSON.stringify(expectedActors)) {
        throw new TypeError(`${path}.actorIds must bind the member actors`);
      }
      const expectedEventKinds = [...new Set(
        members.map((member) => member.eventKind as string),
      )];
      if (
        JSON.stringify(group.eventKinds) !==
          JSON.stringify(expectedEventKinds)
      ) {
        throw new TypeError(`${path}.eventKinds must bind the ordered members`);
      }
      const evidenceLinks = group.evidenceLinks as ReadonlyArray<unknown>;
      const evidenceLinkCount = group.evidenceLinkCount as number;
      if (
        evidenceLinkCount < evidenceLinks.length ||
        group.evidenceLinksTruncated !==
          (evidenceLinkCount > evidenceLinks.length)
      ) {
        throw new TypeError(
          `${path}.evidenceLinksTruncated must bind the retained evidence count`,
        );
      }
      const memberEvidenceUpperBound = members.reduce(
        (total, member) => total + (member.evidenceLinkCount as number),
        0,
      );
      const memberEvidenceLowerBound = Math.max(
        0,
        ...members.map((member) => member.evidenceLinkCount as number),
      );
      if (
        evidenceLinkCount < memberEvidenceLowerBound ||
        evidenceLinkCount > memberEvidenceUpperBound
      ) {
        throw new TypeError(
          `${path}.evidenceLinkCount must be possible for the member evidence counts`,
        );
      }
      return value;
    },
    activityNarrativeGroupBaseCodec.example,
  );
  const activityNarrativeMemberDetailCodec = unionOf([
    objectCodec({
      eventId: identifier,
      status: literal("available"),
      content: text,
      transformation: enumeration([
        "none",
        "terminal-neutralised",
        "capability-redacted",
        "credential-redacted",
        "combined",
      ]),
    }),
    objectCodec({
      eventId: identifier,
      status: literal("referenced"),
      contentBytes: integer({ minimum: 0 }),
      contentDigest: sha256,
      detailRef: objectCodec({
        kind: literal("activity"),
        groupId: identifier,
        eventId: identifier,
        expectedRevision: positiveInteger,
        contentOffset: integer({ minimum: 0 }),
      }),
    }),
    objectCodec({
      eventId: identifier,
      status: literal("unavailable"),
      reason: text,
    }),
  ]);
  const groupedActivityDetailBaseCodec = objectCodec({
    kind: literal("activity"),
    group: activityNarrativeGroupCodec,
    memberDetails: arrayOf(activityNarrativeMemberDetailCodec, {
      minimum: 1,
      maximum: 32,
    }),
  });
  const groupedActivityDetailCodec = parserBacked(
    groupedActivityDetailBaseCodec,
    (value, path) => {
      const detail = value as Record<string, unknown>;
      const group = detail.group as Record<string, unknown>;
      const members = group.members as ReadonlyArray<Record<string, unknown>>;
      const memberDetails =
        detail.memberDetails as ReadonlyArray<Record<string, unknown>>;
      if (
        members.length !== memberDetails.length ||
        members.some(
          (member, index) =>
            member.eventId !== memberDetails[index]?.eventId ||
            member.detailAvailability !== (
              memberDetails[index]?.status === "referenced"
                ? "available"
                : memberDetails[index]?.status
            ),
        )
      ) {
        throw new TypeError(
          `${path}.memberDetails must bind every ordered group member`,
        );
      }
      for (const [index, memberDetail] of memberDetails.entries()) {
        if (memberDetail.status !== "referenced") continue;
        const member = members[index];
        const detailRef = memberDetail.detailRef as Record<string, unknown>;
        if (
          detailRef.groupId !== group.groupId ||
          detailRef.eventId !== member?.eventId ||
          detailRef.expectedRevision !== member?.sourceRevision ||
          detailRef.contentOffset !== 0
        ) {
          throw new TypeError(
            `${path}.memberDetails must bind referenced detail to its group member`,
          );
        }
      }
      return value;
    },
    groupedActivityDetailBaseCodec.example,
  );
  const activityNarrativeMemberContentDetailBaseCodec = objectCodec({
    kind: literal("activity"),
    groupId: identifier,
    eventId: identifier,
    sourceRevision: positiveInteger,
    contentOffset: integer({ minimum: 0 }),
    content: boundedString({ minBytes: 0, maxBytes: ACTIVITY_MEMBER_CONTENT_PAGE_BYTES, example: "detail page" }),
    contentBytes: integer({ minimum: 0 }),
    contentDigest: sha256,
    transformation: enumeration([
      "none",
      "terminal-neutralised",
      "capability-redacted",
      "credential-redacted",
      "combined",
    ]),
    nextDetailRef: nullable(objectCodec({
      kind: literal("activity"),
      groupId: identifier,
      eventId: identifier,
      expectedRevision: positiveInteger,
      contentOffset: integer({ minimum: 0 }),
    })),
  });
  const activityNarrativeMemberContentDetailCodec = parserBacked(
    activityNarrativeMemberContentDetailBaseCodec,
    (value, path) => {
      const detail = value as Record<string, unknown>;
      const pageBytes = Buffer.byteLength(detail.content as string);
      const contentEnd = (detail.contentOffset as number) + pageBytes;
      const contentBytes = detail.contentBytes as number;
      if (contentEnd > contentBytes) {
        throw new TypeError(`${path}.content exceeds the declared byte count`);
      }
      const next = detail.nextDetailRef as Record<string, unknown> | null;
      if (next === null) {
        if (contentEnd !== contentBytes) {
          throw new TypeError(
            `${path}.nextDetailRef is required before the declared byte end`,
          );
        }
      } else {
        if (
          pageBytes < MINIMUM_NONFINAL_ACTIVITY_MEMBER_CONTENT_PAGE_BYTES ||
          pageBytes > ACTIVITY_MEMBER_CONTENT_PAGE_BYTES
        ) {
          throw new TypeError(
            `${path}.content must fill a UTF-8-boundary-safe non-final page`,
          );
        }
        if (
          next.groupId !== detail.groupId ||
          next.eventId !== detail.eventId ||
          next.expectedRevision !== detail.sourceRevision ||
          next.contentOffset !== contentEnd
        ) {
          throw new TypeError(
            `${path}.nextDetailRef must bind the exact next content byte`,
          );
        }
      }
      return value;
    },
    activityNarrativeMemberContentDetailBaseCodec.example,
  );
  return {
    activityNarrativeGroupCodec,
    groupedActivityDetailCodec,
    activityNarrativeMemberContentDetailCodec,
  };
}
