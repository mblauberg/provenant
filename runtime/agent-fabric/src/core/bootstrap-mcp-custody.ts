import { createHash, createHmac } from "node:crypto";
import type Database from "better-sqlite3";

import { FABRIC_OPERATIONS, type FabricOperation } from "../domain/operations.js";
import type { AuthorityInput } from "../domain/types.js";
import { FabricError } from "../errors.js";
import { fabricCliCommand } from "../domain/fabric-roots.js";
import { currentMcpSeatGeneration } from "./mcp-seat-generation.js";
import type { BootstrapMcpSeatInput, BootstrapMcpSeatResult, CurrentMcpSeatBindingInput, CurrentMcpSeatBindingResult } from "./contracts.js";

type Row = Record<string, unknown>;

const BOOTSTRAP_AUTHORITY_LIFETIME_MS = 365 * 24 * 60 * 60 * 1_000;
const BOOTSTRAP_SEAT_LIFETIME_MS = 24 * 60 * 60 * 1_000;
const MCP_SEAT_RENEWAL_WINDOW_MS = 60 * 60 * 1_000;
const BOOTSTRAP_AUTHORITY_LEGACY_ACTIONS: readonly FabricOperation[] = Object.freeze([
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
].sort());
const BOOTSTRAP_AUTHORITY_ACTIONS: readonly FabricOperation[] = Object.freeze([
  ...BOOTSTRAP_AUTHORITY_LEGACY_ACTIONS,
  FABRIC_OPERATIONS.taskRequest,
  FABRIC_OPERATIONS.taskCompleteWithReply,
].sort());
const BOOTSTRAP_AUTHORITY_UPGRADE_V1_LEGACY_ACTIONS_HASH = sha256(canonicalJson(BOOTSTRAP_AUTHORITY_LEGACY_ACTIONS));
const BOOTSTRAP_AUTHORITY_UPGRADE_V1_ACTIONS_HASH = sha256(canonicalJson(BOOTSTRAP_AUTHORITY_ACTIONS));

export type BootstrapMcpCustody = {
  database: Database.Database;
  clock: () => number;
  productRoot: string;
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
function shellQuote(value: string): string { return `'${value.replaceAll("'", `'"'"'`)}'`; }
function capabilityToken(key: string, runId: string, agentId: string, generation: number): string {
  return "afc_" + createHmac("sha256", key).update(canonicalJson({ runId, agentId, principalGeneration: generation })).digest("base64url");
}

function bootstrapAuthorityId(identityDigest: string, seat: "claude" | "codex"): string {
  return `bootstrap-authority:${identityDigest}:${seat}`;
}

function bootstrapAuthorityInput(
  custody: BootstrapMcpCustody,
  canonicalRoot: string,
  identityDigest: string,
  trustRecordDigest: string,
  bootstrapRunDirectory: string,
  expiresAt: string,
  actions: readonly FabricOperation[],
): AuthorityInput {
  return custody.normaliseAuthority({
    schemaVersion: 2,
    approval: {
      approvedBy: "agent-fabric-bootstrap",
      evidenceId: `trusted-workspace:${identityDigest}`,
      evidenceDigest: trustRecordDigest as `sha256:${string}`,
    },
    workspaceRoots: ["."],
    sourcePaths: ["."],
    artifactPaths: [bootstrapRunDirectory],
    actions: [...actions],
    deniedPaths: [],
    deniedActions: [],
    prohibitedActions: ["secrets", "deployment", "irreversible-actions", "tool-egress"],
    disclosure: { level: "forbidden" },
    secrets: { access: "none" },
    deployment: { allowed: false },
    irreversibleActions: { allowed: false },
    network: { toolEgress: "none" },
    expiresAt,
    budget: {},
  }, canonicalRoot);
}

function bootstrapAuthorityMigrationError(message: string): FabricError {
  return new FabricError("AUTHORITY_WIDENING", `bootstrap authority upgrade refused: ${message}`);
}

function migrateBootstrapAuthority(input: {
  custody: BootstrapMcpCustody;
  canonicalRoot: string;
  identityDigest: string;
  trustRecordDigest: string;
  bootstrapRunDirectory: string;
  runId: string;
  authorityId: string;
  parentAuthorityId: string | null;
  required: boolean;
}): boolean {
  const { custody } = input;
  const row = custody.database.prepare(`
    SELECT authority_id,run_id,parent_authority_id,authority_json,authority_hash
      FROM authorities
     WHERE authority_id=? AND run_id=?
  `).get(input.authorityId, input.runId);
  if (row === undefined) {
    if (input.required) throw new FabricError("NOT_FOUND", "bootstrap authority upgrade target was not found");
    return false;
  }
  if (!isRow(row) || stringField(row, "authority_id") !== input.authorityId || stringField(row, "run_id") !== input.runId) {
    throw bootstrapAuthorityMigrationError("authority identity is not canonical");
  }
  if (row.parent_authority_id !== input.parentAuthorityId) {
    throw bootstrapAuthorityMigrationError("authority parent is not the bootstrap parent");
  }
  const authorityJson = stringField(row, "authority_json");
  const authorityHash = stringField(row, "authority_hash");
  if (sha256(authorityJson) !== authorityHash) {
    throw bootstrapAuthorityMigrationError("stored authority hash does not match its canonical bytes");
  }

  let storedAuthority: unknown;
  try {
    storedAuthority = JSON.parse(authorityJson) as unknown;
  } catch (cause: unknown) {
    throw new FabricError("AUTHORITY_WIDENING", "bootstrap authority upgrade refused: stored authority JSON is invalid", { cause });
  }
  let normalisedStoredAuthority: AuthorityInput;
  try {
    normalisedStoredAuthority = custody.normaliseAuthority(storedAuthority as AuthorityInput, input.canonicalRoot);
  } catch (cause: unknown) {
    throw new FabricError("AUTHORITY_WIDENING", "bootstrap authority upgrade refused: stored authority is not normalisable", { cause });
  }
  if (canonicalJson(normalisedStoredAuthority) !== authorityJson) {
    throw bootstrapAuthorityMigrationError("stored authority is not canonical");
  }

  const legacyAuthority = bootstrapAuthorityInput(
    custody,
    input.canonicalRoot,
    input.identityDigest,
    input.trustRecordDigest,
    input.bootstrapRunDirectory,
    normalisedStoredAuthority.expiresAt,
    BOOTSTRAP_AUTHORITY_LEGACY_ACTIONS,
  );
  const currentAuthority = bootstrapAuthorityInput(
    custody,
    input.canonicalRoot,
    input.identityDigest,
    input.trustRecordDigest,
    input.bootstrapRunDirectory,
    normalisedStoredAuthority.expiresAt,
    BOOTSTRAP_AUTHORITY_ACTIONS,
  );
  const storedActionsHash = sha256(canonicalJson(normalisedStoredAuthority.actions));
  if (storedActionsHash !== BOOTSTRAP_AUTHORITY_UPGRADE_V1_ACTIONS_HASH &&
      storedActionsHash !== BOOTSTRAP_AUTHORITY_UPGRADE_V1_LEGACY_ACTIONS_HASH) {
    throw bootstrapAuthorityMigrationError("stored action set is not a recognised upgrade version");
  }
  if (canonicalJson(normalisedStoredAuthority) === canonicalJson(currentAuthority)) return false;
  if (canonicalJson(normalisedStoredAuthority) !== canonicalJson(legacyAuthority)) {
    throw bootstrapAuthorityMigrationError("stored authority is not the expected legacy bootstrap envelope");
  }

  const migratedJson = canonicalJson(currentAuthority);
  const migratedHash = sha256(migratedJson);
  const update = input.parentAuthorityId === null
    ? custody.database.prepare(`
        UPDATE authorities
           SET authority_json=?,authority_hash=?
         WHERE authority_id=? AND run_id=? AND parent_authority_id IS NULL
           AND authority_json=? AND authority_hash=?
      `).run(migratedJson, migratedHash, input.authorityId, input.runId, authorityJson, authorityHash)
    : custody.database.prepare(`
        UPDATE authorities
           SET authority_json=?,authority_hash=?
         WHERE authority_id=? AND run_id=? AND parent_authority_id=?
           AND authority_json=? AND authority_hash=?
      `).run(migratedJson, migratedHash, input.authorityId, input.runId, input.parentAuthorityId, authorityJson, authorityHash);
  if (update.changes !== 1) throw new FabricError("DEDUPE_CONFLICT", "bootstrap authority upgrade compare-and-set lost");
  return true;
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
        `bootstrap creates only chair seats claude or codex; run ` +
        `${fabricCliCommand({ productRootFlag: custody.productRoot })} mcp peer-provision ` +
        `--project ${shellQuote(input.canonicalRoot)} --seat ${input.seat} instead`,
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
        const authority = bootstrapAuthorityInput(
          custody,
          canonicalRoot,
          identityDigest,
          input.trustRecordDigest,
          bootstrapRunDirectory,
          bootstrapAuthorityExpiresAt,
          BOOTSTRAP_AUTHORITY_ACTIONS,
        );
        const authorityJson = canonicalJson(authority);
        const authorityRef = sha256Digest(authorityJson);
        const authorityId = bootstrapAuthorityId(identityDigest, initialChairSeat);
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
      const currentChairAgentId = stringField(run, "chair_agent_id");
      const chairSeat = currentChairAgentId.startsWith("codex_") ? "codex" : "claude";
      const peerSeat = chairSeat === "codex" ? "claude" : "codex";
      const rootAuthorityId = bootstrapAuthorityId(identityDigest, chairSeat);
      const bootstrapAuthorityIds = new Set([rootAuthorityId, bootstrapAuthorityId(identityDigest, peerSeat)]);
      const unexpectedBootstrapAuthority = custody.database.prepare(
        "SELECT authority_id FROM authorities WHERE run_id=? AND authority_id LIKE ?",
      ).all(runId, `bootstrap-authority:${identityDigest}:%`)
        .find((candidate) => !isRow(candidate) || typeof candidate.authority_id !== "string" || !bootstrapAuthorityIds.has(candidate.authority_id));
      if (unexpectedBootstrapAuthority !== undefined) {
        throw bootstrapAuthorityMigrationError("authority identity is not recognised");
      }
      const rootAuthorityMigrated = migrateBootstrapAuthority({
        custody,
        canonicalRoot,
        identityDigest,
        trustRecordDigest: input.trustRecordDigest,
        bootstrapRunDirectory,
        runId,
        authorityId: rootAuthorityId,
        parentAuthorityId: null,
        required: true,
      });
      const peerAuthorityMigrated = migrateBootstrapAuthority({
        custody,
        canonicalRoot,
        identityDigest,
        trustRecordDigest: input.trustRecordDigest,
        bootstrapRunDirectory,
        runId,
        authorityId: bootstrapAuthorityId(identityDigest, peerSeat),
        parentAuthorityId: rootAuthorityId,
        required: false,
      });
      const authorityMigrationMutated = rootAuthorityMigrated || peerAuthorityMigrated;
      const active = custody.database.prepare(`
        SELECT active.generation,generation.previous_generation,generation.expires_at,
               generation.project_session_id,generation.session_revision,generation.session_generation,
               generation.run_id,generation.run_revision,generation.chair_agent_id,
               generation.chair_generation,generation.chair_lease_id
          FROM mcp_active_seat_generations active JOIN mcp_seat_generations generation ON generation.generation=active.generation
         WHERE active.project_id=?
      `).get(projectId);
      const peerAgentId = `${peerSeat}_bootstrap_peer_${identityDigest.slice(0, 16)}`;
      let activeGenerationNeedsRenewal = false;
      let requestedSeatIsActive = false;
      if (isRow(active)) {
        activeGenerationNeedsRenewal =
          numberField(active, "expires_at") - now <= MCP_SEAT_RENEWAL_WINDOW_MS;
        const member = custody.database.prepare(
          "SELECT 1 FROM mcp_seat_generation_members WHERE generation=? AND seat=?",
        ).get(active.generation, input.seat);
        requestedSeatIsActive = member !== undefined;
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
            custodyMutated: authorityMigrationMutated,
          };
        }
      }

      const requestedAgentId = input.seat === chairSeat
        ? currentChairAgentId
        : peerAgentId;
      if (!requestedSeatIsActive && requestedAgentId !== currentChairAgentId && custody.database.prepare(
        "SELECT 1 FROM agents WHERE run_id=? AND agent_id=?",
      ).get(runId, requestedAgentId) === undefined) {
        const chairAuthority = rowOrNotFound(custody.database.prepare(
          "SELECT authority_id,authority_json,authority_hash FROM authorities WHERE run_id=? AND parent_authority_id IS NULL",
        ).get(runId), "bootstrap chair authority");
        const peerAuthorityId = bootstrapAuthorityId(identityDigest, input.seat);
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

      const expiresAt = isRow(active) && !activeGenerationNeedsRenewal
        ? new Date(numberField(active, "expires_at")).toISOString()
        : input.expiresAt;
      const candidates = custody.database.prepare(`
        WITH candidates(seat,agentId,expectedPrincipalGeneration,priority) AS (
          SELECT member.seat,member.agent_id,COALESCE((
                   SELECT MAX(capability.principal_generation)
                     FROM capabilities capability
                    WHERE capability.run_id=member.run_id
                      AND capability.agent_id=member.agent_id
                      AND capability.revoked_at IS NULL
                      AND capability.expires_at>?
                 ),member.principal_generation),1
            FROM mcp_active_seat_generations active
            JOIN mcp_seat_generation_members member ON member.generation=active.generation
           WHERE active.project_id=?
          UNION ALL
          SELECT ?,agent.agent_id,(
                   SELECT MAX(capability.principal_generation)
                     FROM capabilities capability
                    WHERE capability.run_id=agent.run_id
                      AND capability.agent_id=agent.agent_id
                      AND capability.revoked_at IS NULL
                      AND capability.expires_at>?
                 ),2
            FROM agents agent
           WHERE agent.run_id=? AND agent.agent_id=? AND ?=0
        ),
        ranked AS (
          SELECT seat,agentId,expectedPrincipalGeneration,
                 ROW_NUMBER() OVER (PARTITION BY seat ORDER BY priority DESC) AS ordinal
            FROM candidates
        )
        SELECT seat,agentId,expectedPrincipalGeneration
          FROM ranked WHERE ordinal=1 ORDER BY seat
      `).all(
        now,
        projectId,
        input.seat,
        now,
        runId,
        requestedAgentId,
        requestedSeatIsActive ? 1 : 0,
      ) as CurrentMcpSeatBindingInput["bindings"];
      const bindings: CurrentMcpSeatBindingInput["bindings"] = [];
      const droppedSeats: NonNullable<BootstrapMcpSeatResult["droppedSeats"]> = [];
      for (const candidate of candidates) {
        const live = custody.database.prepare(`
          SELECT agent.lifecycle,authority.authority_json,
                 MAX(capability.principal_generation) AS principal_generation
            FROM agents agent
            JOIN authorities authority ON authority.authority_id=agent.authority_id
            JOIN capabilities capability
              ON capability.run_id=agent.run_id AND capability.agent_id=agent.agent_id
           WHERE agent.run_id=? AND agent.agent_id=?
             AND capability.revoked_at IS NULL AND capability.expires_at>?
           GROUP BY agent.lifecycle
        `).get(runId, candidate.agentId, now);
        if (
          !isRow(live) ||
          live.lifecycle === "archived" ||
          live.lifecycle === "suspended"
        ) {
          droppedSeats.push({
            seat: candidate.seat,
            agentId: candidate.agentId,
            reason: "AGENT_NOT_LIVE",
          });
          continue;
        }
        if (numberField(live, "principal_generation") !== candidate.expectedPrincipalGeneration) {
          droppedSeats.push({
            seat: candidate.seat,
            agentId: candidate.agentId,
            reason: "STALE_PRINCIPAL_GENERATION",
          });
          continue;
        }
        if (authorityExpiry(stringField(live, "authority_json")) < Date.parse(expiresAt)) {
          droppedSeats.push({
            seat: candidate.seat,
            agentId: candidate.agentId,
            reason: "AUTHORITY_EXPIRES_BEFORE_RENEWAL",
          });
          continue;
        }
        bindings.push(candidate);
      }
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
      return {
        ...bound,
        credentials,
        projectId,
        canonicalRoot,
        bootstrapRunDirectory,
        custodyMutated: true,
        ...(droppedSeats.length === 0 ? {} : { droppedSeats }),
      };
    }).immediate();
}
