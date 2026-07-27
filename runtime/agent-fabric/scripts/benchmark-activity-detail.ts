import { performance } from "node:perf_hooks";

import Database from "better-sqlite3";

import {
  parseIdentifier,
  parseOperatorCapabilityGrant,
  type OperatorCapabilityCredential,
} from "@local/agent-fabric-protocol";

import { applyMigrations } from "../src/core/migrations.js";
import {
  projectActivityNarrativeGroups,
  type ActivityNarrativeEventInput,
} from "../src/operator/activity-grouping.js";
import {
  boundedOperatorActivityRows,
} from "../src/operator/activity-projection.js";
import { OperatorProjectionStore } from "../src/operator/projection-store.js";
import { OperatorStore } from "../src/operator/store.js";

const digest = `sha256:${"a".repeat(64)}`;
const now = Date.parse("2027-01-01T00:00:00Z");

function identifier<Kind extends string>(value: string) {
  return parseIdentifier<Kind>(value, "benchmark.identifier");
}

function inputs(
  count: number,
  membersPerGroup: number,
): ActivityNarrativeEventInput[] {
  return Array.from({ length: count }, (_, index) => ({
    eventId: `event_${String(index).padStart(6, "0")}`,
    eventKind: "tool-invoked",
    actorId: "agent_benchmark",
    payloadJson: JSON.stringify({
      taskId: `task_${String(Math.floor(index / membersPerGroup)).padStart(6, "0")}`,
    }),
    occurredAt: new Date(now + index).toISOString() as never,
    sourceRevision: index + 1,
    messageBodyRef: null,
    messageTarget: null,
  }));
}

function median(values: readonly number[]): number {
  const ordered = [...values].sort((left, right) => left - right);
  return ordered[Math.floor(ordered.length / 2)] ?? 0;
}

function benchmarkProjector(count: number): number {
  const events = inputs(count, 32);
  const samples = Array.from(
    { length: count >= 50_000 ? 3 : 5 },
    () => {
      const started = performance.now();
      projectActivityNarrativeGroups("run_benchmark", events);
      return performance.now() - started;
    },
  );
  return median(samples);
}

function projectedRows(groupCount: number, membersPerGroup: number) {
  const groups = projectActivityNarrativeGroups(
    "run_page_benchmark",
    inputs(groupCount * membersPerGroup, membersPerGroup),
  );
  return groups.map(({ group }) => ({
    itemId: group.groupId,
    itemRevision: group.sourceRange.last,
    fact: {
      freshness: "live" as const,
      source: "fabric" as const,
      revision: group.sourceRange.last,
      observedAt: group.occurredAtRange.last,
      value: {
        summary: {
          kind: "activity" as const,
          summary: `${group.kind} activity`,
          occurredAt: group.occurredAtRange.last,
          group,
        },
        detailRef: {
          kind: "activity" as const,
          groupId: group.groupId,
          expectedRevision: group.sourceRange.last,
        },
        actionAvailability: {
          state: "read-only" as const,
          reason: "authority-insufficient",
        },
      },
    },
  }));
}

function emittedPageRows(
  groupCount: number,
  membersPerGroup: number,
  requested: number,
): number {
  const rows = projectedRows(groupCount, membersPerGroup).slice(0, requested);
  return boundedOperatorActivityRows(rows, {
    view: "activity",
    cursor: 0,
    totalRows: groupCount,
    snapshotRevision: 1,
    readTransactionId: "projection:benchmark:session:1",
  }).length;
}

function setupStore(eventCount: number): Readonly<{
  database: Database.Database;
  projections: OperatorProjectionStore;
  credential: OperatorCapabilityCredential;
}> {
  const database = new Database(":memory:");
  applyMigrations(database);
  database.exec(`
    INSERT INTO projects(
      project_id, canonical_root, trust_record_digest, revision,
      authority_generation, created_at, updated_at
    ) VALUES (
      'project_benchmark', '/benchmark', '${digest}', 1, 1, ${now}, ${now}
    );
    INSERT INTO project_sessions(
      project_session_id, project_id, mode, state, revision, generation,
      authority_ref, budget_ref, launch_packet_path, launch_packet_digest,
      membership_revision, origin_kind, origin_operator_id, created_at,
      updated_at
    ) VALUES (
      'session_benchmark', 'project_benchmark', 'coordinated', 'active', 1, 1,
      '${digest}', 'budget_benchmark', 'docs/spec.md', '${digest}', 1,
      'operator-launch', 'operator_benchmark', ${now}, ${now}
    );
    INSERT INTO runs(
      run_id, chair_agent_id, workspace_root, project_run_directory,
      created_at, project_session_id, lifecycle_state, revision,
      chair_generation, chair_lease_id, authority_ref, budget_ref,
      dependency_revision, topology_slot, project_run_directory_basis
    ) VALUES (
      'run_benchmark', 'agent_benchmark', '/benchmark', '.agent-run/benchmark',
      ${now}, 'session_benchmark', 'active', 1, 1, 'chair:benchmark:1',
      '${digest}', 'budget_benchmark', 1, 1, 'project-relative'
    );
    INSERT INTO authorities(
      authority_id, run_id, authority_json, authority_hash, created_at
    ) VALUES (
      'authority_benchmark', 'run_benchmark', '{}', '${"b".repeat(64)}', ${now}
    );
    INSERT INTO agents(
      run_id, agent_id, authority_id, provider_session_ref, lifecycle
    ) VALUES (
      'run_benchmark', 'agent_benchmark', 'authority_benchmark', NULL, 'ready'
    );
  `);
  const insertEvent = database.prepare(`
    INSERT INTO events(
      event_id, run_id, type, actor_agent_id, payload_json, created_at
    ) VALUES (?, 'run_benchmark', 'tool-invoked', 'agent_benchmark', ?, ?)
  `);
  const insertSequence = database.prepare(`
    INSERT INTO observer_event_sequence(event_id) VALUES (?)
  `);
  database.transaction(() => {
    for (let index = 0; index < eventCount; index += 1) {
      const eventId = `event_${String(index).padStart(6, "0")}`;
      insertEvent.run(
        eventId,
        JSON.stringify({
          taskId: `task_${String(Math.floor(index / 32)).padStart(6, "0")}`,
        }),
        now + index,
      );
      insertSequence.run(eventId);
    }
  })();
  const operatorStore = new OperatorStore({ database, clock: () => now });
  operatorStore.registerPrincipal({
    operatorId: "operator_benchmark",
    projectId: "project_benchmark",
    authenticatedSubjectHash: "benchmark-subject",
    projectAuthorityGeneration: 1,
  });
  operatorStore.issueCapability(parseOperatorCapabilityGrant({
    capabilityId: "cap_benchmark",
    operatorId: "operator_benchmark",
    projectId: "project_benchmark",
    projectAuthorityGeneration: 1,
    principalGeneration: 1,
    issuedAt: "2026-01-01T00:00:00Z",
    expiresAt: "2099-01-01T00:00:00Z",
    status: "active",
    kind: "session",
    projectSessionId: "session_benchmark",
    sessionGeneration: 1,
    actions: ["read"],
  }), "benchmark-secret");
  return {
    database,
    projections: new OperatorProjectionStore({
      database,
      operatorStore,
      clock: () => now,
    }),
    credential: {
      capabilityId: identifier<"CapabilityId">("cap_benchmark"),
      token: "benchmark-secret",
    },
  };
}

function benchmarkDetailReads(eventCount: number) {
  const fixture = setupStore(eventCount);
  try {
    const projectId = identifier<"ProjectId">("project_benchmark");
    const projectSessionId =
      identifier<"ProjectSessionId">("session_benchmark");
    const snapshot = fixture.projections.snapshot({
      credential: fixture.credential,
      projectId,
      projectSessionId,
    }, "include");
    const legacyStarted = performance.now();
    fixture.projections.detail({
      credential: fixture.credential,
      projectId,
      projectSessionId,
      snapshotRevision: snapshot.snapshotRevision,
      detailRef: {
        kind: "activity",
        eventId: identifier<"EventId">("event_000000"),
        expectedRevision: 1,
      },
    });
    const legacyDetailMs = performance.now() - legacyStarted;

    const page = fixture.projections.viewPage(
      {
        credential: fixture.credential,
        projectId,
        projectSessionId,
        view: "activity",
        snapshotRevision: snapshot.snapshotRevision,
        cursor: 0,
        limit: 2,
      },
      "include",
      "include",
      "include",
      "include",
      "include",
      "include",
      "include",
    );
    if (
      page.status !== "page" ||
      page.rows[1]?.fact.freshness !== "live" ||
      page.rows[1].fact.value.detailRef.kind !== "activity" ||
      !("groupId" in page.rows[1].fact.value.detailRef)
    ) {
      throw new Error("benchmark could not resolve the first activity group");
    }
    const groupRef = page.rows[1].fact.value.detailRef;
    const openedAt = performance.now();
    const groupDetail = fixture.projections.detail(
      {
        credential: fixture.credential,
        projectId,
        projectSessionId,
        snapshotRevision: snapshot.snapshotRevision,
        detailRef: groupRef,
      },
      "include",
      "include",
      "include",
      "include",
      "include",
      "include",
    );
    if (
      groupDetail.status !== "current" ||
      groupDetail.detail.freshness !== "live" ||
      groupDetail.detail.value.kind !== "activity" ||
      !("group" in groupDetail.detail.value)
    ) {
      throw new Error("benchmark could not open the activity group");
    }
    let memberReads = 0;
    for (const detail of groupDetail.detail.value.memberDetails) {
      if (detail.status !== "referenced") continue;
      fixture.projections.detail(
        {
          credential: fixture.credential,
          projectId,
          projectSessionId,
          snapshotRevision: snapshot.snapshotRevision,
          detailRef: detail.detailRef,
        },
        "include",
        "include",
        "include",
        "include",
        "include",
        "include",
      );
      memberReads += 1;
    }
    return {
      legacyDetailMs,
      groupedOpenMs: performance.now() - openedAt,
      detailReads: memberReads + 1,
      members: memberReads,
    };
  } finally {
    fixture.database.close();
  }
}

const result = {
  projectorMs: Object.fromEntries(
    [1_000, 10_000, 50_000].map((count) => [
      String(count),
      benchmarkProjector(count),
    ]),
  ),
  emittedPageRows: [
    { groups: 200, members: 1, requested: 50, emitted: emittedPageRows(200, 1, 50) },
    { groups: 50, members: 4, requested: 50, emitted: emittedPageRows(50, 4, 50) },
    { groups: 20, members: 32, requested: 20, emitted: emittedPageRows(20, 32, 20) },
  ],
  detail50k: benchmarkDetailReads(50_000),
};

process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
