import { createHash } from "node:crypto";

import type {
  ActivityNarrativeTarget,
  ConsoleView,
  MessageBodyRef,
  OperatorActionAvailability,
  OperatorDetail,
  OperatorDetailRef,
  OperatorViewRow,
  ProjectionFact,
  ProjectionViewItemMap,
  Timestamp,
} from "@local/agent-fabric-protocol";
import {
  PROTOCOL_LIMITS,
  parseIdentifier,
  parseTimestamp,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import {
  integer,
  isRow,
  nullableText,
  text,
  type Row,
} from "../project-session/store-support.js";
import {
  compareActivityIdentity,
  projectActivityNarrativeGroups,
  type ActivityNarrativeEventInput,
  type ProjectedActivityNarrativeGroup,
} from "./activity-grouping.js";

export type ActivityNarrativeGroupingProjection = "include" | "omit";

export type LoadedActivityDetail = Readonly<{
  revision: number;
  observedAt: Timestamp;
  detail: OperatorDetail;
}>;

const ACTIVITY_MEMBER_CONTENT_PAGE_BYTES = 256 * 1024;

export function boundedActivityPagePrefix<T>(
  requested: readonly T[],
  resultFor: (values: readonly T[]) => unknown,
): readonly T[] {
  const maximumBytes = PROTOCOL_LIMITS.maximumFrameBytes - 65_536;
  const selected: T[] = [];
  for (const value of requested) {
    const candidate = [...selected, value];
    const frame = {
      jsonrpc: "2.0",
      id: "activity-page-byte-budget".padEnd(128, "x"),
      result: resultFor(candidate),
    };
    if (
      Buffer.byteLength(JSON.stringify(frame)) > maximumBytes ||
      jsonNodeCount(frame) > 3_500
    ) break;
    selected.push(value);
  }
  if (selected.length === 0 && requested.length > 0) {
    throw new TypeError("one activity group exceeds the protocol frame budget");
  }
  return selected;
}

export function boundedOperatorActivityRows<T>(
  requested: readonly T[],
  input: Readonly<{
    view: ConsoleView;
    cursor: number;
    totalRows: number;
    snapshotRevision: number;
    readTransactionId: string;
  }>,
): readonly T[] {
  return boundedActivityPagePrefix(requested, (rows) => ({
    status: "page",
    view: input.view,
    rows,
    nextCursor: input.cursor + rows.length,
    hasMore: input.cursor + rows.length < input.totalRows,
    snapshotRevision: input.snapshotRevision,
    readTransactionId: input.readTransactionId,
  }));
}

export function boundedProjectionActivityItems<T>(
  requested: readonly T[],
  input: Readonly<{
    view: ConsoleView;
    after: number;
    totalItems: number;
    revision: number;
    observedAt: Timestamp;
  }>,
): readonly T[] {
  return boundedActivityPagePrefix(requested, (items) => ({
    view: input.view,
    page: liveFact(input.revision, input.observedAt, {
      items,
      nextCursor: input.after + items.length,
      hasMore: input.after + items.length < input.totalItems,
    }),
  }));
}

function jsonNodeCount(value: unknown): number {
  let count = 0;
  const pending = [value];
  while (pending.length > 0) {
    const current = pending.pop();
    count += 1;
    if (Array.isArray(current)) {
      pending.push(...current);
    } else if (typeof current === "object" && current !== null) {
      pending.push(...Object.values(current));
    }
  }
  return count;
}

function jsonObject(serialized: string, label: string): Row {
  const parsed: unknown = JSON.parse(serialized);
  if (!isRow(parsed)) throw new Error(`${label} is not an object`);
  return parsed;
}

function toTimestamp(milliseconds: number, path: string): Timestamp {
  return parseTimestamp(new Date(milliseconds).toISOString(), path);
}

function liveFact<T>(
  revision: number,
  observedAt: Timestamp,
  value: T,
): ProjectionFact<T> {
  return { freshness: "live", source: "fabric", revision, observedAt, value };
}

function activityKind(
  type: string,
): "message" | "decision" | "lifecycle" | "operation" {
  if (type.includes("message")) return "message";
  if (
    type.includes("decision") ||
    type.includes("gate") ||
    type.includes("approval")
  ) return "decision";
  if (
    type.includes("lifecycle") ||
    type.includes("launch") ||
    type.includes("quiesc") ||
    type.includes("closed") ||
    type.includes("cancel")
  ) return "lifecycle";
  return "operation";
}

function messageBodyRef(
  database: Database.Database,
  event: Row,
): MessageBodyRef {
  const payload = jsonObject(
    text(event, "payload_json"),
    "message activity payload",
  );
  if (typeof payload.messageId !== "string") {
    throw new Error("message activity has no exact message ID binding");
  }
  if (!isRow(database.prepare(`
    SELECT message_id FROM messages WHERE run_id=? AND message_id=?
  `).get(text(event, "run_id"), payload.messageId))) {
    throw new Error("message activity references a message outside its run");
  }
  return {
    projectSessionId: parseIdentifier<"ProjectSessionId">(
      text(event, "project_session_id"),
      "messageBodyRef.projectSessionId",
    ),
    messageId: parseIdentifier<"MessageId">(
      payload.messageId,
      "messageBodyRef.messageId",
    ),
    expectedRevision: 1,
  };
}

function messageTarget(
  database: Database.Database,
  event: Row,
): ActivityNarrativeTarget | null {
  const payload = jsonObject(
    text(event, "payload_json"),
    "message activity payload",
  );
  if (typeof payload.messageId !== "string") return null;
  const stored = database.prepare(`
    SELECT audience_json FROM messages WHERE run_id=? AND message_id=?
  `).get(text(event, "run_id"), payload.messageId);
  if (!isRow(stored)) return null;
  const audience = jsonObject(
    text(stored, "audience_json"),
    "message activity audience",
  );
  if (audience.kind === "task" && typeof audience.taskId === "string") {
    return { kind: "task", id: audience.taskId };
  }
  if (
    audience.kind === "agents" &&
    Array.isArray(audience.agentIds) &&
    audience.agentIds.length === 1 &&
    typeof audience.agentIds[0] === "string"
  ) {
    return { kind: "agent", id: audience.agentIds[0] };
  }
  return { kind: "message", id: payload.messageId };
}

function activityNarrativeGroups(
  database: Database.Database,
  values: readonly Row[],
): readonly ProjectedActivityNarrativeGroup[] {
  const byRun = new Map<string, ActivityNarrativeEventInput[]>();
  for (const event of values) {
    const runId = text(event, "run_id");
    const kind = activityKind(text(event, "type"));
    const input: ActivityNarrativeEventInput = {
      eventId: text(event, "event_id"),
      eventKind: text(event, "type"),
      actorId: nullableText(event, "actor_agent_id"),
      payloadJson: text(event, "payload_json"),
      occurredAt: toTimestamp(
        integer(event, "created_at"),
        "activityGroup.occurredAt",
      ),
      sourceRevision: integer(event, "sequence"),
      messageBodyRef: kind === "message"
        ? messageBodyRef(database, event)
        : null,
      messageTarget: kind === "message"
        ? messageTarget(database, event)
        : null,
    };
    const run = byRun.get(runId);
    if (run === undefined) byRun.set(runId, [input]);
    else run.push(input);
  }
  return [...byRun.entries()]
    .flatMap(([runId, inputs]) =>
      projectActivityNarrativeGroups(runId, inputs)
    )
    .sort((left, right) =>
      right.group.sourceRange.last - left.group.sourceRange.last ||
      compareActivityIdentity(left.group.groupId, right.group.groupId)
    )
    .map((value, index) => ({
      ...value,
      group: { ...value.group, ordinal: index + 1 },
    }));
}

export function activityItems(
  database: Database.Database,
  values: readonly Row[],
  grouping: ActivityNarrativeGroupingProjection,
): ProjectionViewItemMap["activity"][] {
  if (grouping === "include") {
    return activityNarrativeGroups(database, values).map(({ group }) => {
      const member = group.members[0];
      if (member === undefined) throw new Error("activity group has no member");
      const kind = activityKind(member.eventKind);
      const base = {
        eventId: member.eventId,
        actorId: member.actorId,
        taskId: member.target?.kind === "task"
          ? parseIdentifier<"TaskId">(member.target.id, "activityPage.taskId")
          : null,
        summary: `${group.kind} activity`,
        occurredAt: member.occurredAt,
        sourceRevision: member.sourceRevision,
        group,
      };
      return kind === "message" && member.messageBodyRef !== undefined
        ? { ...base, kind, messageBodyRef: member.messageBodyRef }
        : { ...base, kind: kind === "message" ? "operation" as const : kind };
    });
  }
  return values.map((event): ProjectionViewItemMap["activity"] => {
    const payload = jsonObject(
      text(event, "payload_json"),
      "activity page payload",
    );
    const taskId = typeof payload.taskId === "string"
      ? parseIdentifier<"TaskId">(payload.taskId, "activityPage.taskId")
      : null;
    const occurredAt = toTimestamp(
      integer(event, "created_at"),
      "activityPage.occurredAt",
    );
    const kind = activityKind(text(event, "type"));
    const base = {
      eventId: text(event, "event_id"),
      actorId: nullableText(event, "actor_agent_id"),
      taskId,
      summary: text(event, "type"),
      occurredAt,
      sourceRevision: integer(event, "sequence"),
    };
    return kind === "message"
      ? { ...base, kind, messageBodyRef: messageBodyRef(database, event) }
      : { ...base, kind };
  });
}

export function activityRows(
  database: Database.Database,
  values: readonly Row[],
  availability: OperatorActionAvailability,
  grouping: ActivityNarrativeGroupingProjection,
): OperatorViewRow<"activity">[] {
  if (grouping === "include") {
    return activityNarrativeGroups(database, values).map(({ group }) => ({
      itemId: group.groupId,
      itemRevision: group.sourceRange.last,
      fact: liveFact(group.sourceRange.last, group.occurredAtRange.last, {
        summary: {
          kind: "activity",
          summary: `${group.kind} activity`,
          occurredAt: group.occurredAtRange.last,
          group,
        },
        detailRef: {
          kind: "activity",
          groupId: group.groupId,
          expectedRevision: group.sourceRange.last,
        },
        actionAvailability: availability,
      }),
    }));
  }
  return values.map((event): OperatorViewRow<"activity"> => {
    const sequence = integer(event, "sequence");
    const eventId = text(event, "event_id");
    const kind = activityKind(text(event, "type"));
    const occurredAt = toTimestamp(
      integer(event, "created_at"),
      "activityRow.occurredAt",
    );
    const summary = kind === "message"
      ? {
          kind: "activity" as const,
          activityKind: "message" as const,
          summary: text(event, "type"),
          occurredAt,
          messageBodyRef: messageBodyRef(database, event),
        }
      : {
          kind: "activity" as const,
          activityKind: kind,
          summary: text(event, "type"),
          occurredAt,
        };
    return {
      itemId: eventId,
      itemRevision: sequence,
      fact: liveFact(sequence, occurredAt, {
        summary,
        detailRef: { kind: "activity", eventId, expectedRevision: sequence },
        actionAvailability: availability,
      }),
    };
  });
}

export function loadActivityDetail(
  database: Database.Database,
  values: readonly Row[],
  detailRef: Extract<OperatorDetailRef, { kind: "activity" }>,
  grouping: ActivityNarrativeGroupingProjection,
): LoadedActivityDetail {
  if (
    "groupId" in detailRef &&
    "eventId" in detailRef &&
    "contentOffset" in detailRef
  ) {
    if (grouping !== "include") {
      throw new ProjectFabricCoreError(
        "FEATURE_UNAVAILABLE",
        "activity narrative grouping was not negotiated",
      );
    }
    const projected = activityNarrativeGroups(database, values).find(
      ({ group }) => group.groupId === detailRef.groupId,
    );
    const memberIndex = projected?.group.members.findIndex(
      (member) => member.eventId === detailRef.eventId,
    ) ?? -1;
    const member = projected?.group.members[memberIndex];
    const stored = projected?.memberContents[memberIndex];
    if (
      projected === undefined ||
      member === undefined ||
      stored === undefined ||
      stored.status !== "available"
    ) {
      throw new ProjectFabricCoreError(
        "NOT_FOUND",
        "activity member detail is not available",
      );
    }
    const encoded = Buffer.from(stored.content);
    const offset = detailRef.contentOffset;
    if (
      offset < 0 ||
      offset > encoded.length ||
      (offset === encoded.length && encoded.length !== 0) ||
      (
        offset > 0 &&
        offset < encoded.length &&
        ((encoded[offset] ?? 0) & 0xc0) === 0x80
      )
    ) {
      throw new ProjectFabricCoreError(
        "PROTOCOL_INVALID",
        "activity member detail offset is outside the content",
      );
    }
    let end = Math.min(
      encoded.length,
      offset + ACTIVITY_MEMBER_CONTENT_PAGE_BYTES,
    );
    while (
      end < encoded.length &&
      end > offset &&
      ((encoded[end] ?? 0) & 0xc0) === 0x80
    ) {
      end -= 1;
    }
    const nextDetailRef = end < encoded.length
      ? {
          kind: "activity" as const,
          groupId: projected.group.groupId,
          eventId: member.eventId,
          expectedRevision: member.sourceRevision,
          contentOffset: end,
        }
      : null;
    return {
      revision: member.sourceRevision,
      observedAt: member.occurredAt,
      detail: {
        kind: "activity",
        groupId: projected.group.groupId,
        eventId: member.eventId,
        sourceRevision: member.sourceRevision,
        contentOffset: offset,
        content: encoded.subarray(offset, end).toString("utf8"),
        contentBytes: encoded.length,
        contentDigest: `sha256:${createHash("sha256")
          .update(stored.content)
          .digest("hex")}` as never,
        transformation: stored.transformation,
        nextDetailRef,
      },
    };
  }
  if ("groupId" in detailRef) {
    if (grouping !== "include") {
      throw new ProjectFabricCoreError(
        "FEATURE_UNAVAILABLE",
        "activity narrative grouping was not negotiated",
      );
    }
    const projected = activityNarrativeGroups(database, values).find(
      ({ group }) => group.groupId === detailRef.groupId,
    );
    if (projected === undefined) {
      throw new ProjectFabricCoreError(
        "NOT_FOUND",
        "activity group is not available",
      );
    }
    return {
      revision: projected.group.sourceRange.last,
      observedAt: projected.group.occurredAtRange.last,
      detail: {
        kind: "activity",
        group: projected.group,
        memberDetails: projected.memberDetails as [
          (typeof projected.memberDetails)[number],
          ...(typeof projected.memberDetails)[number][],
        ],
      },
    };
  }
  if (grouping === "include") {
    throw new ProjectFabricCoreError(
      "FEATURE_UNAVAILABLE",
      "negotiated activity detail requires a group reference",
    );
  }
  const event = values.find(
    (candidate) => text(candidate, "event_id") === detailRef.eventId,
  );
  if (event === undefined) throw new Error("activity detail not found");
  const occurredAt = toTimestamp(
    integer(event, "created_at"),
    "activityDetail.occurredAt",
  );
  const kind = activityKind(text(event, "type"));
  const detail: OperatorDetail = kind === "message"
    ? {
        kind: "activity",
        eventId: detailRef.eventId,
        activityKind: "message",
        summary: text(event, "type"),
        occurredAt,
        messageBodyRef: messageBodyRef(database, event),
      }
    : {
        kind: "activity",
        eventId: detailRef.eventId,
        activityKind: kind,
        summary: text(event, "type"),
        occurredAt,
      };
  return {
    revision: integer(event, "sequence"),
    observedAt: occurredAt,
    detail,
  };
}
