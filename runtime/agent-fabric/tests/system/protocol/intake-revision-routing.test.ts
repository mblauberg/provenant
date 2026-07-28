import Database from "better-sqlite3";
import { createHash } from "node:crypto";
import { renameSync, writeFileSync } from "node:fs";
import { link, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  FABRIC_OPERATIONS,
  parseLaunchPacketV1,
  parseLaunchResourcePlanV1,
  parseIntakeRevisionRequest,
  parseOperatorCapabilityGrant,
  type IntakeRevisionRequest,
  type ProjectSessionLaunchPacketPrepareRequest,
} from "@local/agent-fabric-protocol";
import { describe, expect, it } from "vitest";

import { openFabric, type Fabric } from "../../../src/index.ts";
import type { PublicProtocolContext } from "../../../src/daemon/public-protocol.ts";
import { OperatorStore } from "../../../src/operator/store.ts";
import { OperatorProjectionStore } from "../../../src/operator/projection-store.ts";
import { canonicalJson, sha256 } from "../../../src/persistence/row-codec.ts";
import { ROOT_AUTHORITY } from "../../support/stage1-fixture.ts";
import { createCurrentSessionRun } from "../../support/current-session-testkit.ts";

const now = Date.parse("2027-01-01T00:00:00Z");
const digestA = `sha256:${"a".repeat(64)}`;
const digestB = `sha256:${"b".repeat(64)}`;
const fakeProviderToken = `sk-${"A".repeat(24)}`;
const fakePrivateKey = ["-----BEGIN PRIVATE", " KEY-----\nnot-real\n-----END PRIVATE", " KEY-----"].join("");

async function quarantinedLaunchPreparationContents(directory: string): Promise<string[]> {
  const quarantine = join(directory, ".agent-run/.fabric-custody");
  let names: string[];
  try {
    names = await readdir(quarantine);
  } catch (error: unknown) {
    if (typeof error === "object" && error !== null && "code" in error && error.code === "ENOENT") return [];
    throw error;
  }
  return await Promise.all(names.map((name) => readFile(join(quarantine, name), "utf8")));
}

type RoutingFixture = {
  fabric: Fabric;
  directory: string;
  databasePath: string;
  projectId: string;
  projectSessionId: string;
  implementationSessionId: string;
  operatorContext: PublicProtocolContext;
  implementationContext: PublicProtocolContext;
  chairContext: PublicProtocolContext;
};

async function setupRoutingFixture(
  fault?: (label: string) => void,
  implementationActions: readonly ["read", ...Array<"read" | "decide" | "launch">] = ["read", "decide", "launch"],
): Promise<RoutingFixture> {
  const directory = await mkdtemp(join(tmpdir(), "fabric-intake-revision-routing-"));
  const databasePath = join(directory, "fabric.sqlite3");
  const initial = await openFabric({ databasePath, workspaceRoots: [directory], clock: () => now });
  const created = await createCurrentSessionRun({
    databasePath,
    workspaceRoot: directory,
    runId: "run_intake_revision",
    chair: {
      agentId: "chair_01",
      authority: {
        ...ROOT_AUTHORITY,
        actions: [...new Set([...ROOT_AUTHORITY.actions, FABRIC_OPERATIONS.intakeRevise])],
      },
    },
  });
  await initial.close();

  const database = new Database(databasePath);
  let projectId = "";
  let projectSessionId = "";
  try {
    database.pragma("foreign_keys = ON");
    const identity = database.prepare(`
      SELECT p.project_id, p.authority_generation, s.project_session_id, s.generation
        FROM projects p JOIN project_sessions s ON s.project_id=p.project_id
       JOIN runs r ON r.project_session_id=s.project_session_id
       WHERE r.run_id='run_intake_revision'
    `).get() as {
      project_id: string;
      authority_generation: number;
      project_session_id: string;
      generation: number;
    };
    projectId = identity.project_id;
    projectSessionId = identity.project_session_id;
    database.prepare(`
      UPDATE run_chair_leases SET status='active'
       WHERE project_session_id=? AND run_id='run_intake_revision' AND generation=1
    `).run(projectSessionId);
    database.prepare(`
      INSERT INTO scoped_gates(
        gate_id, project_session_id, coordination_run_id, dedupe_key, scope_kind,
        scope_task_id, dependency_revision, blocked_operation_ids_json,
        enforcement_points_json, question, reason, options_json, recommendation,
        consequences_json, evidence_refs_json, created_by_ref, expected_approver_ref,
        deadline, default_action, status, human_required, release_binding_json,
        revision, created_at, updated_at
      ) VALUES (
        'gate_routing', ?, 'run_intake_revision', 'gate-routing', 'run', NULL, 1, '[]',
        '["scoped-barrier"]', 'Routing gate?', 'Required.', '["approve"]', 'approve',
        '[]', '[]', 'agent:chair_01', 'authenticated-human-operator', NULL, NULL,
        'pending', 1, NULL, 2, ?, ?
      )
    `).run(projectSessionId, now, now);
    database.prepare(`
      INSERT INTO intakes(
        intake_id, project_id, project_session_id, coordination_run_id, dedupe_key,
        state, revision, chair_request_id, chair_request_revision, summary,
        artifact_refs_json, gate_ids_json, payload_digest, created_at, updated_at
      ) VALUES (
        'intake_routing', ?, ?, 'run_intake_revision', 'intake-routing',
        'awaiting-chair', 2, 'message_intake_routing', 1, 'Discuss routing', ?,
        '["gate_routing"]', ?, ?, ?
      )
    `).run(
      projectId,
      projectSessionId,
      JSON.stringify([{ path: "docs/spec.md", digest: digestA }]),
      digestA,
      now,
      now,
    );
    database.prepare(`
      INSERT INTO intake_revisions(intake_id, revision, state, payload_json, payload_digest, actor_ref, created_at)
      VALUES
        ('intake_routing', 1, 'draft', '{}', ?, 'operator_routing', ?),
        ('intake_routing', 2, 'awaiting-chair', '{}', ?, 'operator_routing', ?)
    `).run(digestA, now, digestA, now);
    database.prepare(`
      INSERT INTO artifacts(
        artifact_id, project_id, project_session_id, run_id, task_id,
        publisher_kind, publisher_ref, publisher_agent_id, source_kind, evidence_kind,
        relative_path, sha256, registry_state, quarantine_reason, revision, created_at
      ) VALUES (
        'artifact_routing', ?, ?, 'run_intake_revision', NULL,
        'project', 'project-owned', NULL, 'project-file', 'artifact',
        'docs/spec.md', ?, 'active', NULL, 1, ?
      )
    `).run(projectId, projectSessionId, digestA, now);
    database.prepare(`
      INSERT INTO artifacts(
        artifact_id, project_id, project_session_id, run_id, task_id,
        publisher_kind, publisher_ref, publisher_agent_id, source_kind, evidence_kind,
        relative_path, sha256, registry_state, quarantine_reason, revision, created_at
      ) VALUES (
        'artifact_routing_plan', ?, ?, 'run_intake_revision', NULL,
        'project', 'project-owned', NULL, 'project-file', 'artifact',
        'plans/routing.md', ?, 'active', NULL, 1, ?
      )
    `).run(projectId, projectSessionId, digestB, now);
    database.prepare(`
      INSERT INTO intake_artifact_bindings(
        intake_id, intake_revision, artifact_id, relative_path, sha256
      ) VALUES ('intake_routing', 2, 'artifact_routing', 'docs/spec.md', ?)
    `).run(digestA);
    database.prepare(`
      INSERT INTO intake_gate_bindings(intake_id, intake_revision, gate_id, gate_revision)
      VALUES ('intake_routing', 2, 'gate_routing', 2)
    `).run();
    database.prepare(`
      INSERT INTO messages(
        message_id, run_id, sender_id, dedupe_key, payload_hash, audience_json,
        kind, body, requires_ack, conversation_id, reply_to_message_id,
        task_revision, hop_count, expires_at, created_at
      ) VALUES (
        'message_intake_routing', 'run_intake_revision', 'chair_01', 'intake-routing-one',
        'payload-one', '{"agentId":"chair_01","kind":"agent"}', 'request',
        'Discuss routing', 1, 'conversation_intake_routing', NULL, 1, 0, ?, ?
      )
    `).run(Date.parse("2099-01-01T00:00:00Z"), now);

    const operatorStore = new OperatorStore({ database, clock: () => now });
    operatorStore.registerPrincipal({
      operatorId: "operator_routing",
      projectId,
      authenticatedSubjectHash: "routing-subject-hash",
      projectAuthorityGeneration: identity.authority_generation,
    });
    operatorStore.issueCapability(parseOperatorCapabilityGrant({
      capabilityId: "cap_intake_routing",
      operatorId: "operator_routing",
      projectId,
      projectSessionId,
      projectAuthorityGeneration: identity.authority_generation,
      sessionGeneration: identity.generation,
      principalGeneration: 1,
      issuedAt: "2026-01-01T00:00:00Z",
      expiresAt: "2099-01-01T00:00:00Z",
      status: "active",
      kind: "session",
      actions: ["read", "decide"],
    }), "intake-routing-secret");
    database.prepare(`
      INSERT INTO project_sessions(
        project_session_id, project_id, mode, state, revision, generation, authority_ref,
        budget_ref, launch_packet_path, launch_packet_digest, membership_revision,
        origin_kind, origin_operator_id, created_at, updated_at
      ) VALUES (
        'session_implementation_target', ?, 'coordinated', 'draft', 1, 1, ?,
        'budget_implementation', 'launch/pending.json', ?, 1,
        'operator-launch', 'operator_routing', ?, ?
      )
    `).run(projectId, digestA, digestA, now, now);
    operatorStore.issueCapability(parseOperatorCapabilityGrant({
      capabilityId: "cap_implementation_routing",
      operatorId: "operator_routing",
      projectId,
      projectSessionId: "session_implementation_target",
      projectAuthorityGeneration: identity.authority_generation,
      sessionGeneration: 1,
      principalGeneration: 1,
      issuedAt: "2026-01-01T00:00:00Z",
      expiresAt: "2099-01-01T00:00:00Z",
      status: "active",
      kind: "session",
      actions: implementationActions,
    }), "implementation-routing-secret");
  } finally {
    database.close();
  }

  const fabric = await openFabric({
    databasePath,
    workspaceRoots: [directory],
    clock: () => now,
    ...(fault === undefined ? {} : { fault }),
  });
  const operator = fabric.verifyProtocolCredential("intake-routing-secret");
  if (operator.principal.kind !== "operator") throw new Error("expected operator principal");
  const chair = fabric.verifyProtocolCredential(created.chairCapability);
  const implementation = fabric.verifyProtocolCredential("implementation-routing-secret");
  if (chair.principal.kind !== "agent") throw new Error("expected chair principal");
  if (implementation.principal.kind !== "operator") throw new Error("expected implementation operator principal");
  return {
    fabric,
    directory,
    databasePath,
    projectId,
    projectSessionId,
    implementationSessionId: "session_implementation_target",
    operatorContext: {
      principal: operator.principal,
      allowedOperations: new Set(operator.grantedOperations),
      features: ["intakes.v1"],
      connectionNonce: "connection_intake_operator",
      credentialHash: createHash("sha256").update("intake-routing-secret").digest("hex"),
      daemonInstanceGeneration: 1,
    },
    implementationContext: {
      principal: implementation.principal,
      allowedOperations: new Set(implementation.grantedOperations),
      features: ["project-sessions.v1", "launch-custody.v1"],
      connectionNonce: "connection_implementation_operator",
      credentialHash: createHash("sha256").update("implementation-routing-secret").digest("hex"),
      daemonInstanceGeneration: 1,
    },
    chairContext: {
      principal: chair.principal,
      allowedOperations: new Set(chair.grantedOperations),
      features: ["intakes.v1"],
      connectionNonce: "connection_intake_chair",
      credentialHash: createHash("sha256").update(created.chairCapability).digest("hex"),
      daemonInstanceGeneration: 1,
    },
  };
}

function operatorRequest(fixture: RoutingFixture): Extract<IntakeRevisionRequest, { origin: "operator" }> {
  const parsed = parseIntakeRevisionRequest({
    origin: "operator",
    command: {
      credential: { capabilityId: "cap_intake_routing", token: "intake-routing-secret" },
      commandId: "command_intake_routing_operator",
      expectedRevision: 2,
      actor: "operator_routing",
      provenance: { kind: "console-direct-input", clientId: "console_routing", inputEventId: "input_routing" },
      evidenceRefs: [{ path: "plans/routing.md", digest: digestB }],
    },
    intakeId: "intake_routing",
    projectSessionId: fixture.projectSessionId,
    coordinationRunId: "run_intake_revision",
    expectedRevision: 2,
    state: "discussing",
    summary: "Discuss authenticated routing",
    artifactRefs: [{ path: "plans/routing.md", digest: digestB }],
    gateIds: ["gate_routing"],
  });
  if (parsed.origin !== "operator") throw new Error("expected operator revision");
  return parsed;
}

function chairRequest(fixture: RoutingFixture): Extract<IntakeRevisionRequest, { origin: "chair" }> {
  const parsed = parseIntakeRevisionRequest({
    origin: "chair",
    command: {
      commandId: "command_intake_routing_chair",
      agentId: "chair_01",
      projectSessionId: fixture.projectSessionId,
      coordinationRunId: "run_intake_revision",
      principalGeneration: 1,
      chairLeaseId: "chair:run_intake_revision:1",
      chairLeaseGeneration: 1,
      expectedRunRevision: 1,
      expectedRevision: 2,
    },
    intakeId: "intake_routing",
    projectSessionId: fixture.projectSessionId,
    coordinationRunId: "run_intake_revision",
    expectedRevision: 2,
    state: "awaiting-human",
    summary: "Chair requests a human routing decision",
    artifactRefs: [{ path: "plans/routing.md", digest: digestB }],
    gateIds: ["gate_routing"],
  });
  if (parsed.origin !== "chair") throw new Error("expected chair revision");
  return parsed;
}

function acceptedRequest(fixture: RoutingFixture): Extract<IntakeRevisionRequest, { origin: "operator" }> {
  const scope = { path: "plans/routing.md", digest: digestB };
  const parsed = parseIntakeRevisionRequest({
    ...operatorRequest(fixture),
    command: {
      ...operatorRequest(fixture).command,
      commandId: "command_intake_routing_accept",
      evidenceRefs: [scope],
    },
    state: "accepted",
    summary: "Accept exact registered routing scope",
    artifactRefs: [scope],
    acceptedScopeRef: scope,
  });
  if (parsed.origin !== "operator") throw new Error("expected accepted operator revision");
  return parsed;
}

function implementationRequest(
  fixture: RoutingFixture,
  commandId: string,
  prompt = "Implement accepted routing scope.",
  nearNameMaxArtifacts = false,
): ProjectSessionLaunchPacketPrepareRequest {
  const resourcePlan = parseLaunchResourcePlanV1({
    schemaVersion: 1,
    projectId: fixture.projectId,
    projectSessionId: fixture.implementationSessionId,
    runId: "run_implementation_target",
    budgetRef: "budget_implementation",
    scopes: {
      project: { scopeId: "scope_implementation_project", limits: { provider_calls: 2, concurrent_turns: 2 } },
      projectSession: { scopeId: "scope_implementation_session", limits: { provider_calls: 2, concurrent_turns: 2 } },
      coordinationRun: { scopeId: "scope_implementation_run", limits: { provider_calls: 1, concurrent_turns: 1 } },
    },
    launchReservation: { amounts: { provider_calls: 1, concurrent_turns: 1 } },
  });
  const resourcePlanRef = {
    path: (
      nearNameMaxArtifacts
        ? `.agent-run/run_implementation_target/${"r".repeat(217)}.json`
        : ".agent-run/run_implementation_target/launch-resources.json"
    ) as never,
    digest: `sha256:${sha256(canonicalJson(resourcePlan))}` as never,
  };
  const launchPacket = parseLaunchPacketV1({
    schemaVersion: 1,
    projectId: fixture.projectId,
    projectSessionId: fixture.implementationSessionId,
    runId: "run_implementation_target",
    chairAgentId: "chair_implementation_target",
    projectRunDirectory: ".agent-run/run_implementation_target",
    topologyMode: "coordinated",
    budgetRef: "budget_implementation",
    resourcePlanRef,
    chairAuthority: {
      ...ROOT_AUTHORITY,
      approval: {
        ...ROOT_AUTHORITY.approval,
        evidenceId: "accepted-routing-scope",
        evidenceDigest: digestB,
      },
      sourcePaths: ["runtime/agent-fabric-console"],
      artifactPaths: [".agent-run/run_implementation_target"],
      budget: { provider_calls: 1, concurrent_turns: 1 },
    },
    provider: {
      adapterId: "fake",
      actionId: "provider_implementation_target",
      contractDigest: digestA,
      inputSchemaId: "provider-launch.v1",
      input: { prompt, model: "reviewed-provider-route" },
    },
  });
  return {
    command: {
      credential: {
        capabilityId: "cap_implementation_routing" as never,
        token: "implementation-routing-secret",
      },
      commandId: commandId as never,
      expectedRevision: 1,
      actor: "operator_routing" as never,
      provenance: {
        kind: "console-direct-input",
        clientId: "console_implementation" as never,
        inputEventId: `input_${commandId}` as never,
      },
      evidenceRefs: [{ path: "plans/routing.md" as never, digest: digestB as never }],
    },
    projectId: fixture.projectId as never,
    projectSessionId: fixture.implementationSessionId as never,
    expectedSessionGeneration: 1,
    intakeId: "intake_routing",
    acceptedScopeRef: { path: "plans/routing.md" as never, digest: digestB as never },
    launchPacketRef: {
      path: (
        nearNameMaxArtifacts
          ? `.agent-run/run_implementation_target/${"p".repeat(217)}.json`
          : ".agent-run/run_implementation_target/launch-packet.json"
      ) as never,
      digest: `sha256:${sha256(canonicalJson(launchPacket))}` as never,
    },
    resourcePlanRef,
    launchPacket,
    resourcePlan,
  };
}

describe("intake revision public routing", () => {
  it("dispatches an authenticated operator revision through the public protocol", async () => {
    const fixture = await setupRoutingFixture();
    try {
      const request = operatorRequest(fixture);
      const revised = await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        request,
      );
      expect(revised).toMatchObject({
        intakeId: "intake_routing",
        projectId: fixture.projectId,
        revision: 3,
        state: "discussing",
      });
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        request,
      )).resolves.toEqual(revised);
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("dispatches an authenticated active-chair revision through the agent protocol", async () => {
    const fixture = await setupRoutingFixture();
    try {
      const request = chairRequest(fixture);
      const revised = await fixture.fabric.dispatchPublicProtocol(
        fixture.chairContext,
        FABRIC_OPERATIONS.intakeRevise,
        request,
      );
      expect(revised).toMatchObject({
        intakeId: "intake_routing",
        projectId: fixture.projectId,
        revision: 3,
        state: "awaiting-human",
      });
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.chairContext,
        FABRIC_OPERATIONS.intakeRevise,
        request,
      )).resolves.toEqual(revised);
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("replays one durable intake history after daemon restart", async () => {
    const fixture = await setupRoutingFixture();
    let initialClosed = false;
    let reopened: Fabric | undefined;
    try {
      const request = operatorRequest(fixture);
      const revised = await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        request,
      );
      await fixture.fabric.close();
      initialClosed = true;

      reopened = await openFabric({
        databasePath: fixture.databasePath,
        workspaceRoots: [fixture.directory],
        clock: () => now,
      });
      const verified = reopened.verifyProtocolCredential("intake-routing-secret");
      if (verified.principal.kind !== "operator") throw new Error("expected operator principal");
      const context: PublicProtocolContext = {
        principal: verified.principal,
        allowedOperations: new Set(verified.grantedOperations),
        features: ["intakes.v1"],
        connectionNonce: "connection_intake_operator_restarted",
        credentialHash: createHash("sha256").update("intake-routing-secret").digest("hex"),
        daemonInstanceGeneration: 2,
      };
      await expect(reopened.dispatchPublicProtocol(
        context,
        FABRIC_OPERATIONS.intakeRevise,
        request,
      )).resolves.toEqual(revised);
      await reopened.close();
      reopened = undefined;

      const database = new Database(fixture.databasePath, { readonly: true });
      try {
        expect(database.prepare(`
          SELECT count(*) AS count FROM intake_revisions WHERE intake_id='intake_routing'
        `).get()).toEqual({ count: 3 });
        expect(database.prepare(`
          SELECT count(*) AS count FROM operator_commands
           WHERE operator_id='operator_routing' AND command_id='command_intake_routing_operator'
        `).get()).toEqual({ count: 1 });
      } finally {
        database.close();
      }
    } finally {
      if (!initialClosed) await fixture.fabric.close();
      if (reopened !== undefined) await reopened.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("rejects a revision whose declared origin differs from its authenticated principal", async () => {
    const fixture = await setupRoutingFixture();
    try {
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        chairRequest(fixture),
      )).rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.chairContext,
        FABRIC_OPERATIONS.intakeRevise,
        operatorRequest(fixture),
      )).rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("persists and projects one exact accepted scope while advancing the project revision", async () => {
    const fixture = await setupRoutingFixture();
    try {
      const before = new Database(fixture.databasePath, { readonly: true });
      const beforeRevision = (before.prepare("SELECT revision FROM projects WHERE project_id=?")
        .get(fixture.projectId) as { revision: number }).revision;
      before.close();
      const accepted = await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        acceptedRequest(fixture),
      );
      expect(accepted).toMatchObject({
        revision: 3,
        state: "accepted",
        acceptedScopeRef: { path: "plans/routing.md", digest: digestB },
      });

      const database = new Database(fixture.databasePath);
      try {
        expect(database.prepare(`
          SELECT accepted_scope_artifact_id, accepted_scope_state
            FROM intakes WHERE intake_id='intake_routing'
        `).get()).toEqual({
          accepted_scope_artifact_id: "artifact_routing_plan",
          accepted_scope_state: "bound",
        });
        expect(database.prepare("SELECT revision FROM projects WHERE project_id=?").get(fixture.projectId))
          .toEqual({ revision: beforeRevision + 1 });
        database.prepare(`
          INSERT INTO artifacts(
            artifact_id,project_id,project_session_id,run_id,task_id,publisher_kind,
            publisher_ref,publisher_agent_id,source_kind,evidence_kind,relative_path,
            sha256,registry_state,quarantine_reason,revision,created_at
          ) VALUES ('artifact_quarantined_scope',?,NULL,NULL,NULL,'project','test',NULL,
                    'project-file','artifact','quarantined.md',?,'quarantined','test',1,?)
        `).run(fixture.projectId, `sha256:${"c".repeat(64)}`, now);
        expect(() => database.prepare(`
          UPDATE intakes SET accepted_scope_artifact_id='artifact_quarantined_scope'
           WHERE intake_id='intake_routing'
        `).run()).toThrow(/accepted scope must reference one active exact-scope registry row/iu);
        const operatorStore = new OperatorStore({ database, clock: () => now });
        const projection = new OperatorProjectionStore({ database, operatorStore, clock: () => now });
        const credential = {
          capabilityId: "cap_intake_routing" as never,
          token: "intake-routing-secret",
        };
        const snapshot = projection.snapshot({
          credential,
          projectId: fixture.projectId as never,
          projectSessionId: fixture.projectSessionId as never,
        }, "include");
        const page = projection.viewPage({
          credential,
          projectId: fixture.projectId as never,
          projectSessionId: fixture.projectSessionId as never,
          view: "project",
          snapshotRevision: snapshot.snapshotRevision,
          cursor: 0,
          limit: 10,
        }, "include");
        expect(page).toMatchObject({
          status: "page",
          rows: [{
            fact: {
              value: {
                summary: {
                  acceptedScopeRef: { path: "plans/routing.md", digest: digestB },
                },
              },
            },
          }],
        });
        const projectFact = page.status === "page" ? page.rows[0]?.fact : undefined;
        if (
          projectFact === undefined ||
          projectFact.freshness === "unavailable" ||
          projectFact.freshness === "conflict" ||
          projectFact.value.detailRef.kind !== "project"
        ) {
          throw new Error("project detail reference unavailable");
        }
        expect(projection.detail({
          credential,
          projectId: fixture.projectId as never,
          projectSessionId: fixture.projectSessionId as never,
          snapshotRevision: snapshot.snapshotRevision,
          detailRef: projectFact.value.detailRef,
        })).toMatchObject({
          status: "current",
          detail: {
            value: {
              acceptedScopeRef: { path: "plans/routing.md", digest: digestB },
            },
          },
        });
      } finally {
        database.close();
      }
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("closes accepted evidence into one restart-safe launch packet without dispatching a provider", async () => {
    const fixture = await setupRoutingFixture();
    let initialClosed = false;
    let reopened: Fabric | undefined;
    try {
      await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        acceptedRequest(fixture),
      );
      const resourcePlan = parseLaunchResourcePlanV1({
        schemaVersion: 1,
        projectId: fixture.projectId,
        projectSessionId: fixture.implementationSessionId,
        runId: "run_implementation_target",
        budgetRef: "budget_implementation",
        scopes: {
          project: { scopeId: "scope_implementation_project", limits: { provider_calls: 2, concurrent_turns: 2 } },
          projectSession: { scopeId: "scope_implementation_session", limits: { provider_calls: 2, concurrent_turns: 2 } },
          coordinationRun: { scopeId: "scope_implementation_run", limits: { provider_calls: 1, concurrent_turns: 1 } },
        },
        launchReservation: { amounts: { provider_calls: 1, concurrent_turns: 1 } },
      });
      const resourcePlanRef = {
        path: ".agent-run/run_implementation_target/launch-resources.json" as never,
        digest: `sha256:${sha256(canonicalJson(resourcePlan))}` as never,
      };
      const launchPacket = parseLaunchPacketV1({
        schemaVersion: 1,
        projectId: fixture.projectId,
        projectSessionId: fixture.implementationSessionId,
        runId: "run_implementation_target",
        chairAgentId: "chair_implementation_target",
        projectRunDirectory: ".agent-run/run_implementation_target",
        topologyMode: "coordinated",
        budgetRef: "budget_implementation",
        resourcePlanRef,
        chairAuthority: {
          ...ROOT_AUTHORITY,
          approval: {
            ...ROOT_AUTHORITY.approval,
            evidenceId: "accepted-routing-scope",
            evidenceDigest: digestB,
          },
          sourcePaths: ["runtime/agent-fabric-console"],
          artifactPaths: [".agent-run/run_implementation_target"],
          budget: { provider_calls: 1, concurrent_turns: 1 },
        },
        provider: {
          adapterId: "fake",
          actionId: "provider_implementation_target",
          contractDigest: digestA,
          inputSchemaId: "provider-launch.v1",
          input: {
            prompt: "Reopen plans/routing.md at its accepted digest and implement it.",
            model: "reviewed-provider-route",
          },
        },
      });
      const launchPacketRef = {
        path: ".agent-run/run_implementation_target/launch-packet.json" as never,
        digest: `sha256:${sha256(canonicalJson(launchPacket))}` as never,
      };
      const misScopedPacket = parseLaunchPacketV1({
        ...launchPacket,
        projectRunDirectory: ".agent-run/another-run",
      });
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        {
          command: {
            credential: {
              capabilityId: "cap_implementation_routing" as never,
              token: "implementation-routing-secret",
            },
            commandId: "command_implementation_mis_scoped" as never,
            expectedRevision: 1,
            actor: "operator_routing" as never,
            provenance: {
              kind: "console-direct-input",
              clientId: "console_implementation" as never,
              inputEventId: "input_implementation_mis_scoped" as never,
            },
            evidenceRefs: [{ path: "plans/routing.md" as never, digest: digestB as never }],
          },
          projectId: fixture.projectId as never,
          projectSessionId: fixture.implementationSessionId as never,
          expectedSessionGeneration: 1,
          intakeId: "intake_routing",
          acceptedScopeRef: { path: "plans/routing.md" as never, digest: digestB as never },
          launchPacketRef: {
            ...launchPacketRef,
            digest: `sha256:${sha256(canonicalJson(misScopedPacket))}` as never,
          },
          resourcePlanRef,
          launchPacket: misScopedPacket,
          resourcePlan,
        },
      )).rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
      const preparationRequest = {
        command: {
          credential: {
            capabilityId: "cap_implementation_routing" as never,
            token: "implementation-routing-secret",
          },
          commandId: "command_implementation_prepare" as never,
          expectedRevision: 1,
          actor: "operator_routing" as never,
          provenance: {
            kind: "console-direct-input" as const,
            clientId: "console_implementation" as never,
            inputEventId: "input_implementation_confirm" as never,
          },
          evidenceRefs: [{ path: "plans/routing.md" as never, digest: digestB as never }],
        },
        projectId: fixture.projectId as never,
        projectSessionId: fixture.implementationSessionId as never,
        expectedSessionGeneration: 1,
        intakeId: "intake_routing",
        acceptedScopeRef: { path: "plans/routing.md" as never, digest: digestB as never },
        launchPacketRef,
        resourcePlanRef,
        launchPacket,
        resourcePlan,
      };
      const prepared = await fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        preparationRequest,
      );
      expect(prepared).toMatchObject({
        projectSession: { state: "awaiting_launch", launchPacketRef },
        launchPacketRef,
        resourcePlanRef,
        acceptedScopeRef: { path: "plans/routing.md", digest: digestB },
      });
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        preparationRequest,
      )).resolves.toEqual(prepared);
      await expect(readFile(join(fixture.directory, launchPacketRef.path), "utf8"))
        .resolves.toBe(canonicalJson(launchPacket));
      await expect(readFile(join(fixture.directory, resourcePlanRef.path), "utf8"))
        .resolves.toBe(canonicalJson(resourcePlan));
      const database = new Database(fixture.databasePath, { readonly: true });
      expect(database.prepare("SELECT COUNT(*) AS count FROM provider_actions WHERE action_id='provider_implementation_target'").get())
        .toEqual({ count: 0 });
      database.close();

      await fixture.fabric.close();
      initialClosed = true;
      reopened = await openFabric({ databasePath: fixture.databasePath, workspaceRoots: [fixture.directory], clock: () => now });
      await expect(reopened.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionGet,
        {
          projectId: fixture.projectId as never,
          projectSessionId: fixture.implementationSessionId as never,
          expectedGeneration: 1,
        },
      )).resolves.toMatchObject({ state: "awaiting_launch", launchPacketRef });
      await expect(reopened.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        preparationRequest,
      )).resolves.toEqual(prepared);
    } finally {
      if (!initialClosed) await fixture.fabric.close();
      if (reopened !== undefined) await reopened.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it.each([
    ["launch-only", ["read", "launch"] as const, true],
    ["decide-only", ["read", "decide"] as const, false],
  ])("enforces %s least-privilege admission for launch packet preparation", async (_label, actions, admitted) => {
    const fixture = await setupRoutingFixture(undefined, actions);
    try {
      await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        acceptedRequest(fixture),
      );
      const request = implementationRequest(fixture, `least_privilege_${String(_label)}`);
      const operation = fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      );
      if (admitted) {
        await expect(operation).resolves.toMatchObject({ projectSession: { state: "awaiting_launch" } });
      } else {
        await expect(operation).rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
      }
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it.each([
    ["trusted key", { apiKey: "not-a-real-provider-key" }],
    ["fabric bearer", { prompt: "use afc_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" }],
    ["provider token", { prompt: `use ${fakeProviderToken}` }],
    ["password assignment", { prompt: "password=not-a-real-password" }],
    ["private key", { prompt: fakePrivateKey }],
    ["URL userinfo", { prompt: "https://operator:not-real@example.invalid/task" }],
  ])("rejects %s in provider input before claiming or writing launch preparation", async (_label, providerInput) => {
    const fixture = await setupRoutingFixture();
    try {
      await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        acceptedRequest(fixture),
      );
      const base = implementationRequest(fixture, `unsafe_${String(_label).replaceAll(" ", "_")}`);
      const launchPacket = parseLaunchPacketV1({
        ...base.launchPacket,
        provider: { ...base.launchPacket.provider, input: providerInput },
      });
      const request = {
        ...base,
        launchPacket,
        launchPacketRef: {
          ...base.launchPacketRef,
          digest: `sha256:${sha256(canonicalJson(launchPacket))}` as never,
        },
      };

      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      )).rejects.toMatchObject({ code: "CAPABILITY_FORBIDDEN" });
      await expect(readFile(join(fixture.directory, request.launchPacketRef.path), "utf8"))
        .rejects.toMatchObject({ code: "ENOENT" });
      await expect(readFile(join(fixture.directory, request.resourcePlanRef.path), "utf8"))
        .rejects.toMatchObject({ code: "ENOENT" });
      const database = new Database(fixture.databasePath, { readonly: true });
      try {
        expect(database.prepare(`
          SELECT COUNT(*) AS count FROM project_session_launch_preparations
           WHERE command_id=?
        `).get(request.command.commandId)).toEqual({ count: 0 });
        expect(database.prepare(`
          SELECT state, revision FROM project_sessions WHERE project_session_id=?
        `).get(fixture.implementationSessionId)).toEqual({ state: "draft", revision: 1 });
      } finally {
        database.close();
      }
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it.each([
    "launch-preparation:after-resource-publish",
    "launch-preparation:after-launch-publish",
    "launch-preparation:before-transition",
  ])("compensates %s so a corrected edit can reuse the reviewed artifact paths", async (faultLabel) => {
    let armed = true;
    const fixture = await setupRoutingFixture((label) => {
      if (armed && label === faultLabel) throw new Error(`fault:${label}`);
    });
    try {
      await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        acceptedRequest(fixture),
      );
      const failed = implementationRequest(fixture, `command_${faultLabel.replaceAll(":", "_")}`);
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        failed,
      )).rejects.toThrow(`fault:${faultLabel}`);
      await expect(readFile(join(fixture.directory, failed.launchPacketRef.path), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
      await expect(readFile(join(fixture.directory, failed.resourcePlanRef.path), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
      const database = new Database(fixture.databasePath, { readonly: true });
      expect(database.prepare("SELECT state, revision FROM project_sessions WHERE project_session_id=?")
        .get(fixture.implementationSessionId)).toEqual({ state: "draft", revision: 1 });
      expect(database.prepare(`
        SELECT status FROM project_session_launch_preparations WHERE command_id=?
      `).get(failed.command.commandId)).toEqual({ status: "claimed" });
      database.close();

      armed = false;
      const corrected = implementationRequest(fixture, `corrected_${faultLabel.replaceAll(":", "_")}`, "Implement the corrected scope.");
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        corrected,
      )).resolves.toMatchObject({ projectSession: { state: "awaiting_launch" } });
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("returns the durable preparation result after the first response is lost", async () => {
    let loseResponse = true;
    const fixture = await setupRoutingFixture((label) => {
      if (loseResponse && label === "launch-preparation:after-commit") throw new Error("lost response");
    });
    try {
      await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        acceptedRequest(fixture),
      );
      const request = implementationRequest(fixture, "command_lost_implementation_response");
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      )).rejects.toThrow("lost response");
      loseResponse = false;
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      )).resolves.toMatchObject({
        projectSession: { state: "awaiting_launch", revision: 2 },
        launchPacketRef: request.launchPacketRef,
      });
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        implementationRequest(fixture, "command_lost_implementation_response", "Changed after confirmation."),
      )).rejects.toMatchObject({ code: "DEDUPE_CONFLICT" });
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("clears committed staging custody so later foreign files survive replay and restart", async () => {
    const fixture = await setupRoutingFixture();
    let closed = false;
    let reopened: Fabric | undefined;
    try {
      await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        acceptedRequest(fixture),
      );
      const request = implementationRequest(fixture, "command_clear_committed_staging");
      const prepared = await fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      );
      const suffix = sha256(`operator_routing:${request.command.commandId}`).slice(0, 16);
      const foreignPaths = [
        `${request.launchPacketRef.path}.prepare-${suffix}`,
        `${request.resourcePlanRef.path}.prepare-${suffix}`,
      ];
      for (const path of foreignPaths) await writeFile(join(fixture.directory, path), `foreign:${path}\n`);
      const database = new Database(fixture.databasePath, { readonly: true });
      try {
        expect(database.prepare(`
          SELECT staged_launch_packet_path, staged_resource_plan_path
            FROM project_session_launch_preparations WHERE command_id=?
        `).get(request.command.commandId)).toEqual({
          staged_launch_packet_path: null,
          staged_resource_plan_path: null,
        });
      } finally {
        database.close();
      }

      await fixture.fabric.close();
      closed = true;
      reopened = await openFabric({ databasePath: fixture.databasePath, workspaceRoots: [fixture.directory], clock: () => now });
      await expect(reopened.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      )).resolves.toEqual(prepared);
      for (const path of foreignPaths) {
        await expect(readFile(join(fixture.directory, path), "utf8")).resolves.toBe(`foreign:${path}\n`);
      }
    } finally {
      if (!closed) await fixture.fabric.close();
      if (reopened !== undefined) await reopened.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("preserves same-byte foreign files found at exclusively claimed staging paths", async () => {
    let request: ProjectSessionLaunchPacketPrepareRequest | undefined;
    let foreignPaths: readonly [string, string] | undefined;
    let fixture: RoutingFixture;
    fixture = await setupRoutingFixture((label) => {
      if (label !== "launch-preparation:before-stage-write" || request === undefined) return;
      const database = new Database(fixture.databasePath, { readonly: true });
      const row = database.prepare(`
        SELECT staged_launch_packet_path, staged_resource_plan_path
          FROM project_session_launch_preparations WHERE command_id=?
      `).get(request.command.commandId) as {
        staged_launch_packet_path: string;
        staged_resource_plan_path: string;
      };
      database.close();
      foreignPaths = [row.staged_launch_packet_path, row.staged_resource_plan_path];
      writeFileSync(join(fixture.directory, row.staged_launch_packet_path), canonicalJson(request.launchPacket));
      writeFileSync(join(fixture.directory, row.staged_resource_plan_path), canonicalJson(request.resourcePlan));
    });
    try {
      await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        acceptedRequest(fixture),
      );
      request = implementationRequest(fixture, "command_same_byte_foreign_staging");
      await mkdir(join(fixture.directory, ".agent-run/run_implementation_target"), { recursive: true });

      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      )).rejects.toMatchObject({ code: "DEDUPE_CONFLICT" });
      expect(foreignPaths).toBeDefined();
      if (foreignPaths === undefined) throw new Error("foreign staging paths were not captured");
      await expect(readFile(join(fixture.directory, foreignPaths[0]), "utf8"))
        .rejects.toMatchObject({ code: "ENOENT" });
      await expect(readFile(join(fixture.directory, foreignPaths[1]), "utf8"))
        .rejects.toMatchObject({ code: "ENOENT" });
      const quarantined = await quarantinedLaunchPreparationContents(fixture.directory);
      expect(quarantined).toContain(canonicalJson(request.launchPacket));
      expect(quarantined).toContain(canonicalJson(request.resourcePlan));
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("replays exactly after crashing between exclusive stage creation and identity persistence", async () => {
    let interruptStageIdentity = true;
    let orphanPath: string | undefined;
    let fixture: RoutingFixture;
    fixture = await setupRoutingFixture((label) => {
      if (!interruptStageIdentity || label !== "launch-preparation:after-resource-stage-write") return;
      const database = new Database(fixture.databasePath, { readonly: true });
      const row = database.prepare(`
        SELECT staged_resource_plan_path FROM project_session_launch_preparations
         WHERE command_id='command_stage_identity_crash'
      `).get() as { staged_resource_plan_path: string };
      database.close();
      orphanPath = row.staged_resource_plan_path;
      throw new Error("stage identity persistence interrupted");
    });
    try {
      await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        acceptedRequest(fixture),
      );
      const request = implementationRequest(fixture, "command_stage_identity_crash");
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      )).rejects.toThrow("stage identity persistence interrupted");
      expect(orphanPath).toBeDefined();
      if (orphanPath === undefined) throw new Error("orphan staging path was not captured");
      await expect(readFile(join(fixture.directory, orphanPath), "utf8"))
        .rejects.toMatchObject({ code: "ENOENT" });
      expect(await quarantinedLaunchPreparationContents(fixture.directory))
        .toContain(canonicalJson(request.resourcePlan));

      interruptStageIdentity = false;
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      )).resolves.toMatchObject({ projectSession: { state: "awaiting_launch" } });
      expect(await quarantinedLaunchPreparationContents(fixture.directory))
        .toContain(canonicalJson(request.resourcePlan));
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("preserves same-byte foreign stage replacements during committed restart recovery", async () => {
    let interruptCleanup = true;
    const fixture = await setupRoutingFixture((label) => {
      if (interruptCleanup && label === "launch-preparation:before-committed-stage-cleanup") {
        throw new Error("committed cleanup interrupted");
      }
    });
    let closed = false;
    let reopened: Fabric | undefined;
    try {
      await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        acceptedRequest(fixture),
      );
      const request = implementationRequest(fixture, "command_same_byte_committed_recovery");
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      )).rejects.toThrow("committed cleanup interrupted");
      const custodyDatabase = new Database(fixture.databasePath, { readonly: true });
      const custody = custodyDatabase.prepare(`
        SELECT staged_launch_packet_path, staged_resource_plan_path
          FROM project_session_launch_preparations WHERE command_id=?
      `).get(request.command.commandId) as {
        staged_launch_packet_path: string;
        staged_resource_plan_path: string;
      };
      custodyDatabase.close();
      const replacements = [
        [custody.staged_launch_packet_path, canonicalJson(request.launchPacket)],
        [custody.staged_resource_plan_path, canonicalJson(request.resourcePlan)],
      ] as const;
      await fixture.fabric.close();
      closed = true;
      interruptCleanup = false;
      let swapIndex = 0;
      reopened = await openFabric({
        databasePath: fixture.databasePath,
        workspaceRoots: [fixture.directory],
        clock: () => now,
        fault: (label) => {
          if (label !== "launch-preparation:before-committed-stage-quarantine") return;
          const replacement = replacements[swapIndex];
          if (replacement === undefined) return;
          swapIndex += 1;
          const [path, content] = replacement;
          renameSync(join(fixture.directory, path), join(fixture.directory, `${path}.owned-backup`));
          writeFileSync(join(fixture.directory, path), content);
        },
      });
      for (const [path, content] of replacements) {
        await expect(readFile(join(fixture.directory, path), "utf8")).rejects.toMatchObject({ code: "ENOENT" });
        expect(await quarantinedLaunchPreparationContents(fixture.directory)).toContain(content);
      }
      const database = new Database(fixture.databasePath, { readonly: true });
      try {
        expect(database.prepare(`
          SELECT staged_launch_packet_path, staged_resource_plan_path,
                 staged_launch_packet_device, staged_launch_packet_inode,
                 staged_resource_plan_device, staged_resource_plan_inode
            FROM project_session_launch_preparations WHERE command_id=?
        `).get(request.command.commandId)).toEqual({
          staged_launch_packet_path: null,
          staged_resource_plan_path: null,
          staged_launch_packet_device: null,
          staged_launch_packet_inode: null,
          staged_resource_plan_device: null,
          staged_resource_plan_inode: null,
        });
      } finally {
        database.close();
      }
      await expect(reopened.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      )).resolves.toMatchObject({ projectSession: { state: "awaiting_launch" } });
    } finally {
      if (!closed) await fixture.fabric.close();
      if (reopened !== undefined) await reopened.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("quarantines a same-byte foreign stage swap during compensation", async () => {
    let request: ProjectSessionLaunchPacketPrepareRequest | undefined;
    let stagePath: string | undefined;
    let compensationStarted = false;
    let swapped = false;
    let fixture: RoutingFixture;
    fixture = await setupRoutingFixture((label) => {
      if (label === "launch-preparation:after-resource-publish") {
        if (request === undefined) return;
        const database = new Database(fixture.databasePath, { readonly: true });
        const row = database.prepare(`
          SELECT staged_launch_packet_path FROM project_session_launch_preparations WHERE command_id=?
        `).get(request.command.commandId) as { staged_launch_packet_path: string };
        database.close();
        stagePath = row.staged_launch_packet_path;
        compensationStarted = true;
        throw new Error("begin compensation swap");
      }
      if (
        label === "launch-preparation:before-compensation-stage-quarantine" &&
        compensationStarted && !swapped && stagePath !== undefined && request !== undefined
      ) {
        swapped = true;
        renameSync(join(fixture.directory, stagePath), join(fixture.directory, `${stagePath}.owned-backup`));
        writeFileSync(join(fixture.directory, stagePath), canonicalJson(request.launchPacket));
      }
    });
    try {
      await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        acceptedRequest(fixture),
      );
      request = implementationRequest(fixture, "command_compensation_quarantine_swap");
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      )).rejects.toMatchObject({ code: "DEDUPE_CONFLICT" });
      expect(swapped).toBe(true);
      expect(await quarantinedLaunchPreparationContents(fixture.directory))
        .toContain(canonicalJson(request.launchPacket));
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("cleans staging custody for artifact names at the filesystem component limit", async () => {
    const fixture = await setupRoutingFixture();
    try {
      await fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        acceptedRequest(fixture),
      );
      const request = implementationRequest(
        fixture,
        "command_near_name_max_cleanup",
        "Implement accepted routing scope.",
        true,
      );

      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.implementationContext,
        FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
        request,
      )).resolves.toMatchObject({ projectSession: { state: "awaiting_launch" } });
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it.each(["resource-only", "both-artifacts"])(
    "recovers %s publication custody after daemon restart",
    async (published) => {
      const fixture = await setupRoutingFixture();
      let initialClosed = false;
      let reopened: Fabric | undefined;
      try {
        await fixture.fabric.dispatchPublicProtocol(
          fixture.operatorContext,
          FABRIC_OPERATIONS.intakeRevise,
          acceptedRequest(fixture),
        );
        const interrupted = implementationRequest(fixture, `interrupted_${published}`);
        await mkdir(join(fixture.directory, ".agent-run/run_implementation_target"), { recursive: true });
        const stagedResourcePlanPath = `${interrupted.resourcePlanRef.path}.prepare-crash`;
        const stagedLaunchPacketPath = `${interrupted.launchPacketRef.path}.prepare-crash`;
        await writeFile(
          join(fixture.directory, stagedResourcePlanPath),
          canonicalJson(interrupted.resourcePlan),
        );
        await link(
          join(fixture.directory, stagedResourcePlanPath),
          join(fixture.directory, interrupted.resourcePlanRef.path),
        );
        if (published === "both-artifacts") {
          await writeFile(
            join(fixture.directory, stagedLaunchPacketPath),
            canonicalJson(interrupted.launchPacket),
          );
          await link(
            join(fixture.directory, stagedLaunchPacketPath),
            join(fixture.directory, interrupted.launchPacketRef.path),
          );
        }
        const stagedResourceIdentity = await stat(join(fixture.directory, stagedResourcePlanPath), { bigint: true });
        const stagedLaunchIdentity = published === "both-artifacts"
          ? await stat(join(fixture.directory, stagedLaunchPacketPath), { bigint: true })
          : null;
        await fixture.fabric.close();
        initialClosed = true;
        const database = new Database(fixture.databasePath);
        database.prepare(`
          INSERT INTO project_session_launch_preparations(
            operator_id, command_id, capability_id, project_id, project_session_id,
            session_generation, payload_hash, status, launch_packet_path,
            launch_packet_digest, resource_plan_path, resource_plan_digest,
            staged_launch_packet_path, staged_resource_plan_path,
            staged_launch_packet_device, staged_launch_packet_inode,
            staged_resource_plan_device, staged_resource_plan_inode,
            created_at, updated_at
          ) VALUES (
            'operator_routing', ?, 'cap_implementation_routing', ?, ?, 1, ?, 'staged',
            ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?
          )
        `).run(
          interrupted.command.commandId,
          fixture.projectId,
          fixture.implementationSessionId,
          digestA,
          interrupted.launchPacketRef.path,
          interrupted.launchPacketRef.digest,
          interrupted.resourcePlanRef.path,
          interrupted.resourcePlanRef.digest,
          published === "both-artifacts" ? stagedLaunchPacketPath : null,
          stagedResourcePlanPath,
          stagedLaunchIdentity?.dev.toString() ?? null,
          stagedLaunchIdentity?.ino.toString() ?? null,
          stagedResourceIdentity.dev.toString(),
          stagedResourceIdentity.ino.toString(),
          now,
          now,
        );
        database.close();

        reopened = await openFabric({
          databasePath: fixture.databasePath,
          workspaceRoots: [fixture.directory],
          clock: () => now,
        });
        await expect(readFile(join(fixture.directory, interrupted.launchPacketRef.path), "utf8"))
          .rejects.toMatchObject({ code: "ENOENT" });
        await expect(readFile(join(fixture.directory, interrupted.resourcePlanRef.path), "utf8"))
          .rejects.toMatchObject({ code: "ENOENT" });
        const recoveredDatabase = new Database(fixture.databasePath, { readonly: true });
        expect(recoveredDatabase.prepare(`
          SELECT status FROM project_session_launch_preparations WHERE command_id=?
        `).get(interrupted.command.commandId)).toEqual({ status: "claimed" });
        recoveredDatabase.close();

        const corrected = implementationRequest(fixture, `restart_corrected_${published}`, "Implement after restart recovery.");
        await expect(reopened.dispatchPublicProtocol(
          fixture.implementationContext,
          FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare,
          corrected,
        )).resolves.toMatchObject({ projectSession: { state: "awaiting_launch" } });
      } finally {
        if (!initialClosed) await fixture.fabric.close();
        if (reopened !== undefined) await reopened.close();
        await rm(fixture.directory, { recursive: true, force: true });
      }
    },
  );

  it("rejects an unregistered project file atomically instead of registering during intake binding", async () => {
    const fixture = await setupRoutingFixture();
    try {
      const content = "unregistered but present\n";
      const digest = `sha256:${createHash("sha256").update(content).digest("hex")}`;
      await writeFile(join(fixture.directory, "unregistered.md"), content);
      const base = operatorRequest(fixture);
      const request = parseIntakeRevisionRequest({
        ...base,
        command: {
          ...base.command,
          commandId: "command_intake_unregistered",
          evidenceRefs: [{ path: "unregistered.md", digest }],
        },
        artifactRefs: [{ path: "unregistered.md", digest }],
      });
      await expect(fixture.fabric.dispatchPublicProtocol(
        fixture.operatorContext,
        FABRIC_OPERATIONS.intakeRevise,
        request,
      )).rejects.toMatchObject({ code: "NOT_FOUND" });
      const database = new Database(fixture.databasePath, { readonly: true });
      try {
        expect(database.prepare("SELECT revision FROM intakes WHERE intake_id='intake_routing'").get())
          .toEqual({ revision: 2 });
        expect(database.prepare("SELECT COUNT(*) AS count FROM artifacts WHERE relative_path='unregistered.md'").get())
          .toEqual({ count: 0 });
      } finally {
        database.close();
      }
    } finally {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });
});
