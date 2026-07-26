import { createHash } from "node:crypto";

import { describe, expect, it } from "vitest";

import {
  ACTIVITY_NARRATIVE_GROUPING_FEATURE,
  ACTIVITY_NARRATIVE_GROUP_KINDS,
  FABRIC_OPERATIONS,
  PROTOCOL_LIMITS,
  PROTOCOL_FEATURES,
  assertOperationResultFeatureShape,
  parseOperationResult,
} from "../src/index.js";

const observedAt = "2026-07-26T00:00:00.000Z";
const laterAt = "2026-07-26T00:00:01.000Z";

const group = {
  groupId: "activity_group_01",
  ordinal: 2,
  kind: "task",
  actorIds: ["agent_01"],
  target: { kind: "task", id: "task_01" },
  eventKinds: ["message-persisted", "tool-invoked", "tool-result-recorded"],
  occurredAtRange: { first: observedAt, last: laterAt },
  sourceRange: { first: 7, last: 9 },
  count: 3,
  evidenceLinkCount: 0,
  evidenceLinksDigest:
    "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945",
  evidenceLinksTruncated: false,
  evidenceLinks: [],
  members: [
    {
      ordinal: 1,
      eventId: "event_message",
      eventKind: "message-persisted",
      actorId: "agent_01",
      target: { kind: "task", id: "task_01" },
      occurredAt: observedAt,
      sourceRevision: 7,
      messageBodyRef: {
        projectSessionId: "session_01",
        messageId: "message_01",
        expectedRevision: 1,
      },
      detailAvailability: "available",
      evidenceLinkCount: 0,
      evidenceLinksDigest:
        "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945",
    },
    {
      ordinal: 2,
      eventId: "event_tool",
      eventKind: "tool-invoked",
      actorId: "agent_01",
      target: { kind: "task", id: "task_01" },
      occurredAt: laterAt,
      sourceRevision: 8,
      detailAvailability: "available",
      evidenceLinkCount: 0,
      evidenceLinksDigest:
        "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945",
    },
    {
      ordinal: 3,
      eventId: "event_result",
      eventKind: "tool-result-recorded",
      actorId: "agent_01",
      target: { kind: "task", id: "task_01" },
      occurredAt: laterAt,
      sourceRevision: 9,
      detailAvailability: "available",
      evidenceLinkCount: 0,
      evidenceLinksDigest:
        "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945",
    },
  ],
} as const;

function activityRow(summary: unknown, detailRef: unknown = {
  kind: "activity",
  groupId: group.groupId,
  expectedRevision: group.sourceRange.last,
}) {
  return {
    itemId: group.groupId,
    itemRevision: group.sourceRange.last,
    fact: {
      freshness: "live",
      source: "fabric",
      revision: group.sourceRange.last,
      observedAt: laterAt,
      value: {
        summary,
        detailRef,
        actionAvailability: {
          state: "read-only",
          reason: "authority-insufficient",
        },
      },
    },
  };
}

function activityPage(rows: readonly unknown[]) {
  return {
    status: "page",
    view: "activity",
    rows,
    nextCursor: 9,
    hasMore: false,
    snapshotRevision: 11,
    readTransactionId: "read_activity_groups",
  };
}

const legacySummary = {
  kind: "activity",
  activityKind: "operation",
  summary: "tool-invoked",
  occurredAt: observedAt,
} as const;

const groupedSummary = {
  kind: "activity",
  summary: "task activity",
  occurredAt: laterAt,
  group,
} as const;

describe("activity-narrative-grouping.v1 closed result shape", () => {
  it("requires the grouped shape exactly when negotiated", () => {
    expect(ACTIVITY_NARRATIVE_GROUPING_FEATURE).toBe(
      "activity-narrative-grouping.v1",
    );
    expect(PROTOCOL_FEATURES).toContain(ACTIVITY_NARRATIVE_GROUPING_FEATURE);

    const legacy = parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      activityPage([activityRow(legacySummary, {
        kind: "activity",
        eventId: "event_tool",
        expectedRevision: 8,
      })]),
    );
    const grouped = parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      activityPage([activityRow(groupedSummary)]),
    );
    const legacyFeatures = ["operator-projection.v2"] as const;
    const groupedFeatures = [
      ...legacyFeatures,
      ACTIVITY_NARRATIVE_GROUPING_FEATURE,
    ] as const;

    expect(assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionViewPage,
      groupedFeatures,
      grouped,
    )).toBe(grouped);
    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionViewPage,
      groupedFeatures,
      legacy,
    )).toThrow(expect.objectContaining({
      code: "PROTOCOL_INCOMPATIBLE",
      reason: "missing-negotiated-field",
    }));
    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionViewPage,
      legacyFeatures,
      grouped,
    )).toThrow(expect.objectContaining({
      code: "PROTOCOL_INCOMPATIBLE",
      reason: "unnegotiated-field",
    }));
  });

  it("rejects mixed grouped and legacy presence", () => {
    const mixed = parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      activityPage([
        activityRow(groupedSummary),
        activityRow(legacySummary, {
          kind: "activity",
          eventId: "event_legacy",
          expectedRevision: 10,
        }),
      ]),
    );
    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionViewPage,
      ["operator-projection.v2", ACTIVITY_NARRATIVE_GROUPING_FEATURE],
      mixed,
    )).toThrow(expect.objectContaining({ reason: "mixed-presence" }));
  });

  it("pins a vocabulary that cannot claim lifecycle, progress, or completion", () => {
    expect(ACTIVITY_NARRATIVE_GROUP_KINDS).toStrictEqual([
      "task",
      "message",
      "gate",
      "provider-action",
      "artifact",
      "event",
    ]);
    expect(ACTIVITY_NARRATIVE_GROUP_KINDS.join(" ")).not.toMatch(
      /\b(?:complete|completed|done|progress|succeed|succeeded|success)\b/iu,
    );
  });

  it("rejects contradictory aggregate ordering and member identity", () => {
    for (const malformed of [
      { ...group, count: 2 },
      { ...group, sourceRange: { first: 9, last: 7 } },
      { ...group, members: [group.members[1], group.members[0], group.members[2]] },
      {
        ...group,
        members: [
          group.members[0],
          { ...group.members[1], eventId: group.members[0].eventId },
          group.members[2],
        ],
      },
      {
        ...group,
        members: group.members.map((member) => ({
          ...member,
          target: { kind: "task" as const, id: "task_02" },
        })),
      },
      {
        ...group,
        members: group.members.map((member) => ({
          ...member,
          actorId: "agent_02",
        })),
      },
      { ...group, kind: "completed" },
    ]) {
      expect(() => parseOperationResult(
        FABRIC_OPERATIONS.projectionViewPage,
        activityPage([activityRow({ ...groupedSummary, group: malformed })]),
      )).toThrowError();
    }
  });

  it("binds grouped rows and detail references to the embedded fact", () => {
    for (const malformedRow of [
      { ...activityRow(groupedSummary), itemId: "wrong_group" },
      activityRow(
        { ...groupedSummary, occurredAt: observedAt },
      ),
      activityRow(groupedSummary, {
        kind: "activity",
        groupId: "wrong_group",
        expectedRevision: group.sourceRange.last,
      }),
    ]) {
      expect(() => parseOperationResult(
        FABRIC_OPERATIONS.projectionViewPage,
        activityPage([malformedRow]),
      )).toThrowError();
    }
  });

  it("retains a bounded evidence summary with an exact count and digest", () => {
    const links = Array.from({ length: 33 }, (_, index) => ({
      path: `evidence/${String(index).padStart(2, "0")}.json`,
      digest: `sha256:${index.toString(16).padStart(64, "0")}`,
    }));
    const evidenced = {
      ...group,
      evidenceLinkCount: links.length,
      evidenceLinksDigest:
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      evidenceLinksTruncated: true,
      evidenceLinks: links.slice(0, 1),
      members: group.members.map((member, index) => ({
        ...member,
        evidenceLinkCount: index === 0 ? links.length : 0,
        evidenceLinksDigest: index === 0
          ? "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
          : member.evidenceLinksDigest,
      })),
    };
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      activityPage([activityRow({ ...groupedSummary, group: evidenced })]),
    )).not.toThrow();
  });

  it("binds each member availability to its ordered detail", () => {
    const detailResult = {
      status: "current",
      detailRef: {
        kind: "activity",
        groupId: group.groupId,
        expectedRevision: group.sourceRange.last,
      },
      detail: {
        freshness: "live",
        source: "fabric",
        revision: group.sourceRange.last,
        observedAt: laterAt,
        value: {
          kind: "activity",
          group,
          memberDetails: group.members.map((member, index) => index === 0
            ? {
                eventId: member.eventId,
                status: "unavailable",
                reason: "unsafe-content",
              }
            : {
                eventId: member.eventId,
                status: "available",
                content: "{}",
                transformation: "none",
              }),
        },
      },
      snapshotRevision: 11,
      readTransactionId: "read_activity_detail",
    };
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionDetailRead,
      detailResult,
    )).toThrowError();
  });

  it("binds referenced details to their member and accepts bounded content pages", () => {
    const content = "x".repeat(300_000);
    const firstPage = content.slice(0, 256 * 1024);
    const contentDigest =
      `sha256:${createHash("sha256").update(content).digest("hex")}`;
    const referenced = {
      eventId: group.members[0].eventId,
      status: "referenced",
      contentBytes: Buffer.byteLength(content),
      contentDigest,
      detailRef: {
        kind: "activity",
        groupId: group.groupId,
        eventId: group.members[0].eventId,
        expectedRevision: group.members[0].sourceRevision,
        contentOffset: 0,
      },
    };
    const groupedDetail = {
      status: "current",
      detailRef: {
        kind: "activity",
        groupId: group.groupId,
        expectedRevision: group.sourceRange.last,
      },
      detail: {
        freshness: "live",
        source: "fabric",
        revision: group.sourceRange.last,
        observedAt: laterAt,
        value: {
          kind: "activity",
          group,
          memberDetails: [
            referenced,
            {
              eventId: group.members[1].eventId,
              status: "available",
              content: "{}",
              transformation: "none",
            },
            {
              eventId: group.members[2].eventId,
              status: "available",
              content: "{}",
              transformation: "none",
            },
          ],
        },
      },
      snapshotRevision: 11,
      readTransactionId: "read_group_reference",
    };
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionDetailRead,
      groupedDetail,
    )).not.toThrow();
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionDetailRead,
      {
        ...groupedDetail,
        detail: {
          ...groupedDetail.detail,
          value: {
            ...groupedDetail.detail.value,
            memberDetails: [
              {
                ...referenced,
                detailRef: { ...referenced.detailRef, eventId: "event_crossed" },
              },
              ...groupedDetail.detail.value.memberDetails.slice(1),
            ],
          },
        },
      },
    )).toThrowError();

    const contentPage = {
      status: "current",
      detailRef: referenced.detailRef,
      detail: {
        freshness: "live",
        source: "fabric",
        revision: group.members[0].sourceRevision,
        observedAt,
        value: {
          kind: "activity",
          groupId: group.groupId,
          eventId: group.members[0].eventId,
          sourceRevision: group.members[0].sourceRevision,
          contentOffset: 0,
          content: firstPage,
          contentBytes: Buffer.byteLength(content),
          contentDigest,
          transformation: "none",
          nextDetailRef: {
            ...referenced.detailRef,
            contentOffset: Buffer.byteLength(firstPage),
          },
        },
      },
      snapshotRevision: 11,
      readTransactionId: "read_member_page",
    };
    expect(Buffer.byteLength(firstPage)).toBeGreaterThan(4_096);
    const parsedContentPage = parseOperationResult(
      FABRIC_OPERATIONS.projectionDetailRead,
      contentPage,
    );
    expect(assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionDetailRead,
      ["operator-projection.v2", ACTIVITY_NARRATIVE_GROUPING_FEATURE],
      parsedContentPage,
    )).toBe(parsedContentPage);
    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionDetailRead,
      ["operator-projection.v2"],
      parsedContentPage,
    )).toThrow(expect.objectContaining({
      code: "PROTOCOL_INCOMPATIBLE",
      reason: "unnegotiated-field",
    }));
    const boundaryPageContent = "x".repeat(256 * 1024 - 3);
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionDetailRead,
      {
        ...contentPage,
        detail: {
          ...contentPage.detail,
          value: {
            ...contentPage.detail.value,
            content: boundaryPageContent,
            nextDetailRef: {
              ...referenced.detailRef,
              contentOffset: Buffer.byteLength(boundaryPageContent),
            },
          },
        },
      },
    )).not.toThrow();
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionDetailRead,
      {
        ...contentPage,
        detail: {
          ...contentPage.detail,
          value: {
            ...contentPage.detail.value,
            content: "",
            nextDetailRef: referenced.detailRef,
          },
        },
      },
    )).toThrowError();
    expect(Buffer.byteLength(JSON.stringify({
      jsonrpc: "2.0",
      id: "activity-member-page",
      result: contentPage,
    }))).toBeLessThan(PROTOCOL_LIMITS.maximumFrameBytes);
  });

  it("keeps a dense evidence page envelope below the fixed frame", () => {
    const rows = Array.from({ length: 32 }, (_, index) => {
      const suffix = String(index).padStart(3, "0");
      const pathPrefix = `evidence/${suffix}/`;
      const evidence = {
        path: `${pathPrefix}${"a".repeat(4_096 - pathPrefix.length)}`,
        digest:
          "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      };
      const member = {
        ...group.members[1],
        ordinal: 1,
        eventId: `event_${suffix}`,
        actorId: `agent_${suffix}`,
        target: { kind: "task", id: `task_${suffix}` },
        occurredAt: laterAt,
        sourceRevision: index + 1,
        evidenceLinkCount: 1,
        evidenceLinksDigest: evidence.digest,
      };
      const boundedGroup = {
        ...group,
        groupId: `activity_group_${suffix}`,
        ordinal: index + 1,
        actorIds: [member.actorId],
        target: member.target,
        eventKinds: [member.eventKind],
        occurredAtRange: { first: laterAt, last: laterAt },
        sourceRange: { first: index + 1, last: index + 1 },
        count: 1,
        evidenceLinkCount: 1,
        evidenceLinksDigest: evidence.digest,
        evidenceLinksTruncated: false,
        evidenceLinks: [evidence],
        members: [member],
      };
      const row = activityRow(
        { ...groupedSummary, group: boundedGroup },
        {
          kind: "activity",
          groupId: boundedGroup.groupId,
          expectedRevision: index + 1,
        },
      );
      return {
        ...row,
        itemId: boundedGroup.groupId,
        itemRevision: index + 1,
        fact: { ...row.fact, revision: index + 1 },
      };
    });
    const page = activityPage(rows);
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      page,
    )).not.toThrow();
    expect(Buffer.byteLength(JSON.stringify({
      jsonrpc: "2.0",
      id: "maximum-activity-page",
      result: page,
    }))).toBeLessThan(PROTOCOL_LIMITS.maximumFrameBytes);
  });
});
