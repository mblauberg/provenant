import {
  assertChairMutationAuthority,
  deriveFinalAcceptanceRef,
  parseIdentifier,
  parseMembershipBindRequest,
  parseMembershipBindResult,
  parseProjectSession,
  parseScopedGate,
  type ChairMutationContext,
  type MembershipBindRequest,
  type MembershipBindResult,
  type ProjectSession,
  type ProjectSessionCloseRequest,
  type ProjectSessionCreateRequest,
  type ProjectSessionGetRequest,
  type ProjectSessionMember,
  type ProjectSessionState,
  type ProjectSessionTransitionRequest,
  type ProjectId,
  type ProjectSessionId,
  type ChairTakeoverRequest,
  type ChairTakeoverResult,
  type FinalAcceptanceGateBinding,
  type HumanGateResolution,
  type ScopedGate,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { CommandJournal } from "../application/command-journal.js";
import { OperatorStore } from "../operator/store.js";
import { supersedeFinalAcceptanceGates } from "./acceptance-cycle.js";
import { retireProjectSessionBridges } from "./bridge-retirement.js";
import { legalProjectSessionTransitions } from "./lifecycle-facts.js";
import { membershipSourceDisposition } from "./membership-disposition.js";
import {
  ProjectFabricCoreError,
  type AuthenticatedAgentContext,
  type AuthenticatedOperatorContext,
  type CoreServiceOptions,
} from "./contracts.js";
import { canonicalJson, integer, nullableText, row, sha256, text, type Row } from "./store-support.js";

const RUN_COUPLED_SESSION_STATES = new Set<ProjectSessionState>([
  "active",
  "visibility_degraded",
  "reconciling",
  "recovery_required",
  "quarantined",
]);

export class ProjectSessionStore {
  readonly #database: Database.Database;
  readonly #operatorStore: OperatorStore;
  readonly #commandJournal: CommandJournal;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #retireVolatileProjectSession: ((projectSessionId: string) => void) | undefined;

  constructor(options: CoreServiceOptions & {
    operatorStore: OperatorStore;
    commandJournal?: CommandJournal;
    retireVolatileProjectSession?: (projectSessionId: string) => void;
  }) {
    this.#database = options.database;
    this.#operatorStore = options.operatorStore;
    this.#clock = options.clock ?? Date.now;
    this.#fault = options.fault ?? (() => undefined);
    this.#retireVolatileProjectSession = options.retireVolatileProjectSession;
    this.#commandJournal = options.commandJournal ?? new CommandJournal(this.#database, this.#clock);
  }

  createProjectSession(
    context: AuthenticatedOperatorContext,
    request: ProjectSessionCreateRequest,
  ): ProjectSession {
    return this.#operatorStore.executeCommand(
      context,
      request.command,
      {
        projectId: request.projectId,
        requiredAction: "launch",
        commandPayload: {
          projectSessionId: request.projectSessionId,
          projectId: request.projectId,
          mode: request.mode,
          generation: request.generation,
          authorityRef: request.authorityRef,
          budgetRef: request.budgetRef,
          launchPacketRef: request.launchPacketRef,
        },
      },
      () => this.#projectRevision(request.projectId),
      () => {
        this.#fault("session:create");
        const now = this.#clock();
        this.#database.prepare(`
          INSERT INTO project_sessions(
            project_session_id, project_id, mode, state, revision, generation,
            authority_ref, budget_ref, launch_packet_path, launch_packet_digest,
            membership_revision, origin_kind, origin_operator_id,
            terminal_path_json, created_at, updated_at
          ) VALUES (?, ?, ?, 'draft', 1, 1, ?, ?, ?, ?, 1, 'operator-launch', ?, NULL, ?, ?)
        `).run(
          request.projectSessionId,
          request.projectId,
          request.mode,
          request.authorityRef,
          request.budgetRef,
          request.launchPacketRef.path,
          request.launchPacketRef.digest,
          context.operatorId,
          now,
          now,
        );
        this.#database.prepare(`
          UPDATE projects SET revision=revision+1, updated_at=? WHERE project_id=?
        `).run(now, request.projectId);
        return this.getProjectSession({
          projectId: request.projectId,
          projectSessionId: request.projectSessionId,
          expectedGeneration: 1,
        });
      },
    );
  }

  getProjectSession(request: ProjectSessionGetRequest): ProjectSession {
    const stored = row(this.#database.prepare(`
      SELECT * FROM project_sessions WHERE project_session_id=? AND project_id=?
    `).get(request.projectSessionId, request.projectId), "project session");
    if (integer(stored, "generation") !== request.expectedGeneration) {
      throw new ProjectFabricCoreError("STALE_GENERATION", "project-session generation changed");
    }
    return this.#sessionFromRow(stored);
  }

  transitionProjectSession(
    context: AuthenticatedOperatorContext,
    request: ProjectSessionTransitionRequest,
    requiredAction: "decide" | "launch" = "decide",
  ): ProjectSession {
    const identity = this.#sessionIdentity(request.projectSessionId);
    const targetState: string = request.transition.to;
    if (
      identity.state === "launching" ||
      identity.state === "launch_ambiguous" ||
      targetState === "launching" ||
      targetState === "launch_ambiguous"
    ) {
      throw new ProjectFabricCoreError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "public project-session transition cannot enter or leave a launch-custody-owned state",
      );
    }
    if (targetState === "quiescing") {
      throw new ProjectFabricCoreError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "public project-session transition cannot enter the drain-custody-owned state",
      );
    }
    return this.#operatorStore.executeCommand(
      context,
      request.command,
      {
        projectId: identity.projectId,
        projectSessionId: request.projectSessionId,
        sessionGeneration: request.expectedGeneration,
        requiredAction,
        commandPayload: {
          projectSessionId: request.projectSessionId,
          expectedGeneration: request.expectedGeneration,
          transition: request.transition,
        },
      },
      () => this.#sessionRevision(request.projectSessionId),
      () => {
        const current = this.#sessionIdentity(request.projectSessionId);
        this.#assertNoOpenLiveHandoff(request.projectSessionId);
        if (current.generation !== request.expectedGeneration) {
          throw new ProjectFabricCoreError("STALE_GENERATION", "project-session generation changed");
        }
        if (!legalProjectSessionTransitions(current.state).includes(request.transition.to)) {
          throw new ProjectFabricCoreError(
            "LIFECYCLE_PRECONDITION_FAILED",
            `illegal project-session transition ${current.state} -> ${request.transition.to}`,
          );
        }
        this.#fault("session:transition");
        if (request.transition.to === "awaiting_launch") {
          this.#database.prepare(`
            UPDATE project_sessions
               SET state='awaiting_launch', launch_packet_path=?, launch_packet_digest=?,
                   revision=revision+1, updated_at=?
             WHERE project_session_id=? AND revision=? AND generation=? AND state='draft'
          `).run(
            request.transition.launchPacketRef.path,
            request.transition.launchPacketRef.digest,
            this.#clock(),
            request.projectSessionId,
            current.revision,
            request.expectedGeneration,
          );
        } else if (request.transition.to === "awaiting_acceptance") {
          const membershipChanged = this.#prepareAwaitingAcceptance(
            context,
            request.projectSessionId,
            current,
            request.transition.closureEvidence,
          );
          const changed = this.#database.prepare(`
            UPDATE project_sessions
               SET state='awaiting_acceptance',
                   membership_revision=membership_revision+?,
                   revision=revision+1,
                   updated_at=?
             WHERE project_session_id=? AND revision=? AND generation=? AND state='quiescing'
          `).run(
            membershipChanged ? 1 : 0,
            this.#clock(),
            request.projectSessionId,
            current.revision,
            request.expectedGeneration,
          );
          if (changed.changes !== 1) {
            throw new ProjectFabricCoreError("STALE_REVISION", "project-session acceptance transition raced another mutation");
          }
        } else if (
          request.transition.to === "active" &&
          (current.state === "awaiting_acceptance" || current.state === "quiescing")
        ) {
          const membershipChanged = this.#reopenFromAcceptance(
            request.projectSessionId,
            request.command.commandId,
            current.state,
          );
          const changed = this.#database.prepare(`
            UPDATE project_sessions
               SET state='active', membership_revision=membership_revision+?,
                   revision=revision+1, updated_at=?
             WHERE project_session_id=? AND revision=? AND generation=?
               AND state=?
          `).run(
            membershipChanged ? 1 : 0,
            this.#clock(),
            request.projectSessionId,
            current.revision,
            request.expectedGeneration,
            current.state,
          );
          if (changed.changes !== 1) {
            throw new ProjectFabricCoreError("STALE_REVISION", "project-session reopen raced another mutation");
          }
        } else if (current.state === "quiescing" || current.state === "awaiting_acceptance") {
          const membershipChanged = this.#divertFromAcceptanceCycle(
            request.projectSessionId,
            request.command.commandId,
            current.state,
            request.transition.to,
          );
          const changed = this.#database.prepare(`
            UPDATE project_sessions
               SET state=?, membership_revision=membership_revision+?,
                   revision=revision+1, updated_at=?
             WHERE project_session_id=? AND revision=? AND generation=? AND state=?
          `).run(
            request.transition.to,
            membershipChanged ? 1 : 0,
            this.#clock(),
            request.projectSessionId,
            current.revision,
            request.expectedGeneration,
            current.state,
          );
          if (changed.changes !== 1) {
            throw new ProjectFabricCoreError("STALE_REVISION", "project-session quiesce exit raced another mutation");
          }
        } else if (
          RUN_COUPLED_SESSION_STATES.has(current.state) &&
          RUN_COUPLED_SESSION_STATES.has(request.transition.to)
        ) {
          this.#transitionCoupledRuns(
            request.projectSessionId,
            current.state,
            request.transition.to,
          );
          const changed = this.#database.prepare(`
            UPDATE project_sessions
               SET state=?, revision=revision+1, updated_at=?
             WHERE project_session_id=? AND revision=? AND generation=? AND state=?
          `).run(
            request.transition.to,
            this.#clock(),
            request.projectSessionId,
            current.revision,
            request.expectedGeneration,
            current.state,
          );
          if (changed.changes !== 1) {
            throw new ProjectFabricCoreError("STALE_REVISION", "exceptional project-session transition raced another mutation");
          }
        } else {
          this.#database.prepare(`
            UPDATE project_sessions
               SET state=?, revision=revision+1, updated_at=?
             WHERE project_session_id=? AND revision=? AND generation=?
          `).run(
            request.transition.to,
            this.#clock(),
            request.projectSessionId,
            current.revision,
            request.expectedGeneration,
          );
        }
        return this.getProjectSession({
          projectId: current.projectId as ProjectId,
          projectSessionId: request.projectSessionId,
          expectedGeneration: request.expectedGeneration,
        });
      },
    );
  }

  closeProjectSession(
    context: AuthenticatedOperatorContext,
    request: ProjectSessionCloseRequest,
  ): ProjectSession {
    const identity = this.#sessionIdentity(request.projectSessionId);
    const terminalPath = canonicalJson(request.terminalPath);
    const result = this.#operatorStore.executeCommand(
      context,
      request.command,
      {
        projectId: identity.projectId,
        projectSessionId: request.projectSessionId,
        sessionGeneration: request.expectedGeneration,
        requiredAction: "decide",
        commandPayload: {
          projectSessionId: request.projectSessionId,
          expectedGeneration: request.expectedGeneration,
          terminalPath: request.terminalPath,
        },
      },
      () => this.#sessionRevision(request.projectSessionId),
      () => {
        const current = this.#sessionIdentity(request.projectSessionId);
        this.#assertNoOpenLiveHandoff(request.projectSessionId);
        const accepted = request.terminalPath.kind === "accepted";
        if (accepted && current.state !== "awaiting_acceptance") {
          throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "accepted project close requires awaiting acceptance");
        }
        if (!accepted && !["draft", "awaiting_launch", "launch_failed", "awaiting_acceptance"].includes(current.state)) {
          throw new ProjectFabricCoreError(
            "LIFECYCLE_PRECONDITION_FAILED",
            "cancelled or failed project close requires a prelaunch, launch-failed or awaiting-acceptance session",
          );
        }
        const superseded = accepted || current.state !== "awaiting_acceptance"
          ? { gateChanges: 0, membershipChanges: 0 }
          : supersedeFinalAcceptanceGates({
              database: this.#database,
              projectSessionId: request.projectSessionId,
              cause: { kind: "operator-command", ref: request.command.commandId },
              reason: `project session closed through ${request.terminalPath.kind} terminal path`,
              now: this.#clock(),
            });
        this.#assertClosure(request.projectSessionId);
        const runs = this.#database.prepare(`
          SELECT run_id, revision, lifecycle_state FROM runs
           WHERE project_session_id=? ORDER BY run_id
        `).all(request.projectSessionId) as Row[];
        const awaitingRuns = runs.filter((run) => text(run, "lifecycle_state") === "awaiting_acceptance");
        for (const run of runs) {
          if (!["awaiting_acceptance", "closed", "cancelled", "launch_failed"].includes(text(run, "lifecycle_state"))) {
            throw new ProjectFabricCoreError(
              "LIFECYCLE_PRECONDITION_FAILED",
              "project close found a nonterminal coordination run outside acceptance",
            );
          }
        }
        if (request.terminalPath.kind === "accepted") {
          if (awaitingRuns.length === 0) {
            throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "accepted close requires a coordination run");
          }
          this.#assertFinalAcceptance(context, current.projectId, request.projectSessionId, request.terminalPath.acceptanceRef, awaitingRuns);
        }
        this.#fault("session:close");
        for (const run of runs) {
          if (text(run, "lifecycle_state") !== "awaiting_acceptance") continue;
          const changedRun = this.#database.prepare(`
            UPDATE runs SET lifecycle_state='closed', revision=revision+1
             WHERE run_id=? AND revision=? AND lifecycle_state='awaiting_acceptance'
          `).run(text(run, "run_id"), integer(run, "revision"));
          if (changedRun.changes !== 1) {
            throw new ProjectFabricCoreError("STALE_REVISION", "coordination run changed during project close");
          }
        }
        this.#fault("session:close:after-runs");
        this.#database.prepare(`
          UPDATE run_chair_leases SET status='revoked', updated_at=?
           WHERE project_session_id=? AND status IN ('active','frozen')
        `).run(this.#clock(), request.projectSessionId);
        this.#database.prepare(`
          UPDATE capabilities SET revoked_at=?
           WHERE run_id IN (SELECT run_id FROM runs WHERE project_session_id=?)
             AND revoked_at IS NULL
        `).run(this.#clock(), request.projectSessionId);
        this.#database.prepare(`
          UPDATE agents SET lifecycle='archived'
           WHERE run_id IN (SELECT run_id FROM runs WHERE project_session_id=?)
        `).run(request.projectSessionId);
        const changedSession = this.#database.prepare(`
          UPDATE project_sessions
             SET state='closed', terminal_path_json=?,
                 membership_revision=membership_revision+?,
                 revision=revision+1, updated_at=?
           WHERE project_session_id=? AND revision=? AND generation=?
        `).run(
          terminalPath,
          superseded.gateChanges + superseded.membershipChanges > 0 ? 1 : 0,
          this.#clock(),
          request.projectSessionId,
          current.revision,
          request.expectedGeneration,
        );
        if (changedSession.changes !== 1) {
          throw new ProjectFabricCoreError("STALE_REVISION", "project close raced another transition");
        }
        return this.getProjectSession({
          projectId: current.projectId as ProjectId,
          projectSessionId: request.projectSessionId,
          expectedGeneration: request.expectedGeneration,
        });
      },
      () => {
        retireProjectSessionBridges(this.#database, {
          projectSessionId: request.projectSessionId,
          sourceKind: "project-session-close",
          terminalKind: request.terminalPath.kind,
          terminalRef: terminalPath,
          ownerOperatorId: context.operatorId,
          ownerRef: request.command.commandId,
          now: this.#clock(),
        });
        this.#fault("session:close:after-bridges");
      },
    );
    try { this.#retireVolatileProjectSession?.(request.projectSessionId); } catch { /* durable fencing already committed */ }
    return result;
  }

  bindMembership(
    context: AuthenticatedOperatorContext | AuthenticatedAgentContext,
    input: MembershipBindRequest,
  ): MembershipBindResult {
    const request = parseMembershipBindRequest(input);
    if (request.origin === "chair") {
      if (!("agentId" in context)) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair membership binding requires an agent principal");
      }
      return this.#executeChairMembership(context, request);
    }
    if (!("operatorId" in context)) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator membership binding requires an operator principal");
    }
    const identity = this.#sessionIdentity(request.projectSessionId);
    return this.#operatorStore.executeCommand(
      context,
      request.command,
      {
        projectId: identity.projectId,
        projectSessionId: request.projectSessionId,
        sessionGeneration: identity.generation,
        requiredAction: "decide",
        commandPayload: {
          projectSessionId: request.projectSessionId,
          coordinationRunId: request.coordinationRunId,
          expectedMembershipRevision: request.expectedMembershipRevision,
          members: request.members,
        },
      },
      () => this.#membershipRevision(request.projectSessionId),
      () => this.#applyMembership(request),
    );
  }

  #executeChairMembership(
    context: AuthenticatedAgentContext,
    request: Extract<MembershipBindRequest, { origin: "chair" }>,
  ): MembershipBindResult {
    const execute = this.#database.transaction(() => {
      this.#assertChairMembershipAuthority(context, request.command);
      return this.#commandJournal.execute(
        context.coordinationRunId,
        context.agentId,
        request.command.commandId,
        request,
        (value): value is MembershipBindResult => {
          try {
            parseMembershipBindResult(value);
            return true;
          } catch {
            return false;
          }
        },
        () => this.#applyMembership(request),
      );
    });
    return execute();
  }

  #applyMembership(request: MembershipBindRequest): MembershipBindResult {
    const session = this.#sessionIdentity(request.projectSessionId);
    if (
      session.state === "awaiting_acceptance" ||
      session.state === "closed" ||
      session.state === "cancelled"
    ) {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "project-session membership is frozen");
    }
    const settlingWhileQuiescing = session.state === "quiescing";
    if (settlingWhileQuiescing && request.members.some((member) => member.state === "active")) {
      throw new ProjectFabricCoreError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "quiescing permits settlement of existing membership only",
      );
    }
    if (session.membershipRevision !== request.expectedMembershipRevision) {
      throw new ProjectFabricCoreError("STALE_REVISION", "membership revision changed");
    }
    this.#assertRun(request.projectSessionId, request.coordinationRunId);
    for (const member of request.members) {
      this.#assertMemberTarget(request.projectSessionId, request.coordinationRunId, member);
      this.#assertMemberDisposition(request.projectSessionId, request.coordinationRunId, member);
      const disposition = member.state === "terminal" ? "reconciled" : member.state;
      const timestamp = this.#clock();
      if (settlingWhileQuiescing) {
        const settled = this.#database.prepare(`
          UPDATE project_session_memberships
             SET state=?, revision=revision+1, abandoned_reason=?, updated_at=?
           WHERE project_session_id=? AND coordination_run_id=?
             AND member_kind=? AND member_id=? AND member_adapter_id=?
             AND required=1 AND state='active'
        `).run(
          disposition,
          member.state === "abandoned" ? member.reason : null,
          timestamp,
          request.projectSessionId,
          request.coordinationRunId,
          member.kind,
          this.#memberId(member),
          this.#memberAdapterId(member),
        );
        if (settled.changes !== 1) {
          throw new ProjectFabricCoreError(
            "LIFECYCLE_PRECONDITION_FAILED",
            "quiescing membership settlement requires one existing active member",
          );
        }
      } else {
        this.#database.prepare(`
          INSERT INTO project_session_memberships(
            project_session_id, coordination_run_id, member_kind, member_id, member_adapter_id,
            required, state, revision, abandoned_reason, created_at, updated_at
          ) VALUES (?, ?, ?, ?, ?, 1, ?, 1, ?, ?, ?)
          ON CONFLICT(project_session_id, coordination_run_id, member_kind, member_id, member_adapter_id)
          DO UPDATE SET state=excluded.state,
                        revision=project_session_memberships.revision+1,
                        abandoned_reason=excluded.abandoned_reason,
                        updated_at=excluded.updated_at
        `).run(
          request.projectSessionId,
          request.coordinationRunId,
          member.kind,
          this.#memberId(member),
          this.#memberAdapterId(member),
          disposition,
          member.state === "abandoned" ? member.reason : null,
          timestamp,
          timestamp,
        );
      }
    }
    this.#fault("session:membership");
    const changed = this.#database.prepare(`
      UPDATE project_sessions
         SET membership_revision=membership_revision+1, revision=revision+1, updated_at=?
       WHERE project_session_id=? AND membership_revision=?
    `).run(this.#clock(), request.projectSessionId, request.expectedMembershipRevision);
    if (changed.changes !== 1) {
      throw new ProjectFabricCoreError("STALE_REVISION", "membership revision changed before commit");
    }
    return {
      projectSessionId: request.projectSessionId,
      coordinationRunId: request.coordinationRunId,
      membershipRevision: request.expectedMembershipRevision + 1,
      members: request.members,
    };
  }

  takeoverChair(
    context: AuthenticatedOperatorContext,
    request: ChairTakeoverRequest,
  ): ChairTakeoverResult {
    const session = this.#sessionIdentity(request.projectSessionId);
    return this.#operatorStore.executeCommand(
      context,
      request.command,
      {
        projectId: session.projectId,
        projectSessionId: request.projectSessionId,
        sessionGeneration: request.expectedSessionGeneration,
        requiredAction: "takeover",
        commandPayload: {
          projectSessionId: request.projectSessionId,
          runId: request.runId,
          expectedChairAgentId: request.expectedChairAgentId,
          successorChairAgentId: request.successorChairAgentId,
          expectedChairGeneration: request.expectedChairGeneration,
          expectedSessionGeneration: request.expectedSessionGeneration,
          handoffRef: request.handoffRef,
          targetRevision: request.targetRevision,
        },
      },
      () => this.#sessionRevision(request.projectSessionId),
      () => {
        const currentSession = this.#sessionIdentity(request.projectSessionId);
        this.#assertNoOpenLiveHandoff(request.projectSessionId);
        if (["quiescing", "awaiting_acceptance", "closed", "cancelled"].includes(currentSession.state)) {
          throw new ProjectFabricCoreError(
            "LIFECYCLE_PRECONDITION_FAILED",
            "chair takeover requires reopening the project session",
          );
        }
        if (currentSession.generation !== request.expectedSessionGeneration) {
          throw new ProjectFabricCoreError("STALE_GENERATION", "project-session generation changed");
        }
        if (this.#database.prepare(`
          SELECT 1 FROM launched_chair_bridge_state
           WHERE project_session_id=? AND coordination_run_id=? AND state IN ('active','lost')
           LIMIT 1
        `).get(request.projectSessionId, request.runId) !== undefined) {
          throw new ProjectFabricCoreError(
            "LIFECYCLE_PRECONDITION_FAILED",
            "launched-chair takeover is owned by typed chair-bridge recovery custody",
          );
        }
        const run = row(this.#database.prepare(`
          SELECT chair_agent_id, chair_generation, chair_lease_id, revision
            FROM runs WHERE project_session_id=? AND run_id=?
        `).get(request.projectSessionId, request.runId), "coordination run");
        if (
          text(run, "chair_agent_id") !== request.expectedChairAgentId ||
          integer(run, "chair_generation") !== request.expectedChairGeneration ||
          integer(run, "revision") + 1 !== request.targetRevision
        ) {
          throw new ProjectFabricCoreError("STALE_REVISION", "chair identity, generation or target revision changed");
        }
        const capability = row(this.#database.prepare(`
          SELECT kind, handoff_digest, old_chair_generation, expected_run_id,
                 expected_run_revision, expected_session_revision, cas_target_revision
            FROM operator_capabilities WHERE capability_id=?
        `).get(request.command.credential.capabilityId), "takeover capability");
        if (
          text(capability, "kind") !== "takeover" ||
          text(capability, "handoff_digest") !== request.handoffRef.digest ||
          integer(capability, "old_chair_generation") !== request.expectedChairGeneration ||
          text(capability, "expected_run_id") !== request.runId ||
          integer(capability, "expected_run_revision") !== integer(run, "revision") ||
          integer(capability, "expected_session_revision") !== currentSession.revision ||
          integer(capability, "cas_target_revision") !== request.targetRevision
        ) {
          throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "takeover capability binding does not match current state");
        }
        const handoff = this.#database.prepare(`
          SELECT 1 FROM artifacts
           WHERE run_id=? AND relative_path=? AND sha256=? AND publisher_agent_id=?
        `).get(request.runId, request.handoffRef.path, request.handoffRef.digest, request.expectedChairAgentId);
        if (handoff === undefined) {
          throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "persisted chair handoff does not match the takeover binding");
        }
        const currentLease = this.#database.prepare(`
          SELECT 1 FROM run_chair_leases
           WHERE project_session_id=? AND run_id=? AND lease_id=? AND holder_agent_id=?
             AND generation=? AND status='active'
        `).get(
          request.projectSessionId,
          request.runId,
          text(run, "chair_lease_id"),
          request.expectedChairAgentId,
          request.expectedChairGeneration,
        );
        if (currentLease === undefined) {
          throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "old chair generation is not an active fenced lease");
        }
        if (this.#database.prepare(`
          SELECT 1 FROM leases
           WHERE run_id=? AND holder_agent_id=? AND status IN ('active','quarantined') LIMIT 1
        `).get(request.runId, request.expectedChairAgentId) !== undefined) {
          throw new ProjectFabricCoreError(
            "WRITE_SCOPE_RECOVERY_REQUIRED",
            "old chair still owns an unreconciled write scope",
          );
        }
        if (this.#database.prepare(`
          SELECT 1 FROM agents WHERE run_id=? AND agent_id=?
        `).get(request.runId, request.successorChairAgentId) === undefined) {
          throw new ProjectFabricCoreError("NOT_FOUND", "successor chair agent was not found");
        }
        const nextGeneration = request.expectedChairGeneration + 1;
        const nextLeaseId = `chair:${request.runId}:${String(nextGeneration)}`;
        this.#fault("takeover:fence");
        this.#database.prepare(`
          UPDATE run_chair_leases
             SET status='frozen', handoff_digest=?, updated_at=?
           WHERE project_session_id=? AND run_id=? AND generation=? AND status='active'
        `).run(
          request.handoffRef.digest,
          this.#clock(),
          request.projectSessionId,
          request.runId,
          request.expectedChairGeneration,
        );
        this.#database.prepare(`
          INSERT INTO delivery_freezes(run_id, agent_id, reason, created_at)
          VALUES (?, ?, 'chair-takeover', ?)
          ON CONFLICT(run_id, agent_id) DO UPDATE SET reason=excluded.reason
        `).run(request.runId, request.expectedChairAgentId, this.#clock());
        this.#fault("takeover:chair");
        this.#database.prepare(`
          UPDATE runs
             SET chair_agent_id=?, chair_generation=?, chair_lease_id=?, revision=revision+1
           WHERE run_id=? AND project_session_id=? AND revision=? AND chair_generation=?
        `).run(
          request.successorChairAgentId,
          nextGeneration,
          nextLeaseId,
          request.runId,
          request.projectSessionId,
          integer(run, "revision"),
          request.expectedChairGeneration,
        );
        this.#database.prepare(`
          UPDATE project_sessions
             SET generation=generation+1, membership_revision=membership_revision+1,
                 revision=revision+1, updated_at=?
           WHERE project_session_id=? AND generation=? AND revision=?
        `).run(
          this.#clock(),
          request.projectSessionId,
          request.expectedSessionGeneration,
          currentSession.revision,
        );
        this.#fault("takeover:lease");
        this.#database.prepare(`
          INSERT INTO run_chair_leases(
            project_session_id, run_id, lease_id, holder_agent_id, generation, status, handoff_digest, updated_at
          ) VALUES (?, ?, ?, ?, ?, 'active', ?, ?)
        `).run(
          request.projectSessionId,
          request.runId,
          nextLeaseId,
          request.successorChairAgentId,
          nextGeneration,
          request.handoffRef.digest,
          this.#clock(),
        );
        const revokedPredecessor = this.#database.prepare(`
          UPDATE run_chair_leases SET status='revoked', updated_at=?
           WHERE project_session_id=? AND run_id=? AND lease_id=?
             AND generation=? AND status='frozen'
        `).run(
          this.#clock(),
          request.projectSessionId,
          request.runId,
          text(run, "chair_lease_id"),
          request.expectedChairGeneration,
        );
        if (revokedPredecessor.changes !== 1) {
          throw new ProjectFabricCoreError("STALE_LEASE_GENERATION", "predecessor chair lease changed during takeover");
        }
        const retiredMembership = this.#database.prepare(`
          UPDATE project_session_memberships
             SET state='abandoned', abandoned_reason='chair-takeover',
                 revision=revision+1, updated_at=?
           WHERE project_session_id=? AND coordination_run_id=?
             AND member_kind='lease' AND member_id=? AND required=1 AND state='active'
        `).run(
          this.#clock(),
          request.projectSessionId,
          request.runId,
          text(run, "chair_lease_id"),
        );
        if (retiredMembership.changes !== 1) {
          throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "predecessor chair membership was not active");
        }
        this.#database.prepare(`
          INSERT INTO project_session_memberships(
            project_session_id, coordination_run_id, member_kind, member_id,
            required, state, revision, abandoned_reason, created_at, updated_at
          ) VALUES (?, ?, 'lease', ?, 1, 'active', 1, NULL, ?, ?)
        `).run(
          request.projectSessionId,
          request.runId,
          nextLeaseId,
          this.#clock(),
          this.#clock(),
        );
        return {
          projectSessionId: request.projectSessionId,
          sessionRevision: currentSession.revision + 1,
          runRevision: request.targetRevision,
          chairAgentId: request.successorChairAgentId,
          chairGeneration: nextGeneration,
        };
      },
    );
  }

  #projectRevision(projectId: string): { revision: number; value: { projectId: string; revision: number } } {
    const project = row(this.#database.prepare("SELECT revision FROM projects WHERE project_id=?").get(projectId), "project");
    const revision = integer(project, "revision");
    return { revision, value: { projectId, revision } };
  }

  #sessionRevision(projectSessionId: string): { revision: number; value: ProjectSession } {
    const identity = this.#sessionIdentity(projectSessionId);
    const value = this.getProjectSession({
      projectId: identity.projectId as ProjectId,
      projectSessionId: projectSessionId as ProjectSessionId,
      expectedGeneration: identity.generation,
    });
    return { revision: identity.revision, value };
  }

  #membershipRevision(projectSessionId: string): { revision: number; value: { membershipRevision: number } } {
    const identity = this.#sessionIdentity(projectSessionId);
    return { revision: identity.membershipRevision, value: { membershipRevision: identity.membershipRevision } };
  }

  #sessionIdentity(projectSessionId: string): {
    projectId: string;
    state: ProjectSessionState;
    revision: number;
    generation: number;
    membershipRevision: number;
  } {
    const stored = row(this.#database.prepare(`
      SELECT project_id, state, revision, generation, membership_revision
        FROM project_sessions WHERE project_session_id=?
    `).get(projectSessionId), "project session");
    return {
      projectId: text(stored, "project_id"),
      state: text(stored, "state") as ProjectSessionState,
      revision: integer(stored, "revision"),
      generation: integer(stored, "generation"),
      membershipRevision: integer(stored, "membership_revision"),
    };
  }

  #sessionFromRow(stored: Row): ProjectSession {
    const state = text(stored, "state") as ProjectSessionState;
    if (text(stored, "origin_kind") !== "operator-launch") {
      throw new Error("stored project-session origin is invalid for the current baseline");
    }
    const origin = { kind: "operator-launch" as const, operatorId: text(stored, "origin_operator_id") };
    const base = {
      projectSessionId: text(stored, "project_session_id"),
      projectId: text(stored, "project_id"),
      mode: text(stored, "mode") as "coordinated" | "independent",
      revision: integer(stored, "revision"),
      generation: integer(stored, "generation"),
      authorityRef: text(stored, "authority_ref"),
      budgetRef: text(stored, "budget_ref"),
      launchPacketRef: {
        path: text(stored, "launch_packet_path"),
        digest: text(stored, "launch_packet_digest"),
      },
      membershipRevision: integer(stored, "membership_revision"),
      origin,
    };
    const terminal = nullableText(stored, "terminal_path_json");
    if (state === "closed") {
      if (terminal === null) throw new Error("closed session has no terminal path");
      return parseProjectSession({ ...base, state, terminalPath: JSON.parse(terminal) });
    }
    if (state === "cancelled") {
      if (terminal === null) throw new Error("cancelled session has no terminal path");
      return parseProjectSession({ ...base, state, terminalPath: JSON.parse(terminal) });
    }
    return parseProjectSession({ ...base, state });
  }

  #assertFinalAcceptance(
    context: AuthenticatedOperatorContext,
    projectId: string,
    projectSessionId: string,
    acceptanceRef: string,
    awaitingRuns: readonly Row[],
  ): void {
    const candidates = this.#database.prepare(`
      SELECT gate.*
        FROM scoped_gates gate
        JOIN runs run
          ON run.project_session_id=gate.project_session_id
         AND run.run_id=gate.coordination_run_id
       WHERE gate.project_session_id=?
         AND run.lifecycle_state='awaiting_acceptance'
         AND gate.scope_kind='run'
         AND gate.status='approved'
         AND gate.human_required=1
         AND EXISTS (
           SELECT 1 FROM scoped_gate_operations operation
            WHERE operation.gate_id=gate.gate_id
              AND operation.operation_id='fabric.v1.project-session.close'
         )
       ORDER BY gate.coordination_run_id, gate.gate_id
    `).all(projectSessionId) as Row[];
    const expectedRunIds = awaitingRuns.map((run) => text(run, "run_id")).sort();
    if (candidates.length !== expectedRunIds.length || candidates.length === 0) {
      throw new ProjectFabricCoreError(
        "CAPABILITY_FORBIDDEN",
        "final acceptance requires exactly one approved human gate per awaiting run",
      );
    }
    const bindings: FinalAcceptanceGateBinding[] = [];
    for (const [index, stored] of candidates.entries()) {
      const gate = this.#acceptanceGateFromRow(stored);
      if (gate.coordinationRunId !== expectedRunIds[index]) {
        throw new ProjectFabricCoreError(
          "CAPABILITY_FORBIDDEN",
          "final acceptance gate set does not cover the exact awaiting runs",
        );
      }
      if (
        gate.expectedApproverRef !== "authenticated-human-operator" &&
        gate.expectedApproverRef !== context.operatorId
      ) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "final acceptance gate has another approver");
      }
      if (
        gate.resolution.operatorId !== context.operatorId ||
        !gate.enforcementPoints.includes("operation") ||
        !gate.blockedOperationIds.includes("fabric.v1.project-session.close")
      ) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "final acceptance gate authority does not match close");
      }
      const runRevision = row(this.#database.prepare(`
        SELECT dependency_revision FROM runs
         WHERE project_session_id=? AND run_id=? AND lifecycle_state='awaiting_acceptance'
      `).get(projectSessionId, gate.coordinationRunId), "final acceptance run binding");
      if (integer(runRevision, "dependency_revision") !== gate.dependencyRevision) {
        throw new ProjectFabricCoreError("STALE_REVISION", "final acceptance dependency binding changed");
      }
      if (gate.resolution.kind === "typed-console") {
        const commandValue = this.#database.prepare(`
          SELECT provenance_json FROM operator_commands
           WHERE operator_id=? AND command_id=? AND project_id=? AND project_session_id=?
             AND operation='decide' AND status='committed'
        `).get(
          context.operatorId,
          gate.resolution.confirmationCommandId,
          projectId,
          projectSessionId,
        );
        if (commandValue === undefined) {
          throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed final acceptance confirmation was not persisted");
        }
        const provenance: unknown = JSON.parse(text(row(commandValue, "final acceptance command"), "provenance_json"));
        if (
          typeof provenance !== "object" || provenance === null || Array.isArray(provenance) ||
          Reflect.get(provenance, "kind") !== "console-direct-input"
        ) {
          throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "final acceptance was not direct Console input");
        }
      } else {
        const attestationValue = this.#database.prepare(`
          SELECT operator_id, project_id, project_session_id, coordination_run_id,
                 gate_id, expected_gate_revision, integration_id,
                 integration_generation, interpreted_decision
            FROM operator_input_attestations WHERE attestation_id=?
        `).get(gate.resolution.attestationId);
        if (attestationValue === undefined) {
          throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "native final acceptance attestation was not persisted");
        }
        const attestation = row(attestationValue, "final acceptance attestation");
        if (
          text(attestation, "operator_id") !== context.operatorId ||
          text(attestation, "project_id") !== projectId ||
          text(attestation, "project_session_id") !== projectSessionId ||
          text(attestation, "coordination_run_id") !== gate.coordinationRunId ||
          text(attestation, "gate_id") !== gate.gateId ||
          integer(attestation, "expected_gate_revision") + 1 !== gate.revision ||
          text(attestation, "integration_id") !== gate.resolution.integrationId ||
          integer(attestation, "integration_generation") !== gate.resolution.integrationGeneration ||
          text(attestation, "interpreted_decision") !== "approve"
        ) {
          throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "native final acceptance binding changed");
        }
      }
      bindings.push({
        gateId: gate.gateId,
        coordinationRunId: gate.coordinationRunId,
        gateRevision: gate.revision,
        status: "approved",
        resolution: gate.resolution,
        evidenceRefs: gate.evidenceRefs,
      });
    }
    const authoritative = deriveFinalAcceptanceRef({
      projectSessionId: projectSessionId as Parameters<typeof deriveFinalAcceptanceRef>[0]["projectSessionId"],
      gates: bindings,
    });
    if (authoritative !== acceptanceRef) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "final acceptance reference is not authoritative");
    }
  }

  #acceptanceGateFromRow(
    stored: Row,
  ): ScopedGate & { status: "approved"; resolution: HumanGateResolution } {
    const affectedTaskIds = this.#database.prepare(`
      SELECT task_id FROM scoped_gate_tasks WHERE gate_id=? ORDER BY task_id
    `).all(text(stored, "gate_id")).map((value) => text(row(value, "gate task binding"), "task_id"));
    const deadline = stored.deadline === null
      ? undefined
      : new Date(integer(stored, "deadline")).toISOString();
    const defaultAction = nullableText(stored, "default_action");
    const release = nullableText(stored, "release_binding_json");
    const resolution = nullableText(stored, "resolution_json");
    const gate = parseScopedGate({
      gateId: text(stored, "gate_id"),
      projectSessionId: text(stored, "project_session_id"),
      coordinationRunId: text(stored, "coordination_run_id"),
      scope: { kind: "run" },
      affectedTaskIds,
      dependencyRevision: integer(stored, "dependency_revision"),
      blockedOperationIds: JSON.parse(text(stored, "blocked_operation_ids_json")),
      enforcementPoints: JSON.parse(text(stored, "enforcement_points_json")),
      question: text(stored, "question"),
      reason: text(stored, "reason"),
      options: JSON.parse(text(stored, "options_json")),
      recommendation: text(stored, "recommendation"),
      consequences: JSON.parse(text(stored, "consequences_json")),
      evidenceRefs: JSON.parse(text(stored, "evidence_refs_json")),
      revision: integer(stored, "revision"),
      createdByRef: text(stored, "created_by_ref"),
      expectedApproverRef: text(stored, "expected_approver_ref"),
      ...(deadline === undefined ? {} : { deadline }),
      ...(defaultAction === null ? {} : { default: defaultAction }),
      status: text(stored, "status"),
      ...(resolution === null ? {} : { resolution: JSON.parse(resolution) }),
      ...(release === null ? {} : { releaseBinding: JSON.parse(release) }),
    });
    if (gate.status !== "approved") {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "final acceptance gate is not approved");
    }
    return gate as ScopedGate & { status: "approved"; resolution: HumanGateResolution };
  }

  #prepareAwaitingAcceptance(
    context: AuthenticatedOperatorContext,
    projectSessionId: string,
    current: { projectId: string; revision: number; generation: number },
    closureEvidence: { path: string; digest: string },
  ): boolean {
    const receiptValue = this.#database.prepare(`
      SELECT * FROM operator_lifecycle_receipts
       WHERE relative_path=? AND sha256=? AND kind='project-session-drain'
    `).get(closureEvidence.path, closureEvidence.digest);
    if (receiptValue === undefined) {
      throw new ProjectFabricCoreError("NOT_FOUND", "project-session drain receipt was not found");
    }
    const receipt = row(receiptValue, "project-session drain receipt");
    if (
      text(receipt, "operator_id") !== context.operatorId ||
      text(receipt, "project_id") !== current.projectId ||
      text(receipt, "authority_session_id") !== projectSessionId ||
      text(receipt, "project_session_id") !== projectSessionId ||
      integer(receipt, "session_revision") !== current.revision ||
      integer(receipt, "session_generation") !== current.generation ||
      `sha256:${sha256(text(receipt, "receipt_json"))}` !== closureEvidence.digest
    ) {
      throw new ProjectFabricCoreError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "closure evidence does not bind the current drained project session",
      );
    }
    const runs = this.#database.prepare(`
      SELECT run_id, revision, chair_lease_id, chair_generation, lifecycle_state
        FROM runs WHERE project_session_id=? ORDER BY run_id
    `).all(projectSessionId) as Row[];
    if (runs.length === 0) {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "acceptance requires a coordination run");
    }
    for (const run of runs) {
      const runId = text(run, "run_id");
      const lifecycle = text(run, "lifecycle_state");
      if (!["quiescing", "awaiting_acceptance", "closed", "cancelled", "launch_failed"].includes(lifecycle)) {
        throw new ProjectFabricCoreError(
          "LIFECYCLE_PRECONDITION_FAILED",
          "coordination run is neither acceptance-ready nor terminal history",
        );
      }
      const leaseValue = this.#database.prepare(`
        SELECT status FROM run_chair_leases
         WHERE project_session_id=? AND run_id=? AND lease_id=? AND generation=?
      `).get(
        projectSessionId,
        runId,
        text(run, "chair_lease_id"),
        integer(run, "chair_generation"),
      );
      if (leaseValue === undefined) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "current chair lease is unavailable for terminal reconciliation");
      }
      const leaseStatus = text(row(leaseValue, "coordination run chair lease"), "status");
      if (lifecycle === "quiescing" && !["active", "frozen"].includes(leaseStatus)) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "quiescing run chair lease is not current");
      }
      if (lifecycle === "awaiting_acceptance" && leaseStatus !== "frozen") {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "acceptance run chair lease is not frozen");
      }
      if (["closed", "cancelled", "launch_failed"].includes(lifecycle) && ["active", "frozen"].includes(leaseStatus)) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "historical run retains a live chair lease");
      }
      const expectedMembershipState = lifecycle === "quiescing"
        ? "active"
        : lifecycle === "awaiting_acceptance" || lifecycle === "closed"
          ? "reconciled"
          : "abandoned";
      for (const [kind, memberId] of [
        ["coordination-run", runId],
        ["lease", text(run, "chair_lease_id")],
      ] as const) {
        const membership = this.#database.prepare(`
          SELECT state, abandoned_reason FROM project_session_memberships
           WHERE project_session_id=? AND coordination_run_id=?
             AND member_kind=? AND member_id=? AND required=1
        `).get(projectSessionId, runId, kind, memberId);
        if (membership === undefined) {
          throw new ProjectFabricCoreError("RECOVERY_REQUIRED", `required ${kind} membership was not found`);
        }
        const value = row(membership, `${kind} membership`);
        if (
          text(value, "state") !== expectedMembershipState ||
          (expectedMembershipState === "abandoned" && nullableText(value, "abandoned_reason") === null)
        ) {
          throw new ProjectFabricCoreError(
            "RECOVERY_REQUIRED",
            `required ${kind} membership has the wrong terminal disposition`,
          );
        }
      }
      if (lifecycle === "quiescing" && this.#database.prepare(`
        SELECT 1 FROM run_chair_leases
         WHERE project_session_id=? AND run_id=? AND lease_id=? AND generation=?
           AND status IN ('active','frozen')
      `).get(
        projectSessionId,
        runId,
        text(run, "chair_lease_id"),
        integer(run, "chair_generation"),
      ) === undefined) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "current chair lease is unavailable for terminal reconciliation");
      }
    }
    const blockers: ReadonlyArray<readonly [string, string]> = [
      ["task", `SELECT 1 FROM tasks task JOIN runs run ON run.run_id=task.run_id
        WHERE run.project_session_id=? AND task.state NOT IN ('complete','cancelled','degraded') LIMIT 1`],
      ["write lease", `SELECT 1 FROM leases lease JOIN runs run ON run.run_id=lease.run_id
        WHERE run.project_session_id=? AND lease.status IN ('active','quarantined') LIMIT 1`],
      ["task-owner lease", `SELECT 1 FROM task_owner_leases
        WHERE project_session_id=? AND status IN ('active','frozen') LIMIT 1`],
      ["provider action", `SELECT 1 FROM provider_actions action JOIN runs run ON run.run_id=action.run_id
        WHERE run.project_session_id=? AND action.status IN ('prepared','dispatched','accepted','ambiguous','quarantined') LIMIT 1`],
      ["required result", `SELECT 1 FROM result_deliveries
        WHERE project_session_id=? AND required=1 AND state NOT IN ('consumed','abandoned') LIMIT 1`],
      ["gate", `SELECT 1 FROM scoped_gates
        WHERE project_session_id=? AND status IN ('pending','deferred') LIMIT 1`],
      ["barrier", `SELECT 1 FROM barriers barrier JOIN runs run ON run.run_id=barrier.run_id
        WHERE run.project_session_id=? AND barrier.state<>'closed' LIMIT 1`],
      ["artifact obligation", `SELECT 1 FROM task_expected_artifacts expected
        JOIN runs run ON run.run_id=expected.run_id
        JOIN tasks task ON task.run_id=expected.run_id AND task.task_id=expected.task_id
        WHERE run.project_session_id=? AND task.state NOT IN ('cancelled','degraded') AND NOT EXISTS (
          SELECT 1 FROM artifacts artifact WHERE artifact.run_id=expected.run_id
            AND artifact.task_id=expected.task_id AND artifact.relative_path=expected.relative_path
        ) LIMIT 1`],
      ["agent context", `SELECT 1 FROM agents agent JOIN runs run ON run.run_id=agent.run_id
        WHERE run.project_session_id=? AND agent.lifecycle='context-unreconciled' LIMIT 1`],
    ];
    for (const [label, sql] of blockers) {
      if (this.#database.prepare(sql).get(projectSessionId) !== undefined) {
        throw new ProjectFabricCoreError(
          "BARRIER_PRECONDITION_FAILED",
          `${label} remains unresolved before project acceptance`,
        );
      }
    }
    if (this.#database.prepare(`
      SELECT 1 FROM project_session_memberships membership
       WHERE membership.project_session_id=? AND membership.required=1 AND membership.state='active'
         AND NOT (
           membership.member_kind='coordination-run' AND membership.member_id=membership.coordination_run_id
         )
         AND NOT (
           membership.member_kind='lease' AND EXISTS (
             SELECT 1 FROM runs run
              WHERE run.project_session_id=membership.project_session_id
                AND run.run_id=membership.coordination_run_id
                AND run.chair_lease_id=membership.member_id
           )
         )
       LIMIT 1
    `).get(projectSessionId) !== undefined) {
      throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", "required project-session membership remains active");
    }
    if (this.#database.prepare(`
      SELECT 1 FROM operator_effect_custody
       WHERE project_session_id=? AND state IN ('prepared','dispatching','conflict','ambiguous','quarantined','failed')
       LIMIT 1
    `).get(projectSessionId) !== undefined) {
      throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", "unresolved operator-effect custody remains active");
    }

    const now = this.#clock();
    for (const run of runs) {
      if (text(run, "lifecycle_state") !== "quiescing") continue;
      const runId = text(run, "run_id");
      const changed = this.#database.prepare(`
        UPDATE runs SET lifecycle_state='awaiting_acceptance', revision=revision+1
         WHERE run_id=? AND revision=? AND lifecycle_state='quiescing'
      `).run(runId, integer(run, "revision"));
      if (changed.changes !== 1) {
        throw new ProjectFabricCoreError("STALE_REVISION", "coordination run changed during acceptance preparation");
      }
      const frozen = this.#database.prepare(`
        UPDATE run_chair_leases SET status='frozen', updated_at=?
         WHERE project_session_id=? AND run_id=? AND lease_id=? AND generation=?
           AND status IN ('active','frozen')
      `).run(
        now,
        projectSessionId,
        runId,
        text(run, "chair_lease_id"),
        integer(run, "chair_generation"),
      );
      if (frozen.changes !== 1) {
        throw new ProjectFabricCoreError("STALE_LEASE_GENERATION", "chair lease changed during acceptance preparation");
      }
    }
    this.#fault("session:acceptance:after-runs");
    const membership = this.#database.prepare(`
      UPDATE project_session_memberships
         SET state='reconciled', revision=revision+1, updated_at=?
       WHERE project_session_id=? AND required=1 AND state='active' AND (
         (member_kind='coordination-run' AND member_id=coordination_run_id) OR
         (member_kind='lease' AND EXISTS (
           SELECT 1 FROM runs run
            WHERE run.project_session_id=project_session_memberships.project_session_id
              AND run.run_id=project_session_memberships.coordination_run_id
              AND run.chair_lease_id=project_session_memberships.member_id
              AND run.lifecycle_state='awaiting_acceptance'
         ))
       )
    `).run(now, projectSessionId);
    this.#fault("session:acceptance:after-memberships");
    this.#assertClosure(projectSessionId);
    return membership.changes > 0;
  }

  #reopenFromAcceptance(
    projectSessionId: string,
    commandId: string,
    sourceState: "quiescing" | "awaiting_acceptance",
  ): boolean {
    const runs = this.#database.prepare(`
      SELECT run_id, revision, chair_lease_id, chair_generation, lifecycle_state
        FROM runs WHERE project_session_id=? ORDER BY run_id
    `).all(projectSessionId) as Row[];
    if (runs.length === 0) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "project session has no coordination run to reopen");
    }
    const now = this.#clock();
    const sourceRuns = runs.filter((run) => text(run, "lifecycle_state") === sourceState);
    if (sourceRuns.length === 0) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "project session has no acceptance-cycle run to resume");
    }
    for (const run of runs) {
      if (![sourceState, "closed", "cancelled", "launch_failed"].includes(text(run, "lifecycle_state"))) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "project session has a nonterminal run outside acceptance");
      }
    }
    const superseded = supersedeFinalAcceptanceGates({
      database: this.#database,
      projectSessionId,
      cause: { kind: "operator-command", ref: commandId },
      reason: "project session exited its acceptance cycle",
      now,
    });
    this.#fault("session:reopen:after-gates");
    for (const run of runs) {
      if (text(run, "lifecycle_state") !== sourceState) continue;
      const runId = text(run, "run_id");
      const changed = this.#database.prepare(`
        UPDATE runs SET lifecycle_state='active', revision=revision+1
         WHERE run_id=? AND revision=? AND lifecycle_state=?
      `).run(runId, integer(run, "revision"), sourceState);
      if (changed.changes !== 1) {
        throw new ProjectFabricCoreError("STALE_REVISION", "coordination run changed during project reopen");
      }
      const activated = this.#database.prepare(`
        UPDATE run_chair_leases SET status='active', updated_at=?
         WHERE project_session_id=? AND run_id=? AND lease_id=? AND generation=?
           AND status IN ('active','frozen')
      `).run(
        now,
        projectSessionId,
        runId,
        text(run, "chair_lease_id"),
        integer(run, "chair_generation"),
      );
      if (activated.changes !== 1) {
        throw new ProjectFabricCoreError("STALE_LEASE_GENERATION", "chair lease changed during project reopen");
      }
    }
    this.#fault("session:reopen:after-runs");
    const memberships = this.#database.prepare(`
      UPDATE project_session_memberships
         SET state='active', revision=revision+1, updated_at=?
       WHERE project_session_id=? AND required=1 AND state='reconciled' AND (
         (member_kind='coordination-run' AND member_id=coordination_run_id) OR
         (member_kind='lease' AND EXISTS (
           SELECT 1 FROM runs run
            WHERE run.project_session_id=project_session_memberships.project_session_id
              AND run.run_id=project_session_memberships.coordination_run_id
              AND run.chair_lease_id=project_session_memberships.member_id
              AND run.lifecycle_state='active'
         ))
       )
    `).run(now, projectSessionId);
    this.#fault("session:reopen:after-memberships");
    return memberships.changes + superseded.membershipChanges + superseded.gateChanges > 0;
  }

  #divertFromAcceptanceCycle(
    projectSessionId: string,
    commandId: string,
    sourceState: "quiescing" | "awaiting_acceptance",
    targetState: ProjectSessionState,
  ): boolean {
    const now = this.#clock();
    const superseded = supersedeFinalAcceptanceGates({
      database: this.#database,
      projectSessionId,
      cause: { kind: "operator-command", ref: commandId },
      reason: `project session diverted from ${sourceState} to ${targetState}`,
      now,
    });
    const runs = this.#database.prepare(`
      SELECT run_id, revision, chair_lease_id, chair_generation, lifecycle_state
        FROM runs WHERE project_session_id=? ORDER BY run_id
    `).all(projectSessionId) as Row[];
    const affectedRuns = runs.filter((run) => text(run, "lifecycle_state") === sourceState);
    if (affectedRuns.length === 0) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "project session has no acceptance-cycle run to divert");
    }
    for (const run of runs) {
      if (![sourceState, "closed", "cancelled", "launch_failed"].includes(text(run, "lifecycle_state"))) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "project session has an incompatible run during diversion");
      }
    }
    let membershipChanges = superseded.membershipChanges + superseded.gateChanges;
    for (const run of affectedRuns) {
      const runId = text(run, "run_id");
      const changed = this.#database.prepare(`
        UPDATE runs SET lifecycle_state=?, revision=revision+1
         WHERE run_id=? AND revision=? AND lifecycle_state=?
      `).run(targetState, runId, integer(run, "revision"), sourceState);
      if (changed.changes !== 1) {
        throw new ProjectFabricCoreError("STALE_REVISION", "coordination run changed during acceptance diversion");
      }
      const leaseStatus = targetState === "cancelled" ? "revoked" : "frozen";
      const frozen = this.#database.prepare(`
        UPDATE run_chair_leases SET status=?, updated_at=?
         WHERE project_session_id=? AND run_id=? AND lease_id=? AND generation=?
           AND status IN ('active','frozen')
      `).run(
        leaseStatus,
        now,
        projectSessionId,
        runId,
        text(run, "chair_lease_id"),
        integer(run, "chair_generation"),
      );
      if (frozen.changes !== 1) {
        throw new ProjectFabricCoreError("STALE_LEASE_GENERATION", "chair lease changed during acceptance diversion");
      }
      membershipChanges += this.#syncAcceptanceRunMemberships(
        projectSessionId,
        runId,
        text(run, "chair_lease_id"),
        sourceState,
        now,
      );
    }
    this.#fault("session:quiesce-exit:after-runs");
    return membershipChanges > 0;
  }

  #syncAcceptanceRunMemberships(
    projectSessionId: string,
    runId: string,
    chairLeaseId: string,
    sourceState: "quiescing" | "awaiting_acceptance",
    now: number,
  ): number {
    const expectedSourceState = sourceState === "quiescing" ? "active" : "reconciled";
    let changes = 0;
    for (const [kind, memberId] of [
      ["coordination-run", runId],
      ["lease", chairLeaseId],
    ] as const) {
      const membership = row(this.#database.prepare(`
        SELECT state, revision FROM project_session_memberships
         WHERE project_session_id=? AND coordination_run_id=?
           AND member_kind=? AND member_id=? AND required=1
      `).get(projectSessionId, runId, kind, memberId), `${kind} membership`);
      if (text(membership, "state") !== expectedSourceState) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", `${kind} membership is outside the acceptance cycle`);
      }
      const disposition = membershipSourceDisposition(
        this.#database,
        projectSessionId,
        runId,
        kind,
        memberId,
      );
      if (disposition.state === expectedSourceState) continue;
      const changed = this.#database.prepare(`
        UPDATE project_session_memberships
           SET state=?, abandoned_reason=?, revision=revision+1, updated_at=?
         WHERE project_session_id=? AND coordination_run_id=?
           AND member_kind=? AND member_id=? AND required=1 AND revision=? AND state=?
      `).run(
        disposition.state,
        disposition.state === "abandoned" ? disposition.reason : null,
        now,
        projectSessionId,
        runId,
        kind,
        memberId,
        integer(membership, "revision"),
        expectedSourceState,
      );
      if (changed.changes !== 1) {
        throw new ProjectFabricCoreError("STALE_REVISION", `${kind} membership changed during acceptance diversion`);
      }
      changes += 1;
    }
    return changes;
  }

  #transitionCoupledRuns(
    projectSessionId: string,
    sourceState: ProjectSessionState,
    targetState: ProjectSessionState,
  ): void {
    if (this.#database.prepare(`
      SELECT 1 FROM launched_chair_bridge_state
       WHERE project_session_id=? AND state='lost'
       LIMIT 1
    `).get(projectSessionId) !== undefined) {
      throw new ProjectFabricCoreError(
        "RECOVERY_REQUIRED",
        "lost chair bridge lifecycle is owned by chair-recovery custody",
      );
    }
    const runs = this.#database.prepare(`
      SELECT run_id, revision, chair_lease_id, chair_generation, lifecycle_state
        FROM runs WHERE project_session_id=? ORDER BY run_id
    `).all(projectSessionId) as Row[];
    const sourceRunStates = sourceState === "visibility_degraded"
      ? new Set<ProjectSessionState>(["visibility_degraded", "active"])
      : new Set<ProjectSessionState>([sourceState]);
    const affected = runs.filter((run) => sourceRunStates.has(text(run, "lifecycle_state") as ProjectSessionState));
    if (affected.length === 0) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "exceptional session has no run owned by its state");
    }
    const allowedUnaffected = new Set(["active", "closed", "cancelled", "launch_failed"]);
    for (const run of runs) {
      const lifecycle = text(run, "lifecycle_state");
      if (!sourceRunStates.has(lifecycle as ProjectSessionState) && !allowedUnaffected.has(lifecycle)) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "exceptional session contains an incompatible run state");
      }
    }
    const now = this.#clock();
    for (const run of affected) {
      const runId = text(run, "run_id");
      if ((targetState === "active" || targetState === "visibility_degraded") && this.#database.prepare(`
        SELECT 1 FROM capabilities capability JOIN runs current ON current.run_id=capability.run_id
         WHERE capability.run_id=? AND capability.agent_id=current.chair_agent_id
           AND capability.revoked_at IS NULL AND capability.expires_at>?
         LIMIT 1
      `).get(runId, now) === undefined) {
        throw new ProjectFabricCoreError(
          "RECOVERY_REQUIRED",
          "exceptional run has no live current-chair capability",
        );
      }
      if ((targetState === "active" || targetState === "visibility_degraded") && this.#database.prepare(`
        SELECT 1
          FROM project_session_memberships run_membership
          JOIN project_session_memberships lease_membership
            ON lease_membership.project_session_id=run_membership.project_session_id
           AND lease_membership.coordination_run_id=run_membership.coordination_run_id
         WHERE run_membership.project_session_id=?
           AND run_membership.coordination_run_id=?
           AND run_membership.member_kind='coordination-run'
           AND run_membership.member_id=?
           AND run_membership.required=1 AND run_membership.state='active'
           AND lease_membership.member_kind='lease'
           AND lease_membership.member_id=?
           AND lease_membership.required=1 AND lease_membership.state='active'
         LIMIT 1
      `).get(projectSessionId, runId, runId, text(run, "chair_lease_id")) === undefined) {
        throw new ProjectFabricCoreError(
          "RECOVERY_REQUIRED",
          "exceptional run is missing active run or current-chair membership",
        );
      }
      const changed = this.#database.prepare(`
        UPDATE runs SET lifecycle_state=?, revision=revision+1
         WHERE run_id=? AND revision=? AND lifecycle_state=?
      `).run(targetState, runId, integer(run, "revision"), text(run, "lifecycle_state"));
      if (changed.changes !== 1) {
        throw new ProjectFabricCoreError("STALE_REVISION", "exceptional coordination run changed");
      }
      const leaseStatus = targetState === "active" || targetState === "visibility_degraded"
        ? "active"
        : "frozen";
      const lease = this.#database.prepare(`
        UPDATE run_chair_leases SET status=?, updated_at=?
         WHERE project_session_id=? AND run_id=? AND lease_id=? AND generation=?
           AND status IN ('active','frozen')
      `).run(
        leaseStatus,
        now,
        projectSessionId,
        runId,
        text(run, "chair_lease_id"),
        integer(run, "chair_generation"),
      );
      if (lease.changes !== 1) {
        throw new ProjectFabricCoreError("STALE_LEASE_GENERATION", "exceptional chair lease changed");
      }
    }
    this.#fault("session:coupled-transition:after-runs");
  }

  #assertClosure(projectSessionId: string): void {
    const blocker = this.#database.prepare(`
      SELECT member_kind, member_id FROM project_session_memberships
       WHERE project_session_id=? AND required=1 AND state='active' LIMIT 1
    `).get(projectSessionId);
    if (blocker !== undefined) {
      throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", "required project-session membership remains active");
    }
    if (this.#database.prepare(`
      SELECT 1 FROM operator_effect_custody
       WHERE project_session_id=? AND state IN ('prepared','dispatching','conflict','ambiguous','quarantined','failed')
       LIMIT 1
    `).get(projectSessionId) !== undefined) {
      throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", "unresolved operator-effect custody remains active");
    }
  }

  #assertNoOpenLiveHandoff(projectSessionId: string): void {
    if (this.#database.prepare(`
      SELECT 1 FROM chair_live_handoff_custody
       WHERE project_session_id=? AND state NOT IN ('terminal','no-effect') LIMIT 1
    `).get(projectSessionId) !== undefined) {
      throw new ProjectFabricCoreError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "chair live handoff custody owns this project-session mutation",
      );
    }
  }

  #assertRun(projectSessionId: string, runId: string): void {
    if (this.#database.prepare(`
      SELECT 1 FROM runs WHERE project_session_id=? AND run_id=?
    `).get(projectSessionId, runId) === undefined) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "coordination run is outside the project session");
    }
  }

  #assertChairMembershipAuthority(
    context: AuthenticatedAgentContext,
    command: ChairMutationContext,
  ): void {
    if (
      context.projectSessionId !== command.projectSessionId ||
      context.coordinationRunId !== command.coordinationRunId
    ) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "agent context is bound to another project session or run");
    }
    const runValue = this.#database.prepare(`
      SELECT project_session_id, chair_agent_id, chair_generation, chair_lease_id, revision
        FROM runs WHERE run_id=?
    `).get(context.coordinationRunId);
    if (runValue === undefined) {
      throw new ProjectFabricCoreError("NOT_FOUND", "membership coordination run was not found");
    }
    const run = row(runValue, "membership coordination run");
    if (text(run, "project_session_id") !== context.projectSessionId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "coordination run is outside the authenticated project session");
    }
    if (text(run, "chair_agent_id") !== context.agentId) {
      throw new ProjectFabricCoreError("TASK_NOT_OWNER", "authenticated agent is not the active membership chair");
    }
    const leaseValue = this.#database.prepare(`
      SELECT lease_id, holder_agent_id, generation, status
        FROM run_chair_leases
       WHERE project_session_id=? AND run_id=? AND lease_id=? AND generation=?
    `).get(
      context.projectSessionId,
      context.coordinationRunId,
      text(run, "chair_lease_id"),
      integer(run, "chair_generation"),
    );
    if (leaseValue === undefined) {
      throw new ProjectFabricCoreError("STALE_LEASE_GENERATION", "active chair lease binding was not found");
    }
    const lease = row(leaseValue, "membership chair lease");
    if (text(lease, "holder_agent_id") !== context.agentId || text(lease, "status") !== "active") {
      throw new ProjectFabricCoreError("TASK_NOT_OWNER", "authenticated agent does not hold the active chair lease");
    }
    const capabilityValue = this.#database.prepare(`
      SELECT 1 FROM capabilities
       WHERE run_id=? AND agent_id=? AND principal_generation=?
         AND revoked_at IS NULL AND expires_at>?
       LIMIT 1
    `).get(context.coordinationRunId, context.agentId, context.principalGeneration, this.#clock());
    if (capabilityValue === undefined) {
      throw new ProjectFabricCoreError(
        "STALE_PRINCIPAL_GENERATION",
        "chair principal generation is stale, expired or revoked",
      );
    }
    const current = {
      agentId: context.agentId,
      projectSessionId: context.projectSessionId,
      coordinationRunId: context.coordinationRunId,
      principalGeneration: context.principalGeneration,
      chairLeaseId: parseIdentifier<"LeaseId">(text(lease, "lease_id"), "membership.chairLeaseId"),
      chairLeaseGeneration: integer(lease, "generation"),
      runRevision: integer(run, "revision"),
    };
    if (command.agentId !== current.agentId) {
      throw new ProjectFabricCoreError("TASK_NOT_OWNER", "chair command authenticated agent is not the current chair");
    }
    if (command.principalGeneration !== current.principalGeneration) {
      throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "chair command principal generation is stale");
    }
    if (
      command.chairLeaseId !== current.chairLeaseId ||
      command.chairLeaseGeneration !== current.chairLeaseGeneration
    ) {
      throw new ProjectFabricCoreError("STALE_LEASE_GENERATION", "chair command lease generation is stale");
    }
    if (command.expectedRunRevision !== current.runRevision) {
      throw new ProjectFabricCoreError("STALE_REVISION", "chair command run revision is stale");
    }
    assertChairMutationAuthority(command, current);
  }

  #assertMemberTarget(
    projectSessionId: string,
    runId: string,
    member: ProjectSessionMember,
  ): void {
    try {
      membershipSourceDisposition(
        this.#database,
        projectSessionId,
        runId,
        member.kind,
        this.#memberId(member),
        this.#memberAdapterId(member),
      );
    } catch (error: unknown) {
      if (
        (error instanceof ProjectFabricCoreError && error.code === "NOT_FOUND") ||
        (error instanceof Error && !(error instanceof ProjectFabricCoreError) && / was not found$/u.test(error.message))
      ) {
        throw new ProjectFabricCoreError("NOT_FOUND", `${member.kind} membership target was not found`);
      }
      throw error;
    }
  }

  #assertMemberDisposition(
    projectSessionId: string,
    runId: string,
    member: ProjectSessionMember,
  ): void {
    const source = membershipSourceDisposition(
      this.#database,
      projectSessionId,
      runId,
      member.kind,
      this.#memberId(member),
      this.#memberAdapterId(member),
    );
    const expected = member.state === "terminal" ? "reconciled" : member.state;
    if (source.state === expected) return;
    throw new ProjectFabricCoreError(
      "LIFECYCLE_PRECONDITION_FAILED",
      `${member.kind} membership disposition does not match its durable source state`,
    );
  }

  #memberId(member: ProjectSessionMember): string {
    switch (member.kind) {
      case "coordination-run": return member.runId;
      case "workstream": return member.workstreamId;
      case "task": return member.taskId;
      case "lease": return member.leaseId;
      case "provider-action": return member.providerActionId;
      case "required-message": return member.messageId;
      case "artifact-obligation": return member.artifactObligationId;
      case "gate": return member.gateId;
      case "scoped-barrier": return member.barrierId;
    }
  }

  #memberAdapterId(member: ProjectSessionMember): string {
    return member.kind === "provider-action" ? member.providerAdapterId : "";
  }
}
