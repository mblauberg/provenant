import { readFileSync } from "node:fs";

import { describe, expect, it } from "vitest";

import {
  projectActivityNarrativeGroups,
  type ActivityNarrativeEventInput,
} from "../../src/operator/activity-grouping.js";

type Fixture = {
  runId: string;
  events: ActivityNarrativeEventInput[];
  expectedTargetOrder: string[];
  expectedSourceRanges: [number, number][];
};

const fixture = JSON.parse(
  readFileSync(
    new URL("../fixtures/activity-grouping-determinism.json", import.meta.url),
    "utf8",
  ),
) as Fixture;

function targetKey(
  target: ReturnType<typeof projectActivityNarrativeGroups>[number]["group"]["target"],
  groupId: string,
): string {
  return target === null ? `event:${groupId}` : `${target.kind}:${target.id}`;
}

describe("daemon activity narrative grouping", () => {
  it("is byte-identical for fixed input independent of input and map iteration order", () => {
    const projected = projectActivityNarrativeGroups(
      fixture.runId,
      fixture.events,
    );
    const reversed = projectActivityNarrativeGroups(
      fixture.runId,
      [...fixture.events].reverse(),
    );

    expect(JSON.stringify(projected)).toBe(JSON.stringify(reversed));
    expect(
      projected.map(({ group }) =>
        group.target === null
          ? `event:${group.members[0]?.eventId ?? group.groupId}`
          : targetKey(group.target, group.groupId)
      ),
    ).toStrictEqual(fixture.expectedTargetOrder);
    expect(
      projected.map(({ group }) => [
        group.sourceRange.first,
        group.sourceRange.last,
      ]),
    ).toStrictEqual(fixture.expectedSourceRanges);
    expect(projected.map(({ group }) => group.ordinal)).toStrictEqual([1, 2, 3]);
  });

  it("retains raw event/message references and labelled event detail", () => {
    const [alpha] = projectActivityNarrativeGroups(
      fixture.runId,
      fixture.events,
    );
    expect(alpha?.group.members.map(({ eventId, sourceRevision }) => ({
      eventId,
      sourceRevision,
    }))).toStrictEqual([
      { eventId: "event_alpha_message", sourceRevision: 1 },
      { eventId: "event_alpha_tool", sourceRevision: 4 },
    ]);
    expect(alpha?.group.members[0]?.messageBodyRef).toMatchObject({
      messageId: "message_alpha",
    });
    expect(alpha?.memberDetails).toStrictEqual([
      expect.objectContaining({
        eventId: "event_alpha_message",
        status: "referenced",
      }),
      expect.objectContaining({
        eventId: "event_alpha_tool",
        status: "referenced",
      }),
    ]);
    expect(alpha?.group.evidenceLinks.map(({ path }) => path)).toStrictEqual([
      "z.txt",
    ]);
    expect(alpha?.group).toMatchObject({
      evidenceLinkCount: 2,
      evidenceLinksTruncated: true,
    });
  });

  it("preserves controls for the Console's visible inert text-safety path", () => {
    const [projected] = projectActivityNarrativeGroups("run_controls", [{
      eventId: "event_controls",
      eventKind: "tool-result-recorded",
      actorId: null,
      payloadJson: JSON.stringify({
        taskId: "task_controls",
        detail: "left\u202eright",
      }),
      occurredAt: "2026-07-26T00:00:00.000Z" as never,
      sourceRevision: 1,
      messageBodyRef: null,
      messageTarget: null,
    }]);
    const [detail] = projected?.memberContents ?? [];
    expect(detail).toMatchObject({
      status: "available",
      transformation: "none",
    });
    expect(detail?.status === "available" ? detail.content : "").toContain(
      "\u202e",
    );
  });

  it("deterministically chunks a related chain at the closed codec bound", () => {
    const events = Array.from({ length: 257 }, (_, index) => ({
      eventId: `event_${String(index).padStart(3, "0")}`,
      eventKind: "tool-invoked",
      actorId: "agent_chunk",
      payloadJson: '{"taskId":"task_chunk"}',
      occurredAt: "2026-07-26T00:00:00.000Z" as never,
      sourceRevision: index + 1,
      messageBodyRef: null,
      messageTarget: null,
    }));
    const projected = projectActivityNarrativeGroups("run_chunk", events);
    expect(projected.map(({ group }) => group.count)).toStrictEqual([
      1,
      32,
      32,
      32,
      32,
      32,
      32,
      32,
      32,
    ]);
    expect(new Set(projected.map(({ group }) => group.groupId)).size).toBe(9);
  });

  it("labels unavailable event detail consistently in member and detail facts", () => {
    const [projected] = projectActivityNarrativeGroups("run_unsafe", [{
      eventId: "event_unsafe",
      eventKind: "tool-result-recorded",
      actorId: null,
      payloadJson: JSON.stringify({
        taskId: "task_unsafe",
        detail: "-----BEGIN PRIVATE KEY-----\nunclosed",
      }),
      occurredAt: "2026-07-26T00:00:00.000Z" as never,
      sourceRevision: 1,
      messageBodyRef: null,
      messageTarget: null,
    }]);
    expect(projected?.group.members[0]?.detailAvailability).toBe("unavailable");
    expect(projected?.memberDetails[0]).toMatchObject({
      eventId: "event_unsafe",
      status: "unavailable",
      reason: "unsafe-content",
    });
  });
});
