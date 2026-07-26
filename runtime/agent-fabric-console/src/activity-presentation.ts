import type { ConsoleRow } from "./model.js";

type ActivitySummary = Extract<
  NonNullable<ConsoleRow<"activity">["summary"]>,
  { kind: "activity" }
>;

function groupFor(summary: ActivitySummary) {
  if (!("group" in summary)) {
    throw new TypeError("exact activity row has no narrative group");
  }
  return summary.group;
}

export function activityGroupSecondary(summary: ActivitySummary): string {
  const group = groupFor(summary);
  const target = group.target === null
    ? "no exact target"
    : `${group.target.kind}:${group.target.id}`;
  return `${group.kind} | x${String(group.count)} | source ${
    String(group.sourceRange.first)
  }-${String(group.sourceRange.last)} | ${target}`;
}

export function activityGroupDetailLines(
  summary: ActivitySummary,
): readonly Readonly<{ label: string; value: string }>[] {
  const group = groupFor(summary);
  const unavailableEvidence = group.members.some((member) =>
    member.evidenceLinkCount > 0 &&
    member.detailAvailability === "unavailable"
  );
  return [
    { label: "Activity group", value: `${group.kind} | ordinal ${String(group.ordinal)}` },
    {
      label: "Group target",
      value: group.target === null
        ? "unavailable | no exact target"
        : `${group.target.kind}:${group.target.id}`,
    },
    {
      label: "Group actors",
      value: group.actorIds.join(", ") || "unavailable | no exact actor",
    },
    { label: "Group count", value: String(group.count) },
    {
      label: "Group source range",
      value: `${String(group.sourceRange.first)}-${String(group.sourceRange.last)} inclusive`,
    },
    {
      label: "Group time range",
      value: `${group.occurredAtRange.first} - ${group.occurredAtRange.last}`,
    },
    { label: "Group event kinds", value: group.eventKinds.join(", ") },
    {
      label: "Group evidence",
      value: group.evidenceLinkCount === 0
        ? "none"
        : `${
          group.evidenceLinks.map(({ path, digest }) => `${path}@${digest}`).join(", ")
        } | ${String(group.evidenceLinkCount)} total${
          group.evidenceLinksTruncated
            ? unavailableEvidence
              ? " | some full links unavailable | evidence-bearing member detail unavailable"
              : " | full links in member detail"
            : ""
        }`,
    },
    ...group.members.flatMap((member) => [
      {
        label: `Member ${String(member.ordinal)}`,
        value: `${member.eventId} | ${member.eventKind} | source ${String(member.sourceRevision)} | ${member.occurredAt}`,
      },
      {
        label: `Member ${String(member.ordinal)} detail`,
        value: `${member.detailAvailability} | ${String(member.evidenceLinkCount)} evidence link(s)`,
      },
      ...(member.messageBodyRef === undefined
        ? []
        : [{
            label: `Member ${String(member.ordinal)} message`,
            value: `${member.messageBodyRef.messageId}@r${String(member.messageBodyRef.expectedRevision)}`,
          }]),
    ]),
  ];
}
