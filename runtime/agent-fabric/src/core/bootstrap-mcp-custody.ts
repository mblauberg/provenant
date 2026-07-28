import { createHash, createHmac } from "node:crypto";
import type Database from "better-sqlite3";

import { FABRIC_OPERATIONS } from "../domain/operations.js";
import type { AuthorityInput } from "../domain/types.js";
import { FabricError } from "../errors.js";
import { currentMcpSeatGeneration } from "./mcp-seat-generation.js";
import type { BootstrapMcpSeatInput, BootstrapMcpSeatResult, CurrentMcpSeatBindingInput, CurrentMcpSeatBindingResult } from "./contracts.js";

type Row = Record<string, unknown>;

const BOOTSTRAP_AUTHORITY_LIFETIME_MS = 365 * 24 * 60 * 60 * 1_000;
const BOOTSTRAP_SEAT_LIFETIME_MS = 24 * 60 * 60 * 1_000;
const MCP_SEAT_RENEWAL_WINDOW_MS = 60 * 60 * 1_000;

export type BootstrapMcpCustody = {
  database: Database.Database;
  clock: () => number;
  workspaceRoots: readonly string[];
  capabilityKey: string;
  canonicalWorkspaceRoot: (root: string) => string;
  normaliseAuthority: (authority: AuthorityInput, workspaceRoot: string) => AuthorityInput;
  bindCurrentMcpSeats: (input: CurrentMcpSeatBindingInput) => CurrentMcpSeatBindingResult;
};

function isRow(value: unknown): value is Row {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function rowOrNotFound(value: unknown, label: string): Row {
  if (!isRow(value)) throw new FabricError("NOT_FOUND", label + " was not found");
  return value;
}

function stringField(row: Row, field: string): string {
  const value = row[field];
  if (typeof value !== "string") throw new Error("database field " + field + " is not text");
  return value;
}

function numberField(row: Row, field: string): number {
  const value = row[field];
  if (typeof value !== "number" || !Number.isSafeInteger(value)) throw new Error("database field " + field + " is not an integer");
  return value;
}

function authorityExpiry(authorityJson: string): number {
  const value: unknown = JSON.parse(authorityJson);
  if (!isRow(value) || typeof value.expiresAt !== "string") {
    throw new Error("bootstrap authority expiry is invalid");
  }
  const expiresAt = Date.parse(value.expiresAt);
  if (!Number.isFinite(expiresAt)) throw new Error("bootstrap authority expiry is invalid");
  return expiresAt;
}

function canonicalJson(value: unknown): string {
  if (value === null || typeof value === "boolean" || typeof value === "number" || typeof value === "string") return JSON.stringify(value);
  if (Array.isArray(value)) return "[" + value.map(canonicalJson).join(",") + "]";
  if (typeof value === "object") {
    const record = value as Record<string, unknown>;
    return "{" + Object.keys(record).sort().map((key) => JSON.stringify(key) + ":" + canonicalJson(record[key])).join(",") + "}";
  }
  throw new TypeError("value is not JSON-compatible");
}

function sha256(value: string): string { return createHash("sha256").update(value).digest("hex"); }
function sha256Digest(value: string): string { return "sha256:" + sha256(value); }
function capabilityToken(key: string, runId: string, agentId: string, generation: number): string {
  return "afc_" + createHmac("sha256", key).update(canonicalJson({ runId, agentId, principalGeneration: generation })).digest("base64url");
}

export function bootstrapCurrentMcpSeat(custody: BootstrapMcpCustody, input: BootstrapMcpSeatInput): BootstrapMcpSeatResult {
    const canonicalRoot = custody.canonicalWorkspaceRoot(input.canonicalRoot);
    if (canonicalRoot !== input.canonicalRoot || !custody.workspaceRoots.includes(canonicalRoot)) {
      throw new FabricError("AUTHORITY_WIDENING", "bootstrap requires the exact configured project root");
    }
    if (!/^sha256:[0-9a-f]{64}$/u.test(input.trustRecordDigest)) {
      throw new FabricError("AUTHENTICATION_FAILED", "bootstrap trust record digest is invalid");
    }
    if (input.seat !== "claude" && input.seat !== "codex") {
      throw new FabricError(
        "AUTHENTICATION_FAILED",
        `bootstrap creates only chair seats claude or codex; run agent-fabric mcp peer-provision ` +
        `--project ${input.canonicalRoot} --seat ${input.seat} instead`,
      );
    }
    const requestedExpiry = Date.parse(input.expiresAt);
    const validatedAt = custody.clock();
    if (
      !Number.isFinite(requestedExpiry) ||
      requestedExpiry <= validatedAt ||
      requestedExpiry > validatedAt + BOOTSTRAP_SEAT_LIFETIME_MS
    ) {
      throw new FabricError(
        "AUTHENTICATION_FAILED",
        "bootstrap seat expiry is invalid, elapsed or exceeds the 24-hour bound",
      );
    }
    const identityDigest = sha256(canonicalJson({ kind: "mcp-zero-state-v1", canonicalRoot }));
    const projectId = `project:local:${sha256(canonicalJson({ canonicalRoot }))}`;
    const projectSessionId = `session_bootstrap_${identityDigest.slice(0, 32)}`;
    const runId = `run_bootstrap_${identityDigest.slice(0, 32)}`;
    const initialChairSeat = input.seat;
    const chairAgentId = `${initialChairSeat}_bootstrap_chair_${identityDigest.slice(0, 16)}`;
    const chairLeaseId = `chair:${runId}:1`;
    const bootstrapRunDirectory = `.agent-run/bootstrap-${identityDigest.slice(0, 12)}`;

    return custody.database.transaction((): BootstrapMcpSeatResult => {
      const existingProject = custody.database.prepare(
        "SELECT project_id,trust_record_digest FROM projects WHERE canonical_root=?",
      ).get(canonicalRoot);
      const now = custody.clock();
      const bootstrapAuthorityExpiresAt = new Date(now + BOOTSTRAP_AUTHORITY_LIFETIME_MS).toISOString();
      if (existingProject === undefined) {
        custody.database.prepare(`
          INSERT INTO projects(project_id,canonical_root,trust_record_digest,revision,authority_generation,created_at,updated_at)
          VALUES (?,?,?,1,1,?,?)
        `).run(projectId, canonicalRoot, input.trustRecordDigest, now, now);
      } else if (!isRow(existingProject) || existingProject.project_id !== projectId || existingProject.trust_record_digest !== input.trustRecordDigest) {
        throw new FabricError("DEDUPE_CONFLICT", "bootstrap project identity conflicts with stored trust custody");
      }

      const existingSession = custody.database.prepare(
        "SELECT project_session_id FROM project_sessions WHERE project_id=?",
      ).all(projectId);
      if (existingSession.length > 0 && !existingSession.some((row) => isRow(row) && row.project_session_id === projectSessionId)) {
        throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "bootstrap is available only for a zero-state project");
      }
      if (existingSession.length === 0) {
        const authority = custody.normaliseAuthority({
          schemaVersion: 2,
          approval: {
            approvedBy: "agent-fabric-bootstrap",
            evidenceId: `trusted-workspace:${identityDigest}`,
            evidenceDigest: input.trustRecordDigest as `sha256:${string}`,
          },
          workspaceRoots: ["."],
          sourcePaths: ["."],
          artifactPaths: [bootstrapRunDirectory],
          actions: [
            FABRIC_OPERATIONS.delegateAuthority,
            FABRIC_OPERATIONS.registerAgent,
            FABRIC_OPERATIONS.sendMessage,
            FABRIC_OPERATIONS.createDiscussionGroup,
            FABRIC_OPERATIONS.receiveMessages,
            FABRIC_OPERATIONS.acknowledgeDelivery,
            FABRIC_OPERATIONS.getMailboxState,
            FABRIC_OPERATIONS.createTask,
            FABRIC_OPERATIONS.getTask,
            FABRIC_OPERATIONS.createTeam,
            FABRIC_OPERATIONS.getTeam,
            FABRIC_OPERATIONS.whoami,
            FABRIC_OPERATIONS.getRunStatus,
            FABRIC_OPERATIONS.listTasks,
            FABRIC_OPERATIONS.listAgents,
            FABRIC_OPERATIONS.listReceipts,
            FABRIC_OPERATIONS.evidencePublish,
          ],
          deniedPaths: [],
          deniedActions: [],
          prohibitedActions: ["secrets", "deployment", "irreversible-actions", "tool-egress"],
          disclosure: { level: "forbidden" },
          secrets: { access: "none" },
          deployment: { allowed: false },
          irreversibleActions: { allowed: false },
          network: { toolEgress: "none" },
          // The narrow authority remains bounded but intentionally outlives its
          // short-lived bearer seats so exact-trust renewal needs no authority
          // mutation or lifecycle reconstruction.
          expiresAt: bootstrapAuthorityExpiresAt,
          budget: {},
        }, canonicalRoot);
        const authorityJson = canonicalJson(authority);
        const authorityRef = sha256Digest(authorityJson);
        const authorityId = `bootstrap-authority:${identityDigest}:${initialChairSeat}`;
        const packetDigest = sha256Digest(canonicalJson({ kind: "mcp-zero-state-v1", projectId, projectSessionId, runId }));
        const operatorId = `operator:bootstrap:${identityDigest}`;
        custody.database.prepare(`
          INSERT INTO project_sessions(
            project_session_id,project_id,mode,state,revision,generation,authority_ref,budget_ref,
            launch_packet_path,launch_packet_digest,membership_revision,origin_kind,origin_operator_id,created_at,updated_at
          ) VALUES (?,?,'coordinated','active',1,1,?,? ,?,?,1,'operator-launch',?,?,?)
        `).run(projectSessionId, projectId, authorityRef, `bootstrap-budget:${identityDigest}`, `${bootstrapRunDirectory}/launch-packet.json`, packetDigest, operatorId, now, now);
        custody.database.prepare(`
          INSERT INTO runs(
            run_id,chair_agent_id,workspace_root,project_run_directory,created_at,project_session_id,
            lifecycle_state,revision,chair_generation,chair_lease_id,authority_ref,budget_ref,dependency_revision,
            topology_slot,project_run_directory_basis
          ) VALUES (?,?,?,?,?,?,'active',1,1,?,?,?,1,1,'project-relative')
        `).run(runId, chairAgentId, canonicalRoot, bootstrapRunDirectory, now, projectSessionId, chairLeaseId, authorityRef, `bootstrap-budget:${identityDigest}`);
        custody.database.prepare(`
          INSERT INTO authorities(authority_id,run_id,parent_authority_id,authority_json,authority_hash,created_at)
          VALUES (?,?,NULL,?,?,?)
        `).run(authorityId, runId, authorityJson, sha256(authorityJson), now);
        custody.database.prepare(`
          INSERT INTO agents(run_id,agent_id,parent_agent_id,authority_id,provider_session_ref,lifecycle)
          VALUES (?,?,NULL,?,NULL,'ready')
        `).run(runId, chairAgentId, authorityId);
        custody.database.prepare("INSERT INTO mailbox_state(run_id,recipient_id) VALUES (?,?)").run(runId, chairAgentId);
        const initialCapability = capabilityToken(custody.capabilityKey, runId, chairAgentId, 1);
        custody.database.prepare(`
          INSERT INTO capabilities(token_hash,run_id,agent_id,principal_generation,expires_at)
          VALUES (?,?,?,1,?)
        `).run(sha256(initialCapability), runId, chairAgentId, Date.parse(bootstrapAuthorityExpiresAt));
        custody.database.prepare(`
          INSERT INTO run_chair_leases(project_session_id,run_id,lease_id,holder_agent_id,generation,status,updated_at)
          VALUES (?,?,?,?,1,'active',?)
        `).run(projectSessionId, runId, chairLeaseId, chairAgentId, now);
        custody.database.prepare(`
          INSERT INTO project_session_memberships(
            project_session_id,coordination_run_id,member_kind,member_id,member_adapter_id,required,state,revision,created_at,updated_at
          ) VALUES (?,?, 'coordination-run',?,'',1,'active',1,?,?), (?,?, 'lease',?,'',1,'active',1,?,?)
        `).run(projectSessionId, runId, runId, now, now, projectSessionId, runId, chairLeaseId, now, now);
        custody.database.prepare("INSERT INTO run_metadata(run_id,execution_profile) VALUES (?,'headless')").run(runId);
        custody.database.prepare(`
          INSERT INTO run_authority_revisions(
            project_session_id,coordination_run_id,authority_revision,authority_ref,git_allowlist_epoch,git_allowlist_digest,activated_at_run_revision,created_at
          ) VALUES (?,?,1,?,1,NULL,1,?)
        `).run(projectSessionId, runId, authorityRef, now);
      }

      const run = rowOrNotFound(custody.database.prepare(`
        SELECT session.revision AS session_revision,session.generation AS session_generation,
               run.revision AS run_revision,run.chair_agent_id,run.chair_generation,run.chair_lease_id
          FROM project_sessions session JOIN runs run ON run.project_session_id=session.project_session_id
         WHERE session.project_session_id=? AND run.run_id=?
      `).get(projectSessionId, runId), "bootstrap run");
      const active = custody.database.prepare(`
        SELECT active.generation,generation.previous_generation,generation.expires_at,
               generation.project_session_id,generation.session_revision,generation.session_generation,
               generation.run_id,generation.run_revision,generation.chair_agent_id,
               generation.chair_generation,generation.chair_lease_id
          FROM mcp_active_seat_generations active JOIN mcp_seat_generations generation ON generation.generation=active.generation
         WHERE active.project_id=?
      `).get(projectId);
      const currentChairAgentId = stringField(run, "chair_agent_id");
      const chairSeat = currentChairAgentId.startsWith("codex_") ? "codex" : "claude";
      const peerSeat = chairSeat === "codex" ? "claude" : "codex";
      const peerAgentId = `${peerSeat}_bootstrap_peer_${identityDigest.slice(0, 16)}`;
      let activeGenerationNeedsRenewal = false;
      if (isRow(active)) {
        activeGenerationNeedsRenewal =
          numberField(active, "expires_at") - now <= MCP_SEAT_RENEWAL_WINDOW_MS;
        const member = custody.database.prepare(
          "SELECT 1 FROM mcp_seat_generation_members WHERE generation=? AND seat=?",
        ).get(active.generation, input.seat);
        if (member !== undefined && !activeGenerationNeedsRenewal) {
          const generation = stringField(active, "generation");
          const storedProjectSessionId = stringField(active, "project_session_id");
          const storedRunId = stringField(active, "run_id");
          const storedChairAgentId = stringField(active, "chair_agent_id");
          const bindings = custody.database.prepare(`
            SELECT seat,agent_id AS agentId,principal_generation AS expectedPrincipalGeneration
              FROM mcp_seat_generation_members WHERE generation=? ORDER BY seat
          `).all(generation) as CurrentMcpSeatBindingInput["bindings"];
          const expiresAt = new Date(numberField(active, "expires_at")).toISOString();
          const sessionRevision = numberField(active, "session_revision");
          const sessionGeneration = numberField(active, "session_generation");
          const runRevision = numberField(active, "run_revision");
          const chairGeneration = numberField(active, "chair_generation");
          const storedChairLeaseId = stringField(active, "chair_lease_id");
          const credentials = bindings.map((binding) => ({
            ...binding,
            authorityId: stringField(rowOrNotFound(custody.database.prepare(
              "SELECT authority_id FROM agents WHERE run_id=? AND agent_id=?",
            ).get(storedRunId, binding.agentId), "bootstrap seat authority"), "authority_id"),
            capability: `afc_${createHmac("sha256", custody.capabilityKey)
              .update(canonicalJson({
                kind: "current-mcp-seat",
                canonicalRoot,
                projectSessionId: storedProjectSessionId,
                sessionRevision,
                sessionGeneration,
                runId: storedRunId,
                runRevision,
                chairAgentId: storedChairAgentId,
                chairGeneration,
                chairLeaseId: storedChairLeaseId,
                generation,
                expiresAt,
                ...binding,
              }))
              .digest("base64url")}`,
          }));
          return {
            expectedPreviousGeneration: active.previous_generation === null
              ? null
              : stringField(active, "previous_generation"),
            generation,
            projectSessionId: storedProjectSessionId,
            sessionRevision,
            sessionGeneration,
            runId: storedRunId,
            runRevision,
            chairAgentId: storedChairAgentId,
            chairGeneration,
            chairLeaseId: storedChairLeaseId,
            expiresAt,
            credentials,
            projectId,
            canonicalRoot,
            bootstrapRunDirectory,
          };
        }
      }

      const requestedAgentId = input.seat === chairSeat
        ? currentChairAgentId
        : peerAgentId;
      if (requestedAgentId !== currentChairAgentId && custody.database.prepare(
        "SELECT 1 FROM agents WHERE run_id=? AND agent_id=?",
      ).get(runId, requestedAgentId) === undefined) {
        const chairAuthority = rowOrNotFound(custody.database.prepare(
          "SELECT authority_id,authority_json,authority_hash FROM authorities WHERE run_id=? AND parent_authority_id IS NULL",
        ).get(runId), "bootstrap chair authority");
        const peerAuthorityId = `bootstrap-authority:${identityDigest}:${input.seat}`;
        custody.database.prepare(`
          INSERT INTO authorities(authority_id,run_id,parent_authority_id,authority_json,authority_hash,created_at)
          VALUES (?,?,?,?,?,?)
        `).run(peerAuthorityId, runId, chairAuthority.authority_id, chairAuthority.authority_json, chairAuthority.authority_hash, now);
        custody.database.prepare(`
          INSERT INTO agents(run_id,agent_id,parent_agent_id,authority_id,provider_session_ref,lifecycle)
          VALUES (?,?,?,?,NULL,'ready')
        `).run(runId, requestedAgentId, currentChairAgentId, peerAuthorityId);
        custody.database.prepare("INSERT INTO mailbox_state(run_id,recipient_id) VALUES (?,?)").run(runId, requestedAgentId);
        const token = capabilityToken(custody.capabilityKey, runId, requestedAgentId, 1);
        custody.database.prepare(`
          INSERT INTO capabilities(token_hash,run_id,agent_id,principal_generation,expires_at)
          VALUES (?,?,?,1,?)
        `).run(
          sha256(token),
          runId,
          requestedAgentId,
          authorityExpiry(stringField(chairAuthority, "authority_json")),
        );
      }

      const bindings = custody.database.prepare(`
        SELECT CASE WHEN agent_id=? THEN ? ELSE ? END AS seat,agent_id AS agentId,1 AS expectedPrincipalGeneration
          FROM agents WHERE run_id=? AND agent_id IN (?,?) ORDER BY seat
      `).all(currentChairAgentId, chairSeat, peerSeat, runId, currentChairAgentId, peerAgentId) as CurrentMcpSeatBindingInput["bindings"];
      const expiresAt = isRow(active) && !activeGenerationNeedsRenewal
        ? new Date(numberField(active, "expires_at")).toISOString()
        : input.expiresAt;
      const expectedPreviousGeneration = isRow(active) ? stringField(active, "generation") : null;
      const generationIdentity = currentMcpSeatGeneration({
        canonicalRoot,
        projectSessionId,
        sessionRevision: numberField(run, "session_revision"),
        sessionGeneration: numberField(run, "session_generation"),
        runId,
        runRevision: numberField(run, "run_revision"),
        chairAgentId: stringField(run, "chair_agent_id"),
        chairGeneration: numberField(run, "chair_generation"),
        chairLeaseId: stringField(run, "chair_lease_id"),
        expiresAt,
        bindings,
      });
      const bound = custody.bindCurrentMcpSeats({
        canonicalRoot,
        expectedPreviousGeneration,
        generation: generationIdentity.generation,
        projectSessionId,
        expectedSessionRevision: numberField(run, "session_revision"),
        expectedSessionGeneration: numberField(run, "session_generation"),
        runId,
        expectedRunRevision: numberField(run, "run_revision"),
        chairAgentId: stringField(run, "chair_agent_id"),
        expectedChairGeneration: numberField(run, "chair_generation"),
        chairLeaseId: stringField(run, "chair_lease_id"),
        expiresAt,
        bindings,
      });
      const credentials = bound.credentials.map((credential) => ({
        ...credential,
        authorityId: stringField(rowOrNotFound(custody.database.prepare(
          "SELECT authority_id FROM agents WHERE run_id=? AND agent_id=?",
        ).get(bound.runId, credential.agentId), "bootstrap seat authority"), "authority_id"),
      }));
      return { ...bound, credentials, projectId, canonicalRoot, bootstrapRunDirectory };
    }).immediate();
}
