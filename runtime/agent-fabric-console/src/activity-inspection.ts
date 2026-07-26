import { createHash } from "node:crypto";

import type {
  ActivityNarrativeMemberDetail,
  ProjectionSnapshotRequest,
} from "@local/agent-fabric-protocol";

import { revisionToProtocol, type Revision } from "./model.js";
import { readConsoleMessageBody } from "./message.js";
import type {
  ConsoleInspectionBinding,
  ConsoleProtocolPort,
  ConsoleReadInspection,
  FabricConsoleDataset,
} from "./protocol-adapter.js";

function unavailableActivity(
  binding: ConsoleInspectionBinding,
  reason: Extract<
    ConsoleReadInspection,
    { kind: "activity"; state: "unavailable" }
  >["reason"],
): ConsoleReadInspection {
  return { kind: "activity", state: "unavailable", binding, reason };
}

function failureCode(error: unknown): string | null {
  if (typeof error !== "object" || error === null) return null;
  const code = Reflect.get(error, "code");
  return typeof code === "string" ? code : null;
}

class ReferencedActivityDetailError extends Error {
  readonly reason: string;

  constructor(reason: string) {
    super(reason);
    this.reason = reason;
  }
}

async function readReferencedActivityDetail(input: Readonly<{
  detail: Extract<ActivityNarrativeMemberDetail, { status: "referenced" }>;
  port: ConsoleProtocolPort;
  readScope: ProjectionSnapshotRequest;
  snapshotRevision: Revision;
}>): Promise<ActivityNarrativeMemberDetail> {
  const { detail, port, readScope, snapshotRevision } = input;
  let detailRef = detail.detailRef;
  let content = "";
  let transformation:
    | Extract<ActivityNarrativeMemberDetail, { status: "available" }>["transformation"]
    | null = null;
  const maximumPages = Math.max(
    1,
    Math.ceil(detail.contentBytes / (256 * 1024 - 3)),
  );
  let pagesRead = 0;
  for (;;) {
    const result = await port.readDetail({
      ...readScope,
      snapshotRevision: revisionToProtocol(snapshotRevision),
      detailRef,
    });
    if (result.status !== "current") {
      throw new ReferencedActivityDetailError("projection-changed");
    }
    if (result.detail.freshness === "unavailable") {
      throw new ReferencedActivityDetailError("detail-unavailable");
    }
    if (result.detail.freshness === "conflict") {
      throw new ReferencedActivityDetailError("detail-conflict");
    }
    if (
      result.snapshotRevision !== revisionToProtocol(snapshotRevision) ||
      result.detailRef.kind !== "activity" ||
      !("groupId" in result.detailRef) ||
      !("eventId" in result.detailRef) ||
      result.detailRef.groupId !== detailRef.groupId ||
      result.detailRef.eventId !== detailRef.eventId ||
      result.detailRef.contentOffset !== detailRef.contentOffset ||
      result.detailRef.expectedRevision !== detailRef.expectedRevision ||
      result.detail.revision !== detailRef.expectedRevision
    ) {
      throw new TypeError("activity member detail contract is invalid");
    }
    const page = result.detail.value;
    if (
      page.kind !== "activity" ||
      !("groupId" in page) ||
      !("eventId" in page) ||
      !("contentOffset" in page) ||
      page.groupId !== detailRef.groupId ||
      page.eventId !== detailRef.eventId ||
      page.contentOffset !== detailRef.contentOffset ||
      page.sourceRevision !== detailRef.expectedRevision ||
      page.contentBytes !== detail.contentBytes ||
      page.contentDigest !== detail.contentDigest ||
      ("group" in page)
    ) {
      throw new TypeError("activity member detail page is invalid");
    }
    pagesRead += 1;
    const pageBytes = Buffer.byteLength(page.content);
    if (
      pagesRead > maximumPages ||
      Buffer.byteLength(content) + pageBytes > detail.contentBytes ||
      (
        page.nextDetailRef !== null &&
        (pageBytes < 256 * 1024 - 3 || pageBytes > 256 * 1024)
      )
    ) {
      throw new TypeError("activity member detail page bound is invalid");
    }
    if (transformation !== null && transformation !== page.transformation) {
      throw new TypeError("activity member detail transformation changed");
    }
    transformation = page.transformation;
    content += page.content;
    if (page.nextDetailRef === null) break;
    if (
      page.nextDetailRef.groupId !== detailRef.groupId ||
      page.nextDetailRef.eventId !== detailRef.eventId ||
      page.nextDetailRef.expectedRevision !== detailRef.expectedRevision ||
      page.nextDetailRef.contentOffset !==
        detailRef.contentOffset + Buffer.byteLength(page.content)
    ) {
      throw new TypeError("activity member detail continuation is invalid");
    }
    detailRef = page.nextDetailRef;
  }
  if (Buffer.byteLength(content) !== detail.contentBytes) {
    throw new TypeError("activity member detail byte count is invalid");
  }
  const contentDigest = `sha256:${createHash("sha256")
    .update(content)
    .digest("hex")}`;
  if (contentDigest !== detail.contentDigest) {
    throw new TypeError("activity member detail digest is invalid");
  }
  return {
    eventId: detail.eventId,
    status: "available",
    content,
    transformation: transformation ?? "none",
  };
}

export async function inspectNarrativeActivity(input: Readonly<{
  binding: ConsoleInspectionBinding;
  dataset: FabricConsoleDataset | null;
  port: ConsoleProtocolPort | null;
  readScope: ProjectionSnapshotRequest;
}>): Promise<ConsoleReadInspection> {
  const { binding, dataset, port, readScope } = input;
  const row = dataset?.pages.activity.rows.find(
    (candidate) => candidate.stableId === binding.itemId,
  );
  if (
    dataset === null ||
    dataset.snapshotRevision !== binding.projectionRevision ||
    row?.revision !== binding.itemRevision ||
    row.summary?.kind !== "activity" ||
    !("group" in row.summary) ||
    row.detailRef?.kind !== "activity" ||
    !("groupId" in row.detailRef)
  ) {
    return unavailableActivity(binding, "projection-changed");
  }
  if (port === null) return unavailableActivity(binding, "feature-unavailable");
  try {
    const detail = await port.readDetail({
      ...readScope,
      snapshotRevision: revisionToProtocol(binding.projectionRevision),
      detailRef: row.detailRef,
    });
    if (detail.status === "resnapshot-required") {
      return unavailableActivity(binding, "projection-changed");
    }
    if (
      detail.snapshotRevision !== revisionToProtocol(binding.projectionRevision) ||
      detail.detailRef.kind !== "activity" ||
      !("groupId" in detail.detailRef) ||
      detail.detailRef.groupId !== row.detailRef.groupId ||
      detail.detailRef.expectedRevision !== row.detailRef.expectedRevision ||
      detail.detail.revision !== row.detailRef.expectedRevision
    ) {
      return unavailableActivity(binding, "contract-invalid");
    }
    if (detail.detail.freshness === "unavailable") {
      return unavailableActivity(binding, "detail-unavailable");
    }
    if (detail.detail.freshness === "conflict") {
      return unavailableActivity(binding, "detail-conflict");
    }
    const activity = detail.detail.value;
    if (
      activity.kind !== "activity" ||
      !("group" in activity) ||
      activity.group.groupId !== row.summary.group.groupId ||
      JSON.stringify(activity.group) !== JSON.stringify(row.summary.group)
    ) {
      return unavailableActivity(binding, "detail-invalid");
    }
    const resolvedMemberDetails: ActivityNarrativeMemberDetail[] = [];
    for (const memberDetail of activity.memberDetails) {
      if (memberDetail.status !== "referenced") {
        resolvedMemberDetails.push(memberDetail);
        continue;
      }
      try {
        resolvedMemberDetails.push(await readReferencedActivityDetail({
          detail: memberDetail,
          port,
          readScope,
          snapshotRevision: binding.projectionRevision,
        }));
      } catch (error: unknown) {
        const code = failureCode(error);
        resolvedMemberDetails.push({
          eventId: memberDetail.eventId,
          status: "unavailable",
          reason: error instanceof ReferencedActivityDetailError
            ? error.reason
            : code === "STALE_REVISION" ||
                code === "PROJECTION_RESNAPSHOT_REQUIRED"
              ? "projection-changed"
              : error instanceof TypeError
                ? "contract-invalid"
                : "transport-failure",
        });
      }
    }
    const messages: Extract<
      ConsoleReadInspection,
      { kind: "activity"; state: "current" }
    >["messages"][number][] = [];
    for (const member of activity.group.members) {
      const reference = member.messageBodyRef;
      if (reference === undefined) continue;
      if (port.readMessageBody === null) {
        messages.push({
          eventId: member.eventId,
          state: "unavailable",
          reason: "feature-unavailable",
        });
        continue;
      }
      try {
        const result = await readConsoleMessageBody(
          { read: port.readMessageBody },
          { credential: readScope.credential, ...reference },
        );
        if (result.available) {
          messages.push({ eventId: member.eventId, state: "current", result });
        } else {
          const reason = {
            "not-found": "message-not-found",
            forbidden: "message-forbidden",
            expired: "message-expired",
          } as const;
          messages.push({
            eventId: member.eventId,
            state: "unavailable",
            reason: reason[result.reason],
          });
        }
      } catch (error: unknown) {
        const code = failureCode(error);
        messages.push({
          eventId: member.eventId,
          state: "unavailable",
          reason: code === "STALE_REVISION" ||
              code === "PROJECTION_RESNAPSHOT_REQUIRED"
            ? "projection-changed"
            : error instanceof Error &&
                error.message.startsWith("message body contract")
              ? "contract-invalid"
              : "transport-failure",
        });
      }
    }
    return {
      kind: "activity",
      state: "current",
      binding,
      readTransactionId: detail.readTransactionId,
      detail: {
        ...activity,
        memberDetails: resolvedMemberDetails as [
          ActivityNarrativeMemberDetail,
          ...ActivityNarrativeMemberDetail[],
        ],
      },
      messages,
    };
  } catch (error: unknown) {
    const code = failureCode(error);
    return unavailableActivity(
      binding,
      code === "STALE_REVISION" ||
          code === "PROJECTION_RESNAPSHOT_REQUIRED"
        ? "projection-changed"
        : "transport-failure",
    );
  }
}
