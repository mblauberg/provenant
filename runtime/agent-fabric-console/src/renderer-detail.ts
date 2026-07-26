import type {
  GitPathPage,
  GitRepositoryProjection,
} from "@local/agent-fabric-protocol";
import {
  cellWidth,
  chromeText,
  graphemes,
  sanitizeDisplayText,
  writeFixedCells,
  type Rect,
} from "./layout.js";
import { presentMessageBodyWindow, presentSafeTextWindow } from "./message.js";
import {
  matchesArtifactConfirmation,
  type FabricConsolePresentation,
  type FabricConsoleUiState,
} from "./presenter.js";
import type { FabricConsoleDataset } from "./protocol-adapter.js";
import {
  presentedBinding,
  setFabricRow,
  type FabricHitRegion,
} from "./renderer-primitives.js";

export function renderFabricDetail(
  rows: string[],
  columns: number,
  presentation: FabricConsolePresentation,
  dataset: FabricConsoleDataset,
  ui: FabricConsoleUiState,
  geometryKey: string,
  hitRegions: FabricHitRegion[],
  bounds: Rect,
): void {
  const inspection = dataset.inspection;
  const height = bounds.y2 - bounds.y1 + 1;
  const selected = presentation.masterRows.find(
    (row) => row.stableId === presentation.detail?.stableId,
  );
  const detailId = selected === undefined
    ? null
    : `detail:${presentation.activeView}:${selected.stableId}`;
  let detailScrollMaximum = 0;
  let lines: readonly string[];
  if (
    inspection?.kind === "activity" &&
    inspection.binding.view === presentation.activeView &&
    inspection.binding.itemId === presentation.detail?.stableId
  ) {
    if (inspection.state === "current") {
      const group = inspection.detail.group;
      const detailByEvent = new Map(
        inspection.detail.memberDetails.map((detail) => [detail.eventId, detail]),
      );
      const messageByEvent = new Map(
        inspection.messages.map((message) => [message.eventId, message]),
      );
      const unavailableEvidence = group.members.some((member) => {
        if (member.evidenceLinkCount === 0) return false;
        return detailByEvent.get(member.eventId)?.status !== "available";
      });
      const detailText = [
        `Activity group: ${group.groupId} | ${group.kind} | ordinal ${String(group.ordinal)}`,
        `Count: ${String(group.count)} | source ${String(group.sourceRange.first)}-${String(group.sourceRange.last)} inclusive`,
        `Time: ${group.occurredAtRange.first} - ${group.occurredAtRange.last}`,
        `Target: ${group.target === null ? "unavailable | no exact target" : `${group.target.kind}:${group.target.id}`}`,
        `Actors: ${group.actorIds.join(", ") || "unavailable | no exact actor"}`,
        `Evidence: ${String(group.evidenceLinkCount)} total${
          group.evidenceLinksTruncated
            ? unavailableEvidence
              ? " | inline summary truncated | some full links unavailable | evidence-bearing member detail unavailable"
              : " | inline summary truncated | full links in member detail"
            : ""
        }`,
        ...group.members.flatMap((member) => {
          const detail = detailByEvent.get(member.eventId);
          const message = messageByEvent.get(member.eventId);
          return [
            `Member ${String(member.ordinal)}: ${member.eventId} | ${member.eventKind} | source ${String(member.sourceRevision)} | ${member.occurredAt}`,
            ...(member.messageBodyRef === undefined
              ? []
              : message === undefined || message.state === "unavailable"
                ? [`Message: unavailable | ${message?.reason ?? "not-returned"}`]
                : [
                    `Message: ${message.result.messageId} r${String(message.result.revision)} | ${message.result.body}`,
                  ]),
            detail === undefined || detail.status !== "available"
              ? `Detail: unavailable | ${
                detail === undefined
                  ? "not-returned"
                  : detail.status === "unavailable"
                    ? detail.reason
                    : "unresolved-reference"
              }`
              : `Detail: ${detail.content}`,
          ];
        }),
      ].join("\n");
      const window = presentSafeTextWindow(
        detailText,
        {
          columns: Math.max(1, bounds.x2 - bounds.x1),
          rows: Math.max(1, height),
          offset: Math.max(
            0,
            Math.trunc(ui.detailScrollOffsetByView[presentation.activeView] ?? 0),
          ),
        },
        { sanitizeDisplayText, graphemes, cellWidth },
      );
      detailScrollMaximum = Math.max(0, window.totalLines - 1);
      lines = window.lines;
    } else {
      lines = [
        `Activity group: ${inspection.binding.itemId}`,
        `Detail: unavailable | ${inspection.reason}`,
      ];
    }
  } else if (
    inspection?.kind === "message" &&
    inspection.binding.view === presentation.activeView &&
    inspection.binding.itemId === presentation.detail?.stableId
  ) {
    if (inspection.state === "current") {
      const window = presentMessageBodyWindow(
        inspection.result,
        {
          columns: Math.max(1, bounds.x2 - bounds.x1 - 5),
          rows: Math.max(1, height - 2),
          offset: Math.max(
            0,
            Math.trunc(ui.detailScrollOffsetByView[presentation.activeView] ?? 0),
          ),
        },
        { sanitizeDisplayText, graphemes, cellWidth },
      );
      detailScrollMaximum = Math.max(0, window.totalLines - 1);
      lines = [
        `Message: ${inspection.result.messageId} r${String(inspection.result.revision)}`,
        "Safety: terminal-neutralised | capability-values-redacted",
        ...window.lines.map((line) => `Body: ${line}`),
      ];
    } else {
      lines = [
        `Message: ${inspection.binding.itemId}`,
        `Read: unavailable | ${inspection.reason}`,
      ];
    }
  } else if (
    inspection?.kind === "artifact" &&
    inspection.binding.view === presentation.activeView &&
    inspection.binding.itemId === presentation.detail?.stableId
  ) {
    if (inspection.state === "current") {
      const result = inspection.result;
      const scope = [
        result.projectSessionId === null ? null : `session:${result.projectSessionId}`,
        result.coordinationRunId === null ? null : `run:${result.coordinationRunId}`,
        result.taskId === null ? null : `task:${result.taskId}`,
      ].filter((value): value is string => value !== null).join(" | ") || "project";
      const confirmed = matchesArtifactConfirmation(
        ui.artifactConfirmation,
        inspection.binding.itemId,
        result,
      );
      const review = result.reviewDisposition === "eligible"
        ? "COMPLETE | Accept/Implement eligible"
        : confirmed
          ? `CONFIRMED ${result.transformation} + ${result.artifactRef.digest}`
        : result.reviewDisposition === "confirm-terminal-neutralised"
          ? `CONFIRM ${result.transformation} + source digest before Accept/Implement`
          : "BLOCKED | hidden source bytes";
      const detailText = [
        `Evidence: ${result.evidenceKind} r${String(result.evidenceRevision)} | ${result.sourceKind}`,
        `Publisher: ${result.publisherKind}:${result.publisherRef} | ${result.createdAt}`,
        `Scope: ${scope}`,
        `Path: ${result.artifactRef.path}`,
        `Source digest: ${result.artifactRef.digest}`,
        `Coverage: ${String(result.coverage.pageCount)}/${String(result.pages.length)} VERIFIED | source ${String(result.totalBytes)}B/${String(result.totalLines)}L | rendered ${String(result.renderedTotalBytes)}B/${String(result.renderedTotalLines)}L`,
        `Rendered digest: ${result.renderedArtifactDigest}`,
        `Transformation: ${result.transformation} | Review: ${review}`,
        "--- CONTENT ---",
        result.content,
      ].join("\n");
      const window = presentSafeTextWindow(
        detailText,
        {
          columns: Math.max(1, bounds.x2 - bounds.x1),
          rows: Math.max(1, height),
          offset: Math.max(
            0,
            Math.trunc(ui.detailScrollOffsetByView[presentation.activeView] ?? 0),
          ),
        },
        { sanitizeDisplayText, graphemes, cellWidth },
      );
      detailScrollMaximum = Math.max(0, window.totalLines - 1);
      lines = window.lines;
    } else {
      lines = [
        `Artifact: ${inspection.binding.itemId}`,
        `Read: unavailable | ${inspection.reason}`,
        "Accept/Implement: DISABLED | complete verified content required",
      ];
    }
  } else if (
    inspection?.kind === "repository" &&
    inspection.binding.view === presentation.activeView &&
    inspection.binding.itemId === presentation.detail?.stableId
  ) {
    if (inspection.state === "current") {
      const offset = Math.max(
        0,
        Math.trunc(ui.detailScrollOffsetByView[presentation.activeView] ?? 0),
      );
      const window = presentSafeTextWindow(
        repositoryDetailLines(inspection.repository).join("\n"),
        {
          columns: Math.max(1, bounds.x2 - bounds.x1),
          rows: Math.max(1, height),
          offset,
        },
        { sanitizeDisplayText, graphemes, cellWidth },
      );
      detailScrollMaximum = Math.max(0, window.totalLines - 1);
      lines = window.lines;
    } else {
      lines = [
        `Repository: ${inspection.binding.itemId}`,
        `Read: unavailable | ${inspection.reason}`,
      ];
    }
  } else {
    const detailText = (presentation.detail?.lines ?? [
      { label: "Detail", value: "Select an item to inspect canonical facts." },
    ]).map((detail) => `${detail.label}: ${detail.value}`).join("\n");
    const window = presentSafeTextWindow(
      detailText,
      {
        columns: Math.max(1, bounds.x2 - bounds.x1),
        rows: Math.max(1, height),
        offset: Math.max(
          0,
          Math.trunc(ui.detailScrollOffsetByView[presentation.activeView] ?? 0),
        ),
      },
      { sanitizeDisplayText, graphemes, cellWidth },
    );
    detailScrollMaximum = Math.max(0, window.totalLines - 1);
    lines = window.lines;
  }
  for (const [index, value] of lines.slice(0, height).entries()) {
    const y = bounds.y1 + index;
    const displayed = index === 0 && presentation.focusId === detailId
      ? `>${value}`
      : value;
    if (bounds.x1 === 1 && bounds.x2 === columns) {
      setFabricRow(rows, y, columns, displayed);
    } else {
      rows[y - 1] = writeFixedCells(
        rows[y - 1] ?? " ".repeat(columns),
        bounds.x1,
        bounds.x2 - bounds.x1 + 1,
        chromeText(displayed),
      );
    }
  }
  if (selected !== undefined && detailId !== null) {
    hitRegions.push({
      id: detailId,
      kind: "pager",
      rect: bounds,
      enabled: true,
      geometryKey,
      binding: presentedBinding(dataset, selected),
      scrollMaximum: detailScrollMaximum,
    });
  }
}

function gitPathLines(label: string, page: GitPathPage): readonly string[] {
  return [
    `${label}: ${String(page.paths.length)}${page.truncated ? " | TRUNCATED" : ""}`,
    ...page.paths.map((path) => `${label} path: ${path}`),
  ];
}

function hostedCheckLines(
  hosted: GitRepositoryProjection["hostedChecks"],
): readonly string[] {
  const header = `GitHub checks: ${hosted.freshness.toUpperCase()} r${String(hosted.revision)} @ ${hosted.observedAt}`;
  if (hosted.freshness === "unavailable") {
    return [`${header} | ${hosted.reason}`];
  }
  if (hosted.freshness === "conflict") {
    return [
      `${header} | ${String(hosted.candidates.length)} candidates`,
      ...hosted.candidates.map(
        (candidate) =>
          candidate === null
            ? "GitHub candidate: none"
            : `GitHub candidate: ${candidate.repository} | ${candidate.headObjectDigest} | ${candidate.state} ${String(candidate.passing)}/${String(candidate.total)}`,
      ),
    ];
  }
  if (hosted.value === null) return [`${header} | none`];
  return [
    `${header} | ${hosted.value.state} ${String(hosted.value.passing)}/${String(hosted.value.total)}`,
    `GitHub target: ${hosted.value.repository} | ${hosted.value.headObjectDigest}`,
    `GitHub counts: pass ${String(hosted.value.passing)} | fail ${String(hosted.value.failing)} | pending ${String(hosted.value.pending)} | total ${String(hosted.value.total)}`,
  ];
}

function repositoryDetailLines(
  repository: GitRepositoryProjection,
): readonly string[] {
  const head = repository.head.detached
    ? `detached@${repository.head.objectDigest}`
    : `${repository.head.refName}@${repository.head.objectDigest}`;
  const upstream = repository.upstream === null
    ? "none"
    : `${repository.upstream.remoteName}/${repository.upstream.branchName} +${String(repository.upstream.ahead)} -${String(repository.upstream.behind)}`;
  const logLines = repository.log.items.map(
    (entry) => `Log: ${entry.objectDigest} | ${entry.authorTimestamp} | ${entry.subject}`,
  );
  const logCursorLines = repository.log.hasMore
    ? [
        `Next log cursor: state ${repository.log.nextCursor.repositoryStateDigest} | after ${repository.log.nextCursor.afterObjectDigest}`,
      ]
    : [];
  const branchLines = repository.branches.items.map((branch) =>
    `Branch: ${branch.checkedOut ? "*" : " "} ${branch.refName}@${branch.objectDigest}${
      branch.upstream === null
        ? ""
        : ` -> ${branch.upstream.remoteName}/${branch.upstream.branchName}`
    }`,
  );
  const worktreeLines = repository.worktrees.items.map((worktree) =>
    `Worktree: ${worktree.current ? "CURRENT" : "other"}${worktree.locked ? " LOCKED" : ""} | ${worktree.canonicalPath}`,
  );
  return [
    `Git: ${repository.freshness.toUpperCase()} r${String(repository.revision)} | ${repository.operationState.kind}`,
    `Git observed: ${repository.observedAt}`,
    `HEAD: ${head}`,
    `Upstream: ${upstream}`,
    ...hostedCheckLines(repository.hostedChecks),
    `Root: ${repository.canonicalRepositoryRoot}`,
    `Worktree: ${repository.canonicalWorktreePath}`,
    `Repository state: ${repository.repositoryStateDigest}`,
    `State digests: HEAD ${repository.headDigest} | index ${repository.indexDigest}`,
    `State digests: worktree ${repository.worktreeDigest} | remote ${repository.remoteDigest}`,
    ...gitPathLines("Staged", repository.changes.staged),
    ...gitPathLines("Unstaged", repository.changes.unstaged),
    ...gitPathLines("Untracked", repository.changes.untracked),
    ...gitPathLines("Conflicted", repository.changes.conflicted),
    `Diff: ${repository.diff.selector.kind} | ${repository.diff.baseDigest} -> ${repository.diff.targetDigest}`,
    `Diff artifact: ${repository.diff.artifactRef.path}@${repository.diff.artifactRef.digest}`,
    `Log page: ${String(repository.log.items.length)} | ${repository.log.hasMore ? "MORE" : "END"}`,
    ...logLines,
    ...logCursorLines,
    `Branches: ${String(repository.branches.items.length)}${repository.branches.truncated ? " | TRUNCATED" : ""}`,
    ...branchLines,
    `Worktrees: ${String(repository.worktrees.items.length)}${repository.worktrees.truncated ? " | TRUNCATED" : ""}`,
    ...worktreeLines,
  ];
}
