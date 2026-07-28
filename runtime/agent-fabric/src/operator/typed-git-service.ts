import { isPreauthorisedGitOperationVariant, parseCanonicalRelativePath } from "@local/agent-fabric-protocol";
import type {
  GitActionGrant,
  GitAuthoriseIntent,
  GitConflictReconcileBinding,
  GitCustodyResolveIntent,
  GitCustodyStatus,
  GitCurrentState,
  GitLookupOutcome,
  GitOperation,
  GitOperationDraftIntent,
  GitRemoteBinding,
  GitRepositoryBinding,
  GitResultRecipeV1,
  JsonValue,
  OperatorActionStatus,
  OperatorGitIntent,
  Sha256Digest,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { canonicalJson, integer, isRow, nullableText, row, sha256, text, type Row } from "../persistence/row-codec.js";
import type { OperatorEffectOutcome } from "./action-store.js";
import type { GitMutationInspection, GitMutationPort } from "./fixed-git-mutation-port.js";

export type TypedGitEffectRequest = {
  commandId: string;
  previewId: string;
  operatorId: string;
  projectId: string;
  projectSessionId: string;
  principalGeneration: number;
  operation: string;
  intent: OperatorGitIntent;
  intentDigest: Sha256Digest;
  beforeStateDigest: Sha256Digest;
  attemptGeneration: number;
};

export type TypedGitAdministrativeIntent = GitAuthoriseIntent | GitOperationDraftIntent | GitCustodyResolveIntent;

export type TypedGitAdministrativeRequest = Omit<TypedGitEffectRequest, "intent"> & {
  intent: TypedGitAdministrativeIntent;
  operatorInputRecordDigest: Sha256Digest;
};

export type TypedGitServiceOptions = {
  database: Database.Database;
  gitPort: GitMutationPort;
  conflictInspector?: GitConflictInspectorPort;
  materializeTrustedRunAllowlist?: (identity: Readonly<{
    projectSessionId: string;
    coordinationRunId: string;
  }>) => void;
  clock?: () => number;
  daemonInstanceId: string;
};

/** Read-only, no-process conflict reader. It must never dispatch Git. */
export interface GitConflictInspectorPort {
  inspect(intent: OperatorGitIntent): Promise<GitMutationInspection>;
}

export function deriveGitResultRecipeDigest(
  recipe: Omit<GitResultRecipeV1, "resultRecipeDigest">,
): Sha256Digest {
  return digest({ domain: "git-result-recipe-v1", recipe });
}

export function deriveGitGrantDigest(
  grant: Omit<GitActionGrant, "grantDigest">,
): Sha256Digest {
  return digest({ domain: "git-action-grant-v1", grant });
}

export function deriveGitEffectBindingDigest(value: Readonly<{
  projectId: string;
  projectSessionId: string;
  coordinationRunId: string;
  authorityRef: Sha256Digest;
  authorityRevision: number;
  gitAllowlistEpoch: number;
  gitAllowlistDigest: Sha256Digest | null;
  repository: GitRepositoryBinding;
  executionProfile: OperatorGitIntent["executionProfile"];
  remoteBinding: GitRemoteBinding | null;
  operation: GitOperation;
  resultRecipeDigest: Sha256Digest;
}>): Sha256Digest {
  return digest({ domain: "git-effect-binding-v1", ...value });
}

export function derivePreauthorisedGitOperationId(value: Readonly<{
  operatorId: string;
  projectId: string;
  projectSessionId: string;
  previewId: string;
  effectBindingDigest: Sha256Digest;
}>): string {
  return `git-operation-${sha256(canonicalJson({ domain: "git-preauthorised-operation-v1", ...value })).slice(0, 48)}`;
}

export class TypedGitService {
  readonly #database: Database.Database;
  readonly #gitPort: GitMutationPort;
  readonly #conflictInspector: GitConflictInspectorPort | undefined;
  readonly #materializeTrustedRunAllowlist: TypedGitServiceOptions["materializeTrustedRunAllowlist"];
  readonly #clock: () => number;
  readonly #daemonInstanceId: string;

  constructor(options: TypedGitServiceOptions) {
    this.#database = options.database;
    this.#gitPort = options.gitPort;
    this.#conflictInspector = options.conflictInspector;
    this.#materializeTrustedRunAllowlist = options.materializeTrustedRunAllowlist;
    this.#clock = options.clock ?? Date.now;
    this.#daemonInstanceId = options.daemonInstanceId;
  }

  custodyId(request: Pick<TypedGitEffectRequest,
    "operatorId" | "projectId" | "projectSessionId" | "principalGeneration" | "operation" | "commandId"
  >): string {
    return `operator-effect-${sha256(canonicalJson({
      operatorId: request.operatorId,
      projectId: request.projectId,
      projectSessionId: request.projectSessionId,
      principalGeneration: request.principalGeneration,
      operation: request.operation,
      commandId: request.commandId,
    })).slice(0, 48)}`;
  }

  async readCurrentState(intent: OperatorGitIntent): Promise<GitCurrentState> {
    this.#gitPort.assertAvailable(intent);
    this.#materializeTrustedRunAllowlist?.(intent.authorisation);
    const authority = this.#authority(intent);
    const repository = await this.#gitPort.observe(intent.repository.repositoryRoot, intent.repository.worktreePath);
    const profile = this.#profile(intent);
    const remoteBinding = this.#remote(intent);
    const grant = intent.authorisation.decision.kind === "preauthorised"
      ? this.#grant(intent)
      : null;
    return {
      revision: authority.sessionRevision,
      projectId: intent.authorisation.projectId,
      projectSessionId: intent.authorisation.projectSessionId,
      sessionRevision: authority.sessionRevision,
      sessionGeneration: authority.sessionGeneration,
      coordinationRunId: intent.authorisation.coordinationRunId,
      runRevision: authority.runRevision,
      dependencyRevision: authority.dependencyRevision,
      authorityRef: authority.authorityRef as Sha256Digest,
      authorityRevision: authority.authorityRevision,
      gitAllowlistEpoch: authority.gitAllowlistEpoch,
      gitAllowlistDigest: authority.gitAllowlistDigest as Sha256Digest | null,
      repository,
      executionProfile: profile,
      remoteBinding,
      grant,
    };
  }

  readAdministrativeCurrentState(intent: TypedGitAdministrativeIntent): { revision: number; state: JsonValue } {
    this.#materializeTrustedRunAllowlist?.(administrativeBinding(intent));
    const authority = this.#administrativeAuthority(intent);
    return {
      revision: authority.sessionRevision,
      state: {
        projectId: authority.projectId,
        sessionRevision: authority.sessionRevision,
        sessionGeneration: authority.sessionGeneration,
        runRevision: authority.runRevision,
        dependencyRevision: authority.dependencyRevision,
        authorityRef: authority.authorityRef,
        authorityRevision: authority.authorityRevision,
        gitAllowlistEpoch: authority.gitAllowlistEpoch,
        gitAllowlistDigest: authority.gitAllowlistDigest,
      },
    };
  }

  prepareAdministrative(request: TypedGitAdministrativeRequest): void {
    this.#materializeTrustedRunAllowlist?.(administrativeBinding(request.intent));
    this.#database.transaction(() => {
      this.#administrativeAuthority(request.intent);
      switch (request.intent.kind) {
        case "git-authorise":
          this.#applyGrantAdministration(request.intent, request.operatorInputRecordDigest);
          return;
        case "git-operation-draft":
          this.#applyDraftAdministration(request.intent, request);
          return;
        case "git-custody-resolve":
          this.#applyCustodyResolution(
            request.intent,
            request.commandId,
            request.operatorId,
            request.operatorInputRecordDigest,
          );
          return;
      }
    })();
  }

  administrativeOutcome(intent: TypedGitAdministrativeIntent): OperatorEffectOutcome {
    if (intent.kind === "git-authorise") {
      const grant = intent.action === "revoke" ? intent.currentGrant : intent.proposedGrant;
      const value = row(this.#database.prepare(
        "SELECT state,revision,grant_digest FROM operator_git_grants WHERE grant_id=? AND revision=?",
      ).get(grant.grantId, grant.revision), "typed Git administrative grant");
      return { status: "committed", afterState: {
        kind: "git-authorise",
        action: intent.action,
        grantId: grant.grantId,
        revision: integer(value, "revision"),
        grantDigest: text(value, "grant_digest"),
        state: text(value, "state"),
      } };
    }
    if (intent.kind === "git-operation-draft") {
      const draftId = intent.action === "cancel"
        ? intent.draftId
        : deriveGitDraftId(administrativeDraftIdentity(intent), intent.draftRequestId);
      const value = row(this.#database.prepare(
        "SELECT draft_id,revision,operation_id,draft_digest,state FROM git_operation_drafts WHERE draft_id=?",
      ).get(draftId), "typed Git administrative draft");
      return { status: "committed", afterState: {
        kind: "git-operation-draft",
        draftId: text(value, "draft_id"),
        revision: integer(value, "revision"),
        operationId: text(value, "operation_id"),
        draftDigest: text(value, "draft_digest"),
        state: text(value, "state"),
      } };
    }
    const value = row(this.#database.prepare(`
      SELECT resolution_id,resolution_digest,adjudication FROM git_custody_resolutions
       WHERE draft_id=? AND target_custody_id=?
    `).get(intent.draftId, intent.custodyId), "typed Git custody resolution");
    return { status: "committed", afterState: {
      kind: `human-adjudicated-${text(value, "adjudication")}`,
      resolutionId: text(value, "resolution_id"),
      resolutionDigest: text(value, "resolution_digest"),
    } };
  }

  prepare(request: TypedGitEffectRequest): void {
    const intent = request.intent;
    this.#gitPort.assertAvailable(intent);
    this.#materializeTrustedRunAllowlist?.(intent.authorisation);
    if (request.operation !== "git") {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git custody accepts only the Git action family");
    }
    if (
      request.projectId !== intent.authorisation.projectId ||
      request.projectSessionId !== intent.authorisation.projectSessionId
    ) throw new ProjectFabricCoreError("WRONG_PROJECT", "typed Git request scope changed");
    const custodyId = this.custodyId(request);
    const transaction = this.#database.transaction(() => {
      const existing = this.#database.prepare("SELECT custody_id FROM operator_git_effect_bindings WHERE custody_id=?").get(custodyId);
      if (isRow(existing)) {
        this.#assertBindingReplay(custodyId, request);
        return;
      }
      this.#assertGenericCustody(custodyId, request);
      const authority = this.#authority(intent);
      const profile = this.#profile(intent);
      const remote = this.#remote(intent);
      const expectedRecipeDigest = deriveGitResultRecipeDigest(stripRecipeDigest(intent.resultRecipe));
      if (expectedRecipeDigest !== intent.resultRecipe.resultRecipeDigest) {
        throw new ProjectFabricCoreError("CONFLICT", "typed Git result recipe digest is invalid");
      }
      const expectedEffectDigest = deriveGitEffectBindingDigest({
        projectId: intent.authorisation.projectId,
        projectSessionId: intent.authorisation.projectSessionId,
        coordinationRunId: intent.authorisation.coordinationRunId,
        authorityRef: intent.authorisation.authorityRef,
        authorityRevision: intent.authorisation.expectedAuthorityRevision,
        gitAllowlistEpoch: intent.authorisation.expectedGitAllowlistEpoch,
        gitAllowlistDigest: intent.authorisation.gitAllowlistDigest,
        repository: intent.repository,
        executionProfile: intent.executionProfile,
        remoteBinding: remote,
        operation: intent.operation,
        resultRecipeDigest: intent.resultRecipe.resultRecipeDigest,
      });
      if (expectedEffectDigest !== intent.authorisation.effectBindingDigest) {
        throw new ProjectFabricCoreError("CONFLICT", "typed Git effect binding digest is invalid");
      }
      this.#writerAdmission(intent);
      const decision = intent.authorisation.decision;
      let grantId: string | null = null;
      let grantRevision: number | null = null;
      let draftId: string | null = null;
      let draftRevision: number | null = null;
      let gateId: string | null = null;
      let gateRevision: number | null = null;
      if (decision.kind === "preauthorised") {
        const expectedOperationId = derivePreauthorisedGitOperationId({
          operatorId: request.operatorId,
          projectId: request.projectId,
          projectSessionId: request.projectSessionId,
          previewId: request.previewId,
          effectBindingDigest: intent.authorisation.effectBindingDigest,
        });
        if (intent.authorisation.operationId !== expectedOperationId) {
          throw new ProjectFabricCoreError("CONFLICT", "typed Git operation ID was not daemon-derived from the Preview binding");
        }
        const grant = this.#grant(intent);
        this.#assertGrantConstraints(intent, grant);
        grantId = grant.grantId;
        grantRevision = grant.revision;
        this.#database.prepare(`
          INSERT INTO operation_admissions(
            operation_id,project_session_id,coordination_run_id,operation_kind,state,revision,payload_digest
          ) VALUES(?,?,?,?, 'authorised',1,?)
        `).run(
          intent.authorisation.operationId,
          intent.authorisation.projectSessionId,
          intent.authorisation.coordinationRunId,
          intent.operation.variant,
          intent.authorisation.effectBindingDigest,
        );
      } else {
        const draft = row(this.#database.prepare(`
          SELECT * FROM git_operation_drafts WHERE draft_id=? AND operation_id=?
        `).get(decision.draftId, decision.blockedOperationId), "typed Git operation draft");
        if (
          integer(draft, "revision") !== decision.expectedDraftRevision ||
          text(draft, "draft_digest") !== decision.draftDigest ||
          text(draft, "payload_digest") !== intent.authorisation.effectBindingDigest ||
          text(draft, "state") !== "gate-bound" || text(draft, "draft_kind") !== "mutation" ||
          text(draft, "operation_kind") !== intent.operation.variant ||
          text(draft, "operator_id") !== request.operatorId || text(draft, "project_id") !== request.projectId ||
          text(draft, "project_session_id") !== request.projectSessionId ||
          text(draft, "coordination_run_id") !== intent.authorisation.coordinationRunId ||
          text(draft, "binding_json") !== canonicalJson(mutationDraftBinding(intent)) ||
          integer(draft, "expires_at") <= this.#clock()
        ) throw new ProjectFabricCoreError("STALE_REVISION", "typed Git draft changed or expired");
        const gate = row(this.#database.prepare(`
          SELECT g.* FROM scoped_gates g
          JOIN scoped_gate_operations operation ON operation.gate_id=g.gate_id
           WHERE g.gate_id=? AND operation.operation_id=?
        `).get(decision.gateId, decision.blockedOperationId), "typed Git operation gate");
        if (
          integer(gate, "revision") !== decision.expectedGateRevision ||
          text(gate, "status") !== "approved" || integer(gate, "human_required") !== 1 ||
          nullableText(gate, "resolved_by_operator_id") === null ||
          text(gate, "project_session_id") !== intent.authorisation.projectSessionId ||
          text(gate, "coordination_run_id") !== intent.authorisation.coordinationRunId ||
          integer(gate, "dependency_revision") !== intent.authorisation.expectedDependencyRevision
        ) throw new ProjectFabricCoreError("GATE_BLOCKED", "typed Git gate is not exact and human-approved");
        const admission = row(this.#database.prepare("SELECT * FROM operation_admissions WHERE operation_id=?")
          .get(decision.blockedOperationId), "typed Git prepared admission");
        if (
          text(admission, "state") !== "prepared" ||
          text(admission, "operation_kind") !== intent.operation.variant ||
          text(admission, "payload_digest") !== intent.authorisation.effectBindingDigest
        ) throw new ProjectFabricCoreError("CONFLICT", "typed Git draft admission changed");
        this.#database.prepare(`
          UPDATE operation_admissions SET state='authorised',revision=revision+1
           WHERE operation_id=? AND state='prepared'
        `).run(decision.blockedOperationId);
        this.#database.prepare(`
          UPDATE git_operation_drafts
             SET state='consumed',revision=revision+1,consumed_command_id=?,updated_at=?
           WHERE draft_id=? AND state IN ('open','gate-bound')
        `).run(request.commandId, this.#clock(), decision.draftId);
        draftId = decision.draftId;
        draftRevision = decision.expectedDraftRevision;
        gateId = decision.gateId;
        gateRevision = decision.expectedGateRevision;
      }
      this.#assertParentWorktreeCreation(intent);
      const reservationGeneration = this.#prepareReservationTransfer(intent, custodyId);
      const lockPlanDigest = digest({
        commonDirectoryIdentityDigest: intent.repository.commonDirectoryIdentityDigest,
        operationVariant: intent.operation.variant,
        affectedPaths: intent.resultRecipe.affectedPaths.map((path) => path.path),
        refs: intent.resultRecipe.refUpdates.map((update) => update.refName),
        config: intent.resultRecipe.configUpdates,
      });
      this.#database.prepare(`
        INSERT INTO git_mutation_reservations(
          custody_id,generation,project_id,project_session_id,coordination_run_id,git_common_dir,
          common_dir_identity_digest,lock_plan_digest,state,owner_instance_id,created_at,updated_at
        ) VALUES(?,?,?,?,?,?,?,?, 'reserved',?,?,?)
      `).run(
        custodyId,
        reservationGeneration,
        intent.authorisation.projectId,
        intent.authorisation.projectSessionId,
        intent.authorisation.coordinationRunId,
        intent.repository.gitCommonDir,
        intent.repository.commonDirectoryIdentityDigest,
        lockPlanDigest,
        this.#daemonInstanceId,
        this.#clock(),
        this.#clock(),
      );
      this.#database.prepare(`
        INSERT INTO operator_git_effect_bindings(
          custody_id,project_id,project_session_id,prepared_session_revision,session_generation,
          coordination_run_id,prepared_run_revision,prepared_dependency_revision,authority_ref,
          authority_revision,git_allowlist_epoch,git_allowlist_digest,grant_id,grant_revision,draft_id,
          draft_revision,gate_id,gate_revision,repository_root,worktree_path,repository_state_digest,
          execution_profile_id,execution_profile_revision,execution_profile_digest,
          remote_registration_id,remote_registration_revision,remote_generation,remote_target_digest,
          operation_id,operation_variant,effect_binding_digest,result_recipe_digest,decision_digest,
          before_git_state_json,expected_terminal_state_json,state,state_revision,terminal_basis,
          predecessor_custody_id,predecessor_conflict_generation,owned_conflict_generation,
          mutation_reservation_generation,lock_plan_digest,lookup_generation,resolution_eligible,
          created_at,updated_at
        ) VALUES(
          ?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,
          'prepared',1,NULL,?,?,NULL,?,?,0,0,?,?
        )
      `).run(
        custodyId,
        intent.authorisation.projectId,
        intent.authorisation.projectSessionId,
        authority.sessionRevision,
        authority.sessionGeneration,
        intent.authorisation.coordinationRunId,
        authority.runRevision,
        authority.dependencyRevision,
        authority.authorityRef,
        authority.authorityRevision,
        authority.gitAllowlistEpoch,
        authority.gitAllowlistDigest,
        grantId,
        grantRevision,
        draftId,
        draftRevision,
        gateId,
        gateRevision,
        intent.repository.repositoryRoot,
        intent.repository.worktreePath,
        intent.repository.repositoryStateDigest,
        profile.profileId,
        profile.revision,
        profile.digest,
        remote?.registrationId ?? null,
        remote?.revision ?? null,
        remote?.generation ?? null,
        remote?.targetDigest ?? null,
        intent.authorisation.operationId,
        intent.operation.variant,
        intent.authorisation.effectBindingDigest,
        intent.resultRecipe.resultRecipeDigest,
        digest(decision),
        canonicalJson(intent.repository),
        canonicalJson(intent.resultRecipe),
        conflictPredecessor(intent.operation)?.custodyId ?? null,
        conflictPredecessor(intent.operation)?.generation ?? null,
        reservationGeneration,
        lockPlanDigest,
        this.#clock(),
        this.#clock(),
      );
    });
    transaction();
  }

  async dispatch(request: TypedGitEffectRequest): Promise<OperatorEffectOutcome> {
    const custodyId = this.custodyId(request);
    let binding = this.#binding(custodyId);
    const state = text(binding, "state");
    if (isGitTerminalState(state)) return this.#storedTerminalOutcome(custodyId, binding);
    if (state !== "prepared") return this.#ambiguousOutcome(custodyId, binding);
    const current = await this.#gitPort.observe(request.intent.repository.repositoryRoot, request.intent.repository.worktreePath);
    if (canonicalJson(current) !== text(binding, "before_git_state_json")) {
      return this.#terminaliseNoEffect(custodyId, "rejected", {
        status: "rejected",
        code: "git-state-changed",
        evidenceRefs: [],
      });
    }
    let claimed = false;
    const pointOfUse = (): void => {
      if (claimed) throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "typed Git mutation port reused point-of-use authority");
      binding = this.#binding(custodyId);
      this.#database.transaction(() => {
        this.#assertPointOfUse(request, binding);
        this.#assertFourOwnerState(custodyId, "prepared", "prepared", "authorised", "reserved");
        this.#database.prepare("UPDATE operator_git_effect_bindings SET state='dispatching',state_revision=state_revision+1,updated_at=? WHERE custody_id=? AND state='prepared'")
          .run(this.#clock(), custodyId);
        this.#database.prepare("UPDATE operator_effect_custody SET state='dispatching',updated_at=? WHERE custody_id=? AND state='prepared'")
          .run(this.#clock(), custodyId);
        this.#database.prepare("UPDATE operation_admissions SET state='executing',revision=revision+1 WHERE operation_id=? AND state='authorised'")
          .run(text(binding, "operation_id"));
        this.#database.prepare("UPDATE git_mutation_reservations SET state='dispatching',updated_at=? WHERE custody_id=? AND generation=? AND state='reserved'")
          .run(this.#clock(), custodyId, integer(binding, "mutation_reservation_generation"));
      })();
      claimed = true;
    };
    let inspection: GitMutationInspection;
    try {
      inspection = await this.#gitPort.dispatch(
        request.intent,
        { remoteTarget: this.#remoteTarget(binding) },
        pointOfUse,
      );
      if (!claimed) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "typed Git mutation port returned without point-of-use authority");
      }
    } catch (error: unknown) {
      if (!claimed) throw error;
      inspection = {
        outcome: "incomplete",
        repository: await this.#gitPort.observe(request.intent.repository.repositoryRoot, request.intent.repository.worktreePath),
        evidenceDigest: digest({ custodyId, outcome: "incomplete" }),
        failureSignatureDigest: digest({ class: "typed-git-dispatch-error" }),
        conflict: null,
      };
    }
    this.#applyInspection(custodyId, inspection);
    binding = this.#binding(custodyId);
    if (text(binding, "state") === "applied") return this.#storedTerminalOutcome(custodyId, binding);
    if (text(binding, "state") === "no-effect") return this.#storedTerminalOutcome(custodyId, binding);
    return this.#ambiguousOutcome(custodyId, binding);
  }

  /** Recovery/ordinary reconciliation is lookup-only and never repeats dispatch. */
  async observe(request: TypedGitEffectRequest): Promise<OperatorEffectOutcome> {
    const custodyId = this.custodyId(request);
    const binding = this.#binding(custodyId);
    const state = text(binding, "state");
    if (isGitTerminalState(state)) return this.#storedTerminalOutcome(custodyId, binding);
    if (state === "conflict" || state === "quarantined") return this.#ambiguousOutcome(custodyId, binding);
    if (state === "prepared" && nullableText(binding, "predecessor_custody_id") === null) {
      return this.#terminaliseNoEffect(custodyId, "rejected", {
        status: "rejected",
        code: "state-changed",
        evidenceRefs: [],
      });
    }
    let inspection: GitMutationInspection;
    if (state === "prepared") {
      inspection = await this.#inspectInheritedSuccessor(request.intent, custodyId);
    } else {
      try {
        inspection = await this.#gitPort.inspect(request.intent, { remoteTarget: this.#remoteTarget(binding) });
      } catch {
        inspection = unavailableInspection(request.intent.repository, custodyId, "typed-git-recovery-inspector");
      }
    }
    this.#applyRecoveryInspection(custodyId, inspection);
    const after = this.#binding(custodyId);
    return isGitTerminalState(text(after, "state"))
      ? this.#storedTerminalOutcome(custodyId, after)
      : this.#ambiguousOutcome(custodyId, after);
  }

  async recover(): Promise<{ reconciled: number; quarantined: number }> {
    const values = this.#database.prepare(`
      SELECT custody.custody_id,custody.intent_json,custody.command_id,custody.intent_digest,
             custody.operator_id,custody.project_id,custody.project_session_id,custody.principal_generation,
             custody.operation,binding.state
        FROM operator_git_effect_bindings binding
        JOIN operator_effect_custody custody ON custody.custody_id=binding.custody_id
       WHERE binding.state IN ('prepared','dispatching','ambiguous')
       ORDER BY custody.custody_id
    `).all().map((value) => row(value, "typed Git recovery row"));
    let reconciled = 0;
    let quarantined = 0;
    for (const value of values) {
      const intent = JSON.parse(text(value, "intent_json")) as OperatorGitIntent;
      const request: TypedGitEffectRequest = {
        commandId: text(value, "command_id"),
        previewId: "startup-recovery",
        operatorId: text(value, "operator_id"),
        projectId: text(value, "project_id"),
        projectSessionId: text(value, "project_session_id"),
        principalGeneration: integer(value, "principal_generation"),
        operation: text(value, "operation"),
        intent,
        intentDigest: text(value, "intent_digest") as Sha256Digest,
        beforeStateDigest: intent.repository.repositoryStateDigest,
        attemptGeneration: 1,
      };
      await this.observe(request);
      reconciled += 1;
      if (text(this.#binding(text(value, "custody_id")), "state") === "quarantined") quarantined += 1;
    }
    return { reconciled, quarantined };
  }

  async reconcileConflict(input: Readonly<{
    reconciliationCommandId: string;
    targetCommandId: string;
    intentDigest: Sha256Digest;
    nextAttemptGeneration: number;
    binding: GitConflictReconcileBinding;
  }>): Promise<OperatorActionStatus> {
    const before = this.#conflictBinding(input.binding.custodyId);
    this.#assertConflictReconcileBinding(before, input.binding);
    const intent = JSON.parse(text(before, "intent_json")) as OperatorGitIntent;
    let inspection: GitMutationInspection;
    if (this.#conflictInspector === undefined) {
      inspection = {
        outcome: "inspector-unavailable",
        repository: intent.repository,
        evidenceDigest: digest({ custodyId: input.binding.custodyId, class: "pinned-conflict-reader-absent" }),
        failureSignatureDigest: digest({ class: "pinned-conflict-reader-absent" }),
        conflict: null,
      };
    } else {
      try {
        inspection = await this.#conflictInspector.inspect(intent);
      } catch {
        inspection = unavailableInspection(intent.repository, input.binding.custodyId, "typed-git-conflict-reader");
      }
    }
    this.#database.transaction(() => {
      const current = this.#conflictBinding(input.binding.custodyId);
      this.#assertConflictReconcileBinding(current, input.binding);
      const normalised = this.#normaliseConflictInspection(
        input.reconciliationCommandId,
        input.binding.custodyId,
        inspection,
      );
      this.#applyConflictInspection(current, input.binding, normalised);
    })();
    const result = this.status(input.targetCommandId, input.intentDigest, input.nextAttemptGeneration);
    if (result === null) throw new Error("typed Git reconciliation produced a terminal target unexpectedly");
    return result;
  }

  status(commandId: string, intentDigest: Sha256Digest, attemptGeneration?: number): OperatorActionStatus | null {
    const value = this.#database.prepare(`
      SELECT binding.*,custody.command_id,custody.intent_digest,custody.state AS custody_state
        FROM operator_git_effect_bindings binding
        JOIN operator_effect_custody custody ON custody.custody_id=binding.custody_id
       WHERE custody.command_id=?
    `).get(commandId);
    if (!isRow(value)) return null;
    const state = text(value, "state");
    if (isGitTerminalState(state)) return null;
    const gitCustody = custodyStatus(value);
    const projectedAttempt = attemptGeneration ?? this.#attemptGeneration(commandId, integer(value, "state_revision"));
    if (state === "conflict") {
      return { status: "conflict", commandId, intentDigest, attemptGeneration: projectedAttempt, gitCustody };
    }
    if (state === "quarantined") {
      return { status: "quarantined", commandId, intentDigest, attemptGeneration: projectedAttempt, gitCustody };
    }
    if (state === "prepared") {
      return { status: "pending", commandId, intentDigest, phase: "prepared", attemptGeneration: projectedAttempt, gitCustody };
    }
    return { status: "ambiguous", commandId, intentDigest, attemptGeneration: projectedAttempt, gitCustody };
  }

  #attemptGeneration(commandId: string, fallback: number): number {
    const value = this.#database.prepare(`
      SELECT json_extract(preview_json,'$.action.attemptGeneration') AS attempt_generation
        FROM operator_previews WHERE confirmed_command_id=?
    `).get(commandId);
    if (!isRow(value)) return fallback;
    const candidate = value.attempt_generation;
    return typeof candidate === "number" && Number.isSafeInteger(candidate) && candidate >= 1 ? candidate : fallback;
  }

  async #inspectInheritedSuccessor(intent: OperatorGitIntent, custodyId: string): Promise<GitMutationInspection> {
    if (this.#conflictInspector === undefined) {
      return unavailableInspection(intent.repository, custodyId, "typed-git-inherited-conflict-reader");
    }
    try {
      return await this.#conflictInspector.inspect(intent);
    } catch {
      return unavailableInspection(intent.repository, custodyId, "typed-git-inherited-conflict-reader");
    }
  }

  #applyRecoveryInspection(custodyId: string, inspection: GitMutationInspection): void {
    this.#database.transaction(() => {
      const binding = this.#binding(custodyId);
      const source = text(binding, "state");
      if (source === "dispatching") {
        this.#applyInspection(custodyId, inspection);
        return;
      }
      if (source !== "prepared" && source !== "ambiguous") return;
      const operationId = text(binding, "operation_id");
      const generation = integer(binding, "mutation_reservation_generation");
      if (inspection.outcome === "exact-applied" || inspection.outcome === "exact-no-effect") {
        this.#terminaliseFromLookup(binding, source, inspection);
        return;
      }
      const nextLookup = integer(binding, "lookup_generation") + 1;
      if (inspection.outcome === "exact-conflict") {
        const predecessorGeneration = nullableInteger(binding, "predecessor_conflict_generation") ?? 0;
        this.#database.prepare(`
          UPDATE operator_git_effect_bindings
             SET state='conflict',state_revision=state_revision+1,owned_conflict_generation=?,
                 lookup_generation=?,lookup_evidence_digest=?,lookup_outcome='exact-conflict',
                 lookup_failure_signature_digest=NULL,lookup_observed_at=?,updated_at=?
           WHERE custody_id=? AND state=?
        `).run(predecessorGeneration + 1, nextLookup, inspection.evidenceDigest, this.#clock(), this.#clock(), custodyId, source);
        this.#transitionOuterOwners(binding, source, "conflict");
        return;
      }
      const permanent = isPermanentOutcome(inspection.outcome) && source !== "prepared";
      const next = permanent ? "quarantined" : "ambiguous";
      this.#database.prepare(`
        UPDATE operator_git_effect_bindings
           SET state=?,state_revision=state_revision+1,lookup_generation=?,lookup_evidence_digest=?,lookup_outcome=?,
               lookup_failure_signature_digest=?,lookup_observed_at=?,resolution_eligible=?,
               resolution_eligible_lookup_generation=?,resolution_eligible_evidence_digest=?,
               resolution_eligibility_reason=?,updated_at=?
         WHERE custody_id=? AND state=?
      `).run(
        next, nextLookup, inspection.evidenceDigest, inspection.outcome, inspection.failureSignatureDigest,
        this.#clock(), permanent ? 1 : 0, permanent ? nextLookup : null,
        permanent ? inspection.evidenceDigest : null, permanent ? inspection.outcome : null,
        this.#clock(), custodyId, source,
      );
      this.#transitionOuterOwners(binding, source, next);
      void operationId;
      void generation;
    })();
  }

  #terminaliseFromLookup(binding: Row, source: "prepared" | "dispatching" | "ambiguous", inspection: GitMutationInspection): void {
    const custodyId = text(binding, "custody_id");
    const operationId = text(binding, "operation_id");
    const bindingState = inspection.outcome === "exact-applied" ? "applied" : "no-effect";
    const custodyState = inspection.outcome === "exact-applied" ? "terminal" : "no-effect";
    const admissionSource = ownerAdmissionState(source);
    const reservationSource = ownerReservationState(source);
    this.#database.prepare(`
      UPDATE operator_git_effect_bindings
         SET state=?,state_revision=state_revision+1,terminal_basis='machine-proof',
             lookup_generation=lookup_generation+1,lookup_evidence_digest=?,lookup_outcome=?,
             lookup_failure_signature_digest=NULL,lookup_observed_at=?,resolution_eligible=0,
             resolution_eligible_lookup_generation=NULL,resolution_eligible_evidence_digest=NULL,
             resolution_eligibility_reason=NULL,updated_at=?
       WHERE custody_id=? AND state=?
    `).run(bindingState, inspection.evidenceDigest, inspection.outcome, this.#clock(), this.#clock(), custodyId, source);
    this.#database.prepare("UPDATE operator_effect_custody SET state=?,outcome_json=?,updated_at=? WHERE custody_id=? AND state=?")
      .run(custodyState, canonicalJson({ status: "committed", afterState: inspection.repository }), this.#clock(), custodyId, ownerCustodyState(source));
    this.#database.prepare("UPDATE operation_admissions SET state='terminal',revision=revision+1 WHERE operation_id=? AND state=?")
      .run(operationId, admissionSource);
    this.#database.prepare("UPDATE git_mutation_reservations SET state='released',updated_at=? WHERE custody_id=? AND generation=? AND state=?")
      .run(this.#clock(), custodyId, integer(binding, "mutation_reservation_generation"), reservationSource);
  }

  #transitionOuterOwners(
    binding: Row,
    source: "prepared" | "dispatching" | "ambiguous" | "quarantined" | "conflict",
    target: "conflict" | "ambiguous" | "quarantined",
  ): void {
    const custodyId = text(binding, "custody_id");
    this.#database.prepare("UPDATE operation_admissions SET state=?,revision=revision+1 WHERE operation_id=? AND state=?")
      .run(target, text(binding, "operation_id"), ownerAdmissionState(source));
    this.#database.prepare("UPDATE git_mutation_reservations SET state=?,updated_at=? WHERE custody_id=? AND generation=? AND state=?")
      .run(target, this.#clock(), custodyId, integer(binding, "mutation_reservation_generation"), ownerReservationState(source));
    this.#database.prepare("UPDATE operator_effect_custody SET state=?,updated_at=? WHERE custody_id=? AND state=?")
      .run(target, this.#clock(), custodyId, ownerCustodyState(source));
  }

  #conflictBinding(custodyId: string): Row {
    return row(this.#database.prepare(`
      SELECT binding.*,custody.state AS custody_state,custody.command_id,custody.intent_digest,custody.intent_json,
             admission.state AS admission_state,reservation.state AS reservation_state
        FROM operator_git_effect_bindings binding
        JOIN operator_effect_custody custody ON custody.custody_id=binding.custody_id
        JOIN operation_admissions admission ON admission.operation_id=binding.operation_id
        JOIN git_mutation_reservations reservation
          ON reservation.custody_id=binding.custody_id
         AND reservation.generation=binding.mutation_reservation_generation
       WHERE binding.custody_id=?
    `).get(custodyId), "typed Git conflict custody");
  }

  #assertConflictReconcileBinding(current: Row, expected: GitConflictReconcileBinding): void {
    const before = JSON.parse(text(current, "before_git_state_json")) as GitRepositoryBinding;
    const common = [
      text(current, "custody_id") === expected.custodyId,
      text(current, "state") === expected.expectedBindingState,
      integer(current, "state_revision") === expected.expectedBindingStateRevision,
      nullableInteger(current, "owned_conflict_generation") === expected.expectedOwnedConflictGeneration,
      integer(current, "mutation_reservation_generation") === expected.expectedReservationGeneration,
      before.commonDirectoryIdentityDigest === expected.expectedCommonDirectoryIdentityDigest,
      integer(current, "lookup_generation") === expected.expectedLookupGeneration,
      nullableText(current, "lookup_evidence_digest") === expected.expectedLookupEvidenceDigest,
      integer(current, "resolution_eligible") === 0,
    ];
    if (expected.kind === "owned-conflict") {
      common.push(
        text(current, "custody_state") === "conflict",
        text(current, "admission_state") === "conflict",
        text(current, "reservation_state") === "conflict",
        nullableText(current, "predecessor_custody_id") === expected.expectedPredecessorCustodyId,
        nullableInteger(current, "predecessor_conflict_generation") === expected.expectedPredecessorConflictGeneration,
      );
    } else {
      common.push(
        nullableText(current, "predecessor_custody_id") === expected.expectedPredecessorCustodyId,
        nullableInteger(current, "predecessor_conflict_generation") === expected.expectedPredecessorConflictGeneration,
        text(current, "custody_state") === ownerCustodyState(expected.expectedBindingState),
        text(current, "admission_state") === ownerAdmissionState(expected.expectedBindingState),
        text(current, "reservation_state") === ownerReservationState(expected.expectedBindingState),
      );
    }
    if (common.some((matches) => !matches)) {
      throw new ProjectFabricCoreError("STALE_REVISION", "typed Git conflict custody changed before inspection");
    }
  }

  #normaliseConflictInspection(
    reconciliationCommandId: string,
    custodyId: string,
    inspection: GitMutationInspection,
  ): GitMutationInspection {
    if (inspection.outcome === "exact-applied" || inspection.outcome === "exact-no-effect") {
      return {
        ...inspection,
        outcome: "conflict-state-unverifiable",
        evidenceDigest: digest({ priorEvidence: inspection.evidenceDigest, outcome: "conflict-state-unverifiable" }),
        failureSignatureDigest: null,
      };
    }
    if (
      (inspection.outcome !== "unavailable" && inspection.outcome !== "inconsistent") ||
      inspection.failureSignatureDigest === null
    ) return inspection;
    const rows = this.#database.prepare(`
      SELECT command_id,result_json,created_at FROM operator_commands
       WHERE operation='git-custody-resolve' AND command_id<>?
       ORDER BY created_at DESC,command_id DESC LIMIT 32
    `).all(reconciliationCommandId).map((value) => row(value, "typed Git prior reconciliation"));
    const matching: Row[] = [];
    for (const value of rows) {
      let result: unknown;
      try { result = JSON.parse(text(value, "result_json")); } catch { break; }
      if (!isRow(result) || !isRow(result.gitCustody)) break;
      const gitCustody = result.gitCustody;
      if (
        gitCustody.custodyId !== custodyId || gitCustody.lookupOutcome !== inspection.outcome ||
        gitCustody.lookupFailureSignatureDigest !== inspection.failureSignatureDigest
      ) break;
      matching.push(value);
      if (matching.length === 2) break;
    }
    if (matching.length !== 2 || this.#clock() - integer(matching[1] as Row, "created_at") < 60_000) return inspection;
    const permanent = inspection.outcome === "unavailable" ? "inspector-unavailable" : "evidence-integrity-failure";
    return {
      ...inspection,
      outcome: permanent,
      evidenceDigest: digest({ priorEvidence: inspection.evidenceDigest, outcome: permanent, streak: 3 }),
    };
  }

  #applyConflictInspection(
    current: Row,
    expected: GitConflictReconcileBinding,
    inspection: GitMutationInspection,
  ): void {
    const custodyId = text(current, "custody_id");
    const source = text(current, "state") as "prepared" | "ambiguous" | "quarantined" | "conflict";
    const nextLookup = integer(current, "lookup_generation") + 1;
    let target: "conflict" | "ambiguous" | "quarantined";
    let ownedGeneration = nullableInteger(current, "owned_conflict_generation");
    let eligible = false;
    if (inspection.outcome === "exact-conflict") {
      target = "conflict";
      if (expected.kind === "inherited-successor") ownedGeneration = expected.expectedPredecessorConflictGeneration + 1;
    } else if (isPermanentOutcome(inspection.outcome) || inspection.outcome === "conflict-state-unverifiable") {
      target = "quarantined";
      eligible = true;
    } else if (expected.kind === "owned-conflict") {
      target = "conflict";
    } else {
      target = source === "quarantined" ? "quarantined" : "ambiguous";
    }
    this.#database.prepare(`
      UPDATE operator_git_effect_bindings
         SET state=?,state_revision=state_revision+1,owned_conflict_generation=?,
             lookup_generation=?,lookup_evidence_digest=?,lookup_outcome=?,lookup_failure_signature_digest=?,
             lookup_observed_at=?,resolution_eligible=?,resolution_eligible_lookup_generation=?,
             resolution_eligible_evidence_digest=?,resolution_eligibility_reason=?,updated_at=?
       WHERE custody_id=? AND state=? AND state_revision=?
    `).run(
      target, ownedGeneration, nextLookup, inspection.evidenceDigest, inspection.outcome,
      inspection.failureSignatureDigest, this.#clock(), eligible ? 1 : 0, eligible ? nextLookup : null,
      eligible ? inspection.evidenceDigest : null, eligible ? inspection.outcome : null,
      this.#clock(), custodyId, source, expected.expectedBindingStateRevision,
    );
    this.#transitionOuterOwners(current, source, target);
  }

  #administrativeAuthority(intent: TypedGitAdministrativeIntent): AuthorityState {
    const expected = administrativeBinding(intent);
    const value = row(this.#database.prepare(`
      SELECT session.project_id,session.revision AS session_revision,session.generation AS session_generation,
             run.revision AS run_revision,run.dependency_revision,run.authority_ref,run.authority_revision,
             run.git_allowlist_epoch,run.git_allowlist_digest
        FROM project_sessions session JOIN runs run ON run.project_session_id=session.project_session_id
       WHERE session.project_session_id=? AND run.run_id=?
    `).get(expected.projectSessionId, expected.coordinationRunId), "typed Git administrative authority");
    const state: AuthorityState = {
      projectId: text(value, "project_id"),
      sessionRevision: integer(value, "session_revision"),
      sessionGeneration: integer(value, "session_generation"),
      runRevision: integer(value, "run_revision"),
      dependencyRevision: integer(value, "dependency_revision"),
      authorityRef: text(value, "authority_ref"),
      authorityRevision: integer(value, "authority_revision"),
      gitAllowlistEpoch: integer(value, "git_allowlist_epoch"),
      gitAllowlistDigest: nullableText(value, "git_allowlist_digest"),
    };
    if (
      state.projectId !== expected.projectId || state.sessionRevision !== expected.expectedSessionRevision ||
      state.sessionGeneration !== expected.expectedSessionGeneration || state.runRevision !== expected.expectedRunRevision ||
      state.dependencyRevision !== expected.expectedDependencyRevision ||
      (expected.authorityRef !== undefined && state.authorityRef !== expected.authorityRef) ||
      (expected.expectedAuthorityRevision !== undefined && state.authorityRevision !== expected.expectedAuthorityRevision) ||
      (expected.gitAllowlistEpoch !== undefined && state.gitAllowlistEpoch !== expected.gitAllowlistEpoch) ||
      (expected.gitAllowlistDigest !== undefined && state.gitAllowlistDigest !== expected.gitAllowlistDigest)
    ) throw new ProjectFabricCoreError("STALE_GENERATION", "typed Git administrative authority tuple changed");
    return state;
  }

  #applyGrantAdministration(intent: GitAuthoriseIntent, operatorInputRecordDigest: Sha256Digest): void {
    if (intent.action === "revoke") {
      this.#assertStoredGrant(intent.currentGrant, "active");
      const changed = this.#database.prepare(`
        UPDATE operator_git_grants SET state='revoked',revoked_at=?
         WHERE grant_id=? AND revision=? AND state='active' AND grant_digest=?
      `).run(this.#clock(), intent.currentGrant.grantId, intent.currentGrant.revision, intent.currentGrant.grantDigest);
      if (changed.changes !== 1) throw new ProjectFabricCoreError("STALE_REVISION", "typed Git grant changed before revocation");
      return;
    }
    const proposed = intent.proposedGrant;
    if (deriveGitGrantDigest(stripGrantDigest(proposed)) !== proposed.grantDigest) {
      throw new ProjectFabricCoreError("CONFLICT", "typed Git proposed grant digest is invalid");
    }
    if (proposed.sourceAuthority.kind !== "operator-command" || proposed.sourceAuthority.digest !== operatorInputRecordDigest) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git grant lacks exact independently recorded operator input");
    }
    if (
      proposed.projectId !== intent.projectId || proposed.projectSessionId !== intent.projectSessionId ||
      proposed.sessionGeneration !== intent.expectedSessionGeneration || proposed.issuingSessionRevision !== intent.expectedSessionRevision ||
      proposed.coordinationRunId !== intent.coordinationRunId || proposed.issuingRunRevision !== intent.expectedRunRevision ||
      proposed.issuingDependencyRevision !== intent.expectedDependencyRevision || proposed.authorityRef !== intent.authorityRef ||
      proposed.authorityRevision !== intent.expectedAuthorityRevision || proposed.gitAllowlistEpoch !== intent.expectedGitAllowlistEpoch ||
      proposed.gitAllowlistDigest !== intent.gitAllowlistDigest
    ) throw new ProjectFabricCoreError("CONFLICT", "typed Git proposed grant provenance does not match the current authority");
    if (intent.action === "issue") {
      if (proposed.revision !== 1 || isRow(this.#database.prepare(
        "SELECT 1 FROM operator_git_grants WHERE grant_id=?",
      ).get(proposed.grantId))) throw new ProjectFabricCoreError("CONFLICT", "typed Git grant identity already exists");
    } else {
      this.#assertStoredGrant(intent.currentGrant, "active");
      if (proposed.grantId !== intent.currentGrant.grantId || proposed.revision !== intent.currentGrant.revision + 1) {
        throw new ProjectFabricCoreError("CONFLICT", "typed Git grant revision is not the next immutable revision");
      }
    }
    this.#assertGrantWithinAllowlist(proposed);
    if (intent.action === "revise") {
      const changed = this.#database.prepare(`
        UPDATE operator_git_grants SET state='revoked',revoked_at=?
         WHERE grant_id=? AND revision=? AND state='active' AND grant_digest=?
      `).run(this.#clock(), intent.currentGrant.grantId, intent.currentGrant.revision, intent.currentGrant.grantDigest);
      if (changed.changes !== 1) throw new ProjectFabricCoreError("STALE_REVISION", "typed Git grant changed before revision");
    }
    this.#insertGrant(proposed);
  }

  #assertStoredGrant(grant: GitActionGrant, expectedState: "active" | "revoked"): void {
    const stored = row(this.#database.prepare(`
      SELECT state,grant_digest,constraints_json FROM operator_git_grants WHERE grant_id=? AND revision=?
    `).get(grant.grantId, grant.revision), "typed Git current grant");
    if (
      text(stored, "state") !== expectedState || text(stored, "grant_digest") !== grant.grantDigest ||
      text(stored, "constraints_json") !== canonicalJson(grant.constraints) ||
      deriveGitGrantDigest(stripGrantDigest(grant)) !== grant.grantDigest
    ) throw new ProjectFabricCoreError("STALE_REVISION", "typed Git current grant identity changed");
  }

  #assertGrantWithinAllowlist(grant: GitActionGrant): void {
    const allowlist = row(this.#database.prepare(`
      SELECT * FROM run_git_allowlists
       WHERE project_session_id=? AND coordination_run_id=? AND authority_revision=?
         AND git_allowlist_epoch=? AND git_allowlist_digest=?
    `).get(
      grant.projectSessionId, grant.coordinationRunId, grant.authorityRevision,
      grant.gitAllowlistEpoch, grant.gitAllowlistDigest,
    ), "typed Git positive run allow-list");
    if (
      Date.parse(grant.expiresAt) <= this.#clock() || Date.parse(grant.expiresAt) > integer(allowlist, "maximum_expiry") ||
      (grant.constraints.allowWorktreeCreation && integer(allowlist, "allow_worktree_creation") !== 1) ||
      (grant.constraints.operationVariants.some(isWorktreeCreateVariant) && !grant.constraints.allowWorktreeCreation)
    ) throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git grant expiry or worktree authority exceeds the allow-list");
    if (!isRow(this.#database.prepare(`
      SELECT 1 FROM run_git_allowlist_profiles
       WHERE project_session_id=? AND coordination_run_id=? AND authority_revision=? AND git_allowlist_epoch=?
         AND profile_id=? AND profile_revision=? AND profile_digest=?
    `).get(
      grant.projectSessionId, grant.coordinationRunId, grant.authorityRevision, grant.gitAllowlistEpoch,
      grant.executionProfileId, grant.executionProfileRevision, grant.executionProfileDigest,
    ))) throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git execution profile exceeds the allow-list");
    for (const variant of unique(grant.constraints.operationVariants, "operation variant")) {
      if (!isRow(this.#database.prepare(`
        SELECT 1 FROM run_git_allowlist_variants
         WHERE project_session_id=? AND coordination_run_id=? AND authority_revision=? AND git_allowlist_epoch=?
           AND operation_variant=?
      `).get(grant.projectSessionId, grant.coordinationRunId, grant.authorityRevision, grant.gitAllowlistEpoch, variant))) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git operation variant exceeds the allow-list");
      }
    }
    for (const remote of uniqueBy(grant.constraints.remoteBindings, (value) => canonicalJson(value), "remote binding")) {
      if (!isRow(this.#database.prepare(`
        SELECT 1 FROM run_git_allowlist_remotes
         WHERE project_session_id=? AND coordination_run_id=? AND authority_revision=? AND git_allowlist_epoch=?
           AND registration_id=? AND registration_revision=? AND generation=? AND target_digest=?
      `).get(
        grant.projectSessionId, grant.coordinationRunId, grant.authorityRevision, grant.gitAllowlistEpoch,
        remote.registrationId, remote.revision, remote.generation, remote.targetDigest,
      ))) throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git remote exceeds the allow-list");
    }
    for (const ref of unique(grant.constraints.refs, "ref")) {
      if (!isRow(this.#database.prepare(`
        SELECT 1 FROM run_git_allowlist_refs
         WHERE project_session_id=? AND coordination_run_id=? AND authority_revision=? AND git_allowlist_epoch=? AND ref_name=?
      `).get(grant.projectSessionId, grant.coordinationRunId, grant.authorityRevision, grant.gitAllowlistEpoch, ref))) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git ref exceeds the allow-list");
      }
    }
    for (const prefix of unique(grant.constraints.pathPrefixes, "path prefix")) {
      if (!isRow(this.#database.prepare(`
        SELECT 1 FROM run_git_allowlist_paths
         WHERE project_session_id=? AND coordination_run_id=? AND authority_revision=? AND git_allowlist_epoch=?
           AND repository_root=? AND worktree_path=?
           AND (?=canonical_prefix OR ? LIKE canonical_prefix || '/%')
      `).get(
        grant.projectSessionId, grant.coordinationRunId, grant.authorityRevision, grant.gitAllowlistEpoch,
        grant.repositoryRoot, grant.worktreePath, prefix, prefix,
      ))) throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git path prefix exceeds the allow-list");
    }
  }

  #insertGrant(grant: GitActionGrant): void {
    this.#database.prepare(`
      INSERT INTO operator_git_grants(
        grant_id,revision,project_id,project_session_id,session_generation,issuing_session_revision,
        coordination_run_id,issuing_run_revision,issuing_dependency_revision,authority_ref,authority_revision,
        git_allowlist_epoch,git_allowlist_digest,repository_root,worktree_path,execution_profile_id,
        execution_profile_revision,execution_profile_digest,allow_worktree_creation,source_kind,source_digest,
        constraints_json,grant_digest,state,expires_at,created_at,revoked_at
      ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'active',?,?,NULL)
    `).run(
      grant.grantId, grant.revision, grant.projectId, grant.projectSessionId, grant.sessionGeneration,
      grant.issuingSessionRevision, grant.coordinationRunId, grant.issuingRunRevision,
      grant.issuingDependencyRevision, grant.authorityRef, grant.authorityRevision, grant.gitAllowlistEpoch,
      grant.gitAllowlistDigest, grant.repositoryRoot, grant.worktreePath, grant.executionProfileId,
      grant.executionProfileRevision, grant.executionProfileDigest, grant.constraints.allowWorktreeCreation ? 1 : 0,
      grant.sourceAuthority.kind, grant.sourceAuthority.digest, canonicalJson(grant.constraints), grant.grantDigest,
      Date.parse(grant.expiresAt), this.#clock(),
    );
    const variant = this.#database.prepare("INSERT INTO operator_git_grant_variants VALUES(?,?,?)");
    for (const value of grant.constraints.operationVariants) variant.run(grant.grantId, grant.revision, value);
    const remote = this.#database.prepare("INSERT INTO operator_git_grant_remotes VALUES(?,?,?,?,?,?)");
    for (const value of grant.constraints.remoteBindings) {
      remote.run(grant.grantId, grant.revision, value.registrationId, value.revision, value.generation, value.targetDigest);
    }
    const ref = this.#database.prepare("INSERT INTO operator_git_grant_refs VALUES(?,?,?)");
    for (const value of grant.constraints.refs) ref.run(grant.grantId, grant.revision, value);
    const path = this.#database.prepare("INSERT INTO operator_git_grant_paths VALUES(?,?,?)");
    for (const value of grant.constraints.pathPrefixes) path.run(grant.grantId, grant.revision, value);
  }

  #applyDraftAdministration(intent: GitOperationDraftIntent, request: TypedGitAdministrativeRequest): void {
    if (intent.action === "cancel") {
      const draft = row(this.#database.prepare("SELECT * FROM git_operation_drafts WHERE draft_id=?")
        .get(intent.draftId), "typed Git draft cancellation");
      if (
        text(draft, "project_id") !== intent.projectId || text(draft, "project_session_id") !== intent.projectSessionId ||
        text(draft, "coordination_run_id") !== intent.coordinationRunId || integer(draft, "revision") !== intent.expectedDraftRevision ||
        text(draft, "draft_digest") !== intent.draftDigest || !["open", "gate-bound"].includes(text(draft, "state"))
      ) throw new ProjectFabricCoreError("STALE_REVISION", "typed Git draft changed before cancellation");
      this.#database.prepare(`
        UPDATE git_operation_drafts
           SET state='cancelled',revision=revision+1,terminal_reason='operator-cancelled',updated_at=?
         WHERE draft_id=? AND state IN ('open','gate-bound')
      `).run(this.#clock(), intent.draftId);
      this.#database.prepare("UPDATE operation_admissions SET state='cancelled',revision=revision+1 WHERE operation_id=? AND state='prepared'")
        .run(text(draft, "operation_id"));
      this.#database.prepare(`
        UPDATE scoped_gates SET status='superseded',revision=revision+1,updated_at=?
         WHERE gate_id IN (SELECT gate_id FROM scoped_gate_operations WHERE operation_id=?)
           AND status IN ('pending','deferred','approved')
      `).run(this.#clock(), text(draft, "operation_id"));
      return;
    }
    const binding = intent.binding;
    const identity = administrativeDraftIdentity(intent);
    const payloadDigest = binding.kind === "mutation"
      ? binding.authorisation.effectBindingDigest
      : digest({ domain: "git-custody-resolution-binding-v1", binding });
    if (binding.kind === "mutation") {
      if (isPreauthorisedGitOperationVariant(binding.operation.variant)) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git draft is reserved for gate-only mutations");
      }
      if (
        binding.authorisation.operationVariant !== binding.operation.variant ||
        binding.authorisation.resultRecipeDigest !== binding.resultRecipe.resultRecipeDigest ||
        binding.authorisation.repositoryStateDigest !== binding.repository.repositoryStateDigest ||
        binding.resultRecipe.beforeRepositoryStateDigest !== binding.repository.repositoryStateDigest ||
        deriveGitResultRecipeDigest(stripRecipeDigest(binding.resultRecipe)) !== binding.resultRecipe.resultRecipeDigest ||
        deriveGitEffectBindingDigest({
          projectId: binding.authorisation.projectId,
          projectSessionId: binding.authorisation.projectSessionId,
          coordinationRunId: binding.authorisation.coordinationRunId,
          authorityRef: binding.authorisation.authorityRef,
          authorityRevision: binding.authorisation.expectedAuthorityRevision,
          gitAllowlistEpoch: binding.authorisation.expectedGitAllowlistEpoch,
          gitAllowlistDigest: binding.authorisation.gitAllowlistDigest,
          repository: binding.repository,
          executionProfile: binding.executionProfile,
          remoteBinding: binding.authorisation.remoteBinding,
          operation: binding.operation,
          resultRecipeDigest: binding.resultRecipe.resultRecipeDigest,
        }) !== binding.authorisation.effectBindingDigest
      ) throw new ProjectFabricCoreError("CONFLICT", "typed Git draft mutation binding is invalid");
    } else {
      const target = this.#conflictBinding(binding.custodyId);
      if (
        text(target, "project_id") !== binding.projectId || text(target, "project_session_id") !== binding.projectSessionId ||
        text(target, "coordination_run_id") !== binding.coordinationRunId || text(target, "state") !== binding.expectedCustodyState ||
        integer(target, "lookup_generation") !== binding.expectedLookupGeneration ||
        text(target, "lookup_evidence_digest") !== binding.lookupEvidenceDigest ||
        integer(target, "resolution_eligible") !== 1 ||
        text(target, "resolution_eligibility_reason") !== binding.resolutionEligibilityReason
      ) throw new ProjectFabricCoreError("STALE_REVISION", "typed Git custody resolution draft target changed");
    }
    const expiresAt = Date.parse(intent.expiresAt);
    if (expiresAt <= this.#clock() || expiresAt > this.#clock() + 24 * 60 * 60 * 1000) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git draft expiry is outside the bounded window");
    }
    const requestDigest = digest({ draftRequestId: intent.draftRequestId, expiresAt: intent.expiresAt, binding });
    const draftId = deriveGitDraftId(identity, intent.draftRequestId);
    const operationId = deriveGitDraftOperationId({ ...identity, draftId, payloadDigest });
    const existing = this.#database.prepare("SELECT request_digest,draft_id FROM git_operation_drafts WHERE draft_id=?")
      .get(draftId);
    if (isRow(existing)) {
      if (text(existing, "request_digest") !== requestDigest) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "typed Git draft request ID was reused with changed binding");
      }
      return;
    }
    const operationKind = binding.kind === "mutation" ? binding.operation.variant : "git-custody-resolve";
    const immutable = {
      draftId,
      draftRequestId: intent.draftRequestId,
      requestDigest,
      operatorId: request.operatorId,
      ...identity,
      draftKind: binding.kind,
      operationId,
      operationKind,
      payloadDigest,
      binding,
      expiresAt: intent.expiresAt,
    };
    const draftDigest = digest({ domain: "git-operation-draft-v1", immutable });
    this.#database.prepare(`
      INSERT INTO operation_admissions(
        operation_id,project_session_id,coordination_run_id,operation_kind,state,revision,payload_digest
      ) VALUES(?,?,?,?, 'prepared',1,?)
    `).run(operationId, identity.projectSessionId, identity.coordinationRunId, operationKind, payloadDigest);
    this.#database.prepare(`
      INSERT INTO git_operation_drafts(
        draft_id,revision,draft_request_id,request_digest,operator_id,project_id,project_session_id,
        observed_session_revision,session_generation,coordination_run_id,observed_run_revision,
        observed_dependency_revision,authority_ref,authority_revision,git_allowlist_epoch,git_allowlist_digest,
        draft_kind,operation_id,operation_kind,payload_digest,binding_json,draft_digest,state,expires_at,
        consumed_command_id,terminal_reason,created_at,updated_at
      ) VALUES(?,1,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?, 'open',?,NULL,NULL,?,?)
    `).run(
      draftId, intent.draftRequestId, requestDigest, request.operatorId, identity.projectId, identity.projectSessionId,
      identity.expectedSessionRevision, identity.expectedSessionGeneration, identity.coordinationRunId,
      identity.expectedRunRevision, identity.expectedDependencyRevision, identity.authorityRef,
      identity.expectedAuthorityRevision ?? 1, identity.gitAllowlistEpoch ?? 1,
      identity.gitAllowlistDigest ?? null, binding.kind, operationId, operationKind, payloadDigest,
      canonicalJson(binding), draftDigest, expiresAt, this.#clock(), this.#clock(),
    );
  }

  #applyCustodyResolution(
    intent: GitCustodyResolveIntent,
    commandId: string,
    operatorId: string,
    operatorInputRecordDigest: Sha256Digest,
  ): void {
    const draft = row(this.#database.prepare("SELECT * FROM git_operation_drafts WHERE draft_id=? AND operation_id=?")
      .get(intent.draftId, intent.operationId), "typed Git custody resolution draft");
    const binding = resolutionBindingFromIntent(intent);
    const payloadDigest = digest({ domain: "git-custody-resolution-binding-v1", binding });
    if (
      integer(draft, "revision") !== intent.expectedDraftRevision || text(draft, "draft_digest") !== intent.draftDigest ||
      text(draft, "state") !== "gate-bound" || text(draft, "draft_kind") !== "custody-resolution" ||
      text(draft, "operation_kind") !== "git-custody-resolve" || text(draft, "payload_digest") !== payloadDigest ||
      text(draft, "binding_json") !== canonicalJson(binding) || integer(draft, "expires_at") <= this.#clock()
    ) throw new ProjectFabricCoreError("STALE_REVISION", "typed Git custody resolution draft changed");
    const gate = row(this.#database.prepare(`
      SELECT gate.* FROM scoped_gates gate JOIN scoped_gate_operations operation ON operation.gate_id=gate.gate_id
       WHERE gate.gate_id=? AND operation.operation_id=?
    `).get(intent.gateId, intent.operationId), "typed Git custody resolution gate");
    if (
      integer(gate, "revision") !== intent.expectedGateRevision || text(gate, "status") !== "approved" ||
      integer(gate, "human_required") !== 1 || nullableText(gate, "resolved_by_operator_id") === null ||
      text(gate, "project_session_id") !== intent.projectSessionId ||
      text(gate, "coordination_run_id") !== intent.coordinationRunId ||
      integer(gate, "dependency_revision") !== intent.expectedDependencyRevision
    ) throw new ProjectFabricCoreError("GATE_BLOCKED", "typed Git custody resolution gate is not exact and human-approved");
    const target = this.#conflictBinding(intent.custodyId);
    if (
      text(target, "state") !== intent.expectedCustodyState || text(target, "custody_state") !== intent.expectedCustodyState ||
      text(target, "admission_state") !== intent.expectedCustodyState || text(target, "reservation_state") !== intent.expectedCustodyState ||
      integer(target, "lookup_generation") !== intent.expectedLookupGeneration ||
      text(target, "lookup_evidence_digest") !== intent.lookupEvidenceDigest ||
      integer(target, "resolution_eligible") !== 1 ||
      text(target, "resolution_eligibility_reason") !== intent.resolutionEligibilityReason
    ) throw new ProjectFabricCoreError("STALE_REVISION", "typed Git custody resolution target changed");
    const resolutionId = `git-resolution-${sha256(canonicalJson({ draftId: intent.draftId, custodyId: intent.custodyId })).slice(0, 48)}`;
    const reservationDisposition = intent.adjudication === "quarantine-accepted" ? "retired" : "released";
    const immutable = {
      resolutionId,
      draftId: intent.draftId,
      resolutionOperationId: intent.operationId,
      targetCustodyId: intent.custodyId,
      targetOperationId: text(target, "operation_id"),
      projectId: intent.projectId,
      projectSessionId: intent.projectSessionId,
      coordinationRunId: intent.coordinationRunId,
      expectedLookupGeneration: intent.expectedLookupGeneration,
      lookupEvidenceDigest: intent.lookupEvidenceDigest,
      eligibilityReason: intent.resolutionEligibilityReason,
      adjudication: intent.adjudication,
      reason: intent.reason,
      gateId: intent.gateId,
      gateRevision: intent.expectedGateRevision,
      resolvedByOperatorId: operatorId,
      operatorInputRecordDigest,
      reservationDisposition,
    };
    const resolutionDigest = digest({ domain: "git-custody-resolution-v1", immutable });
    this.#database.prepare("UPDATE operation_admissions SET state='authorised',revision=revision+1 WHERE operation_id=? AND state='prepared'")
      .run(intent.operationId);
    this.#database.prepare(`
      INSERT INTO git_custody_resolutions(
        resolution_id,draft_id,resolution_operation_id,target_custody_id,target_operation_id,project_id,
        project_session_id,coordination_run_id,expected_lookup_generation,lookup_evidence_digest,
        eligibility_reason,adjudication,reason,gate_id,gate_revision,resolved_by_operator_id,
        operator_input_record_digest,reservation_disposition,resolution_digest,created_at
      ) VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      resolutionId, intent.draftId, intent.operationId, intent.custodyId, text(target, "operation_id"),
      intent.projectId, intent.projectSessionId, intent.coordinationRunId, intent.expectedLookupGeneration,
      intent.lookupEvidenceDigest, intent.resolutionEligibilityReason, intent.adjudication, intent.reason,
      intent.gateId, intent.expectedGateRevision, operatorId, operatorInputRecordDigest,
      reservationDisposition, resolutionDigest, this.#clock(),
    );
    this.#database.prepare(`
      UPDATE operator_git_effect_bindings
         SET state='human-resolved',state_revision=state_revision+1,terminal_basis='human-adjudication',
             resolution_eligible=0,resolution_eligible_lookup_generation=NULL,
             resolution_eligible_evidence_digest=NULL,resolution_eligibility_reason=NULL,updated_at=?
       WHERE custody_id=? AND state=? AND lookup_generation=? AND lookup_evidence_digest=?
    `).run(this.#clock(), intent.custodyId, intent.expectedCustodyState, intent.expectedLookupGeneration, intent.lookupEvidenceDigest);
    this.#database.prepare("UPDATE operator_effect_custody SET state='terminal',outcome_json=?,updated_at=? WHERE custody_id=? AND state=?")
      .run(canonicalJson({ status: "committed", afterState: { kind: `human-adjudicated-${intent.adjudication}`, resolutionDigest } }), this.#clock(), intent.custodyId, intent.expectedCustodyState);
    this.#database.prepare("UPDATE operation_admissions SET state='terminal',revision=revision+1 WHERE operation_id=? AND state=?")
      .run(text(target, "operation_id"), intent.expectedCustodyState);
    this.#database.prepare("UPDATE git_mutation_reservations SET state=?,updated_at=? WHERE custody_id=? AND generation=? AND state=?")
      .run(reservationDisposition, this.#clock(), intent.custodyId, integer(target, "mutation_reservation_generation"), intent.expectedCustodyState);
    this.#database.prepare("UPDATE git_operation_drafts SET state='consumed',revision=revision+1,consumed_command_id=?,updated_at=? WHERE draft_id=? AND state='gate-bound'")
      .run(commandId, this.#clock(), intent.draftId);
    this.#database.prepare("UPDATE operation_admissions SET state='terminal',revision=revision+1 WHERE operation_id=? AND state='authorised'")
      .run(intent.operationId);
  }

  #authority(intent: OperatorGitIntent): AuthorityState {
    const value = row(this.#database.prepare(`
      SELECT session.project_id,session.revision AS session_revision,session.generation AS session_generation,
             run.revision AS run_revision,run.dependency_revision,run.authority_ref,run.authority_revision,
             run.git_allowlist_epoch,run.git_allowlist_digest
        FROM project_sessions session JOIN runs run ON run.project_session_id=session.project_session_id
       WHERE session.project_session_id=? AND run.run_id=?
    `).get(intent.authorisation.projectSessionId, intent.authorisation.coordinationRunId), "typed Git authority");
    const state: AuthorityState = {
      projectId: text(value, "project_id"),
      sessionRevision: integer(value, "session_revision"),
      sessionGeneration: integer(value, "session_generation"),
      runRevision: integer(value, "run_revision"),
      dependencyRevision: integer(value, "dependency_revision"),
      authorityRef: text(value, "authority_ref"),
      authorityRevision: integer(value, "authority_revision"),
      gitAllowlistEpoch: integer(value, "git_allowlist_epoch"),
      gitAllowlistDigest: nullableText(value, "git_allowlist_digest"),
    };
    const expected = intent.authorisation;
    if (
      state.projectId !== expected.projectId || state.sessionRevision !== expected.expectedSessionRevision ||
      state.sessionGeneration !== expected.expectedSessionGeneration || state.runRevision !== expected.expectedRunRevision ||
      state.dependencyRevision !== expected.expectedDependencyRevision || state.authorityRef !== expected.authorityRef ||
      state.authorityRevision !== expected.expectedAuthorityRevision || state.gitAllowlistEpoch !== expected.expectedGitAllowlistEpoch ||
      state.gitAllowlistDigest !== expected.gitAllowlistDigest
    ) throw new ProjectFabricCoreError("STALE_GENERATION", "typed Git authority tuple changed");
    if (!isRow(this.#database.prepare(`
      SELECT 1 FROM run_authority_revisions
       WHERE project_session_id=? AND coordination_run_id=? AND authority_revision=? AND authority_ref=?
         AND git_allowlist_epoch=? AND git_allowlist_digest IS ?
    `).get(
      expected.projectSessionId, expected.coordinationRunId, expected.expectedAuthorityRevision,
      expected.authorityRef, expected.expectedGitAllowlistEpoch, expected.gitAllowlistDigest,
    ))) throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "typed Git authority history is incomplete");
    return state;
  }

  #profile(intent: OperatorGitIntent): OperatorGitIntent["executionProfile"] {
    const value = row(this.#database.prepare(`
      SELECT profile_id,revision,profile_digest,git_binary_digest,object_format,state
        FROM git_execution_profiles WHERE profile_id=? AND revision=?
    `).get(intent.executionProfile.profileId, intent.executionProfile.revision), "typed Git execution profile");
    if (
      text(value, "state") !== "active" || text(value, "profile_digest") !== intent.executionProfile.digest ||
      text(value, "git_binary_digest") !== intent.executionProfile.gitBinaryDigest ||
      text(value, "object_format") !== intent.executionProfile.objectFormat
    ) throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git execution profile changed");
    return intent.executionProfile;
  }

  #remote(intent: OperatorGitIntent): GitRemoteBinding | null {
    const binding = intent.authorisation.remoteBinding;
    if (binding === null) {
      if (operationRemote(intent.operation) !== null) throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git remote binding is absent");
      return null;
    }
    const operationBinding = operationRemote(intent.operation);
    if (operationBinding === null || canonicalJson(operationBinding) !== canonicalJson(binding)) {
      throw new ProjectFabricCoreError("CONFLICT", "typed Git operation remote does not match authority");
    }
    const value = row(this.#database.prepare(`
      SELECT * FROM git_remote_registrations WHERE registration_id=? AND revision=?
    `).get(binding.registrationId, binding.revision), "typed Git remote registration");
    if (
      text(value, "state") !== "active" || integer(value, "generation") !== binding.generation ||
      text(value, "project_id") !== intent.authorisation.projectId || text(value, "remote_name") !== binding.remoteName ||
      text(value, "target_digest") !== binding.targetDigest || text(value, "adapter_id") !== binding.adapterId ||
      text(value, "adapter_contract_digest") !== binding.adapterContractDigest
    ) throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git registered remote changed");
    return binding;
  }

  #grant(intent: OperatorGitIntent): GitActionGrant {
    const decision = intent.authorisation.decision;
    if (decision.kind !== "preauthorised") throw new Error("gate-bound typed Git action has no grant");
    const value = row(this.#database.prepare(`
      SELECT * FROM operator_git_grants WHERE grant_id=? AND revision=?
    `).get(decision.grantId, decision.expectedGrantRevision), "typed Git grant");
    if (
      text(value, "state") !== "active" || integer(value, "expires_at") <= this.#clock() ||
      text(value, "grant_digest") !== decision.grantDigest || text(value, "project_id") !== intent.authorisation.projectId ||
      text(value, "project_session_id") !== intent.authorisation.projectSessionId ||
      integer(value, "session_generation") !== intent.authorisation.expectedSessionGeneration ||
      text(value, "coordination_run_id") !== intent.authorisation.coordinationRunId ||
      text(value, "authority_ref") !== intent.authorisation.authorityRef ||
      integer(value, "authority_revision") !== intent.authorisation.expectedAuthorityRevision ||
      integer(value, "git_allowlist_epoch") !== intent.authorisation.expectedGitAllowlistEpoch ||
      text(value, "git_allowlist_digest") !== intent.authorisation.gitAllowlistDigest ||
      text(value, "repository_root") !== intent.repository.repositoryRoot || text(value, "worktree_path") !== intent.repository.worktreePath ||
      text(value, "execution_profile_id") !== intent.executionProfile.profileId ||
      integer(value, "execution_profile_revision") !== intent.executionProfile.revision ||
      text(value, "execution_profile_digest") !== intent.executionProfile.digest
    ) throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git grant is stale or out of scope");
    const constraints = JSON.parse(text(value, "constraints_json")) as GitActionGrant["constraints"];
    const grant: GitActionGrant = {
      grantId: text(value, "grant_id"),
      revision: integer(value, "revision"),
      projectId: text(value, "project_id") as GitActionGrant["projectId"],
      projectSessionId: text(value, "project_session_id") as GitActionGrant["projectSessionId"],
      sessionGeneration: integer(value, "session_generation"),
      issuingSessionRevision: integer(value, "issuing_session_revision"),
      coordinationRunId: text(value, "coordination_run_id") as GitActionGrant["coordinationRunId"],
      issuingRunRevision: integer(value, "issuing_run_revision"),
      issuingDependencyRevision: integer(value, "issuing_dependency_revision"),
      authorityRef: text(value, "authority_ref") as Sha256Digest,
      authorityRevision: integer(value, "authority_revision"),
      gitAllowlistEpoch: integer(value, "git_allowlist_epoch"),
      gitAllowlistDigest: text(value, "git_allowlist_digest") as Sha256Digest,
      repositoryRoot: text(value, "repository_root"),
      worktreePath: text(value, "worktree_path"),
      executionProfileId: text(value, "execution_profile_id"),
      executionProfileRevision: integer(value, "execution_profile_revision"),
      executionProfileDigest: text(value, "execution_profile_digest") as Sha256Digest,
      constraints,
      sourceAuthority: {
        kind: text(value, "source_kind") as "launch-envelope" | "operator-command",
        digest: text(value, "source_digest") as Sha256Digest,
      },
      expiresAt: new Date(integer(value, "expires_at")).toISOString() as GitActionGrant["expiresAt"],
      grantDigest: text(value, "grant_digest") as Sha256Digest,
    };
    if (deriveGitGrantDigest(stripGrantDigest(grant)) !== grant.grantDigest) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "typed Git grant digest mirror is inconsistent");
    }
    return grant;
  }

  #assertGrantConstraints(intent: OperatorGitIntent, grant: GitActionGrant): void {
    if (isWorktreeCreateVariant(intent.operation.variant) && !grant.constraints.allowWorktreeCreation) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git grant does not permit worktree creation");
    }
    if (!grant.constraints.operationVariants.includes(intent.operation.variant as never)) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git grant digest does not admit this operation variant");
    }
    if (!isRow(this.#database.prepare(`
      SELECT 1 FROM operator_git_grant_variants WHERE grant_id=? AND grant_revision=? AND operation_variant=?
    `).get(grant.grantId, grant.revision, intent.operation.variant))) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git grant does not admit this operation variant");
    }
    for (const path of operationPaths(intent.operation)) {
      if (!grant.constraints.pathPrefixes.some((prefix) => path === prefix || path.startsWith(`${prefix}/`))) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git path is outside the digest-bound grant");
      }
      const prefixes = this.#database.prepare(`
        SELECT canonical_prefix FROM operator_git_grant_paths WHERE grant_id=? AND grant_revision=?
      `).all(grant.grantId, grant.revision).map((value) => text(row(value, "grant path"), "canonical_prefix"));
      if (!prefixes.some((prefix) => path === prefix || path.startsWith(`${prefix}/`))) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git path is outside the grant");
      }
    }
    const remote = intent.authorisation.remoteBinding;
    if (remote !== null && !grant.constraints.remoteBindings.some((candidate) => canonicalJson(candidate) === canonicalJson(remote))) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git remote is outside the digest-bound grant");
    }
    if (remote !== null && !isRow(this.#database.prepare(`
      SELECT 1 FROM operator_git_grant_remotes
       WHERE grant_id=? AND grant_revision=? AND registration_id=? AND registration_revision=?
         AND generation=? AND target_digest=?
    `).get(grant.grantId, grant.revision, remote.registrationId, remote.revision, remote.generation, remote.targetDigest))) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git remote is outside the grant");
    }
    for (const ref of operationRefs(intent.operation)) {
      if (!grant.constraints.refs.includes(ref)) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git ref is outside the digest-bound grant");
      }
      if (!isRow(this.#database.prepare(`
        SELECT 1 FROM operator_git_grant_refs WHERE grant_id=? AND grant_revision=? AND ref_name=?
      `).get(grant.grantId, grant.revision, ref))) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git ref is outside the grant");
      }
    }
  }

  #writerAdmission(intent: OperatorGitIntent): void {
    const writer = this.#database.prepare(`
      SELECT admission.writer_admission_id
        FROM writer_admissions admission
        JOIN resource_reservations reservation ON reservation.reservation_id=admission.reservation_id
       WHERE reservation.project_session_id=? AND reservation.coordination_run_id=?
         AND admission.repository_root=? AND admission.worktree_path=? AND admission.state='active'
    `).get(
      intent.authorisation.projectSessionId,
      intent.authorisation.coordinationRunId,
      intent.repository.repositoryRoot,
      intent.repository.worktreePath,
    );
    if (!isRow(writer)) throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git worktree lacks active writer admission");
    for (const path of operationPaths(intent.operation)) {
      if (!isRow(this.#database.prepare(`
        SELECT 1 FROM writer_prefixes WHERE writer_admission_id=?
          AND (?=canonical_prefix OR ? LIKE canonical_prefix || '/%')
      `).get(text(writer, "writer_admission_id"), path, path))) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git path is outside writer admission");
      }
    }
  }

  #prepareReservationTransfer(intent: OperatorGitIntent, custodyId: string): number {
    const predecessor = conflictPredecessor(intent.operation);
    if (predecessor === null) return 1;
    const binding = row(this.#database.prepare(`
      SELECT * FROM operator_git_effect_bindings WHERE custody_id=?
    `).get(predecessor.custodyId), "typed Git conflict predecessor");
    if (
      text(binding, "state") !== "conflict" ||
      integer(binding, "owned_conflict_generation") !== predecessor.generation ||
      text(binding, "repository_root") !== intent.repository.repositoryRoot ||
      text(binding, "worktree_path") !== intent.repository.worktreePath
    ) throw new ProjectFabricCoreError("STALE_REVISION", "typed Git conflict predecessor changed");
    const generation = integer(binding, "mutation_reservation_generation") + 1;
    this.#database.prepare(`
      UPDATE operator_git_effect_bindings SET state='conflict-transferred',state_revision=state_revision+1,
             terminal_basis='conflict-transfer',updated_at=? WHERE custody_id=? AND state='conflict'
    `).run(this.#clock(), predecessor.custodyId);
    this.#database.prepare("UPDATE operator_effect_custody SET state='terminal',updated_at=? WHERE custody_id=? AND state='conflict'")
      .run(this.#clock(), predecessor.custodyId);
    this.#database.prepare("UPDATE operation_admissions SET state='terminal',revision=revision+1 WHERE operation_id=? AND state='conflict'")
      .run(text(binding, "operation_id"));
    this.#database.prepare("UPDATE git_mutation_reservations SET state='released',updated_at=? WHERE custody_id=? AND state='conflict'")
      .run(this.#clock(), predecessor.custodyId);
    void custodyId;
    return generation;
  }

  #applyInspection(custodyId: string, inspection: GitMutationInspection): void {
    this.#database.transaction(() => {
      const binding = this.#binding(custodyId);
      if (text(binding, "state") !== "dispatching") return;
      const operationId = text(binding, "operation_id");
      const generation = integer(binding, "mutation_reservation_generation");
      if (inspection.outcome === "exact-applied" || inspection.outcome === "exact-no-effect") {
        const bindingState = inspection.outcome === "exact-applied" ? "applied" : "no-effect";
        const custodyState = inspection.outcome === "exact-applied" ? "terminal" : "no-effect";
        this.#database.prepare(`
          UPDATE operator_git_effect_bindings
             SET state=?,state_revision=state_revision+1,terminal_basis='machine-proof',
                 lookup_generation=lookup_generation+1,lookup_evidence_digest=?,lookup_outcome=?,
                 lookup_failure_signature_digest=NULL,lookup_observed_at=?,updated_at=?
           WHERE custody_id=? AND state='dispatching'
        `).run(bindingState, inspection.evidenceDigest, inspection.outcome, this.#clock(), this.#clock(), custodyId);
        this.#database.prepare("UPDATE operator_effect_custody SET state=?,outcome_json=?,updated_at=? WHERE custody_id=? AND state='dispatching'")
          .run(custodyState, canonicalJson({ status: "committed", afterState: inspection.repository }), this.#clock(), custodyId);
        this.#database.prepare("UPDATE operation_admissions SET state='terminal',revision=revision+1 WHERE operation_id=? AND state='executing'")
          .run(operationId);
        this.#database.prepare("UPDATE git_mutation_reservations SET state='released',updated_at=? WHERE custody_id=? AND generation=? AND state='dispatching'")
          .run(this.#clock(), custodyId, generation);
        return;
      }
      if (inspection.outcome === "exact-conflict") {
        const predecessorGeneration = nullableInteger(binding, "predecessor_conflict_generation") ?? 0;
        this.#database.prepare(`
          UPDATE operator_git_effect_bindings
             SET state='conflict',state_revision=state_revision+1,owned_conflict_generation=?,
                 lookup_generation=lookup_generation+1,lookup_evidence_digest=?,lookup_outcome='exact-conflict',
                 lookup_failure_signature_digest=NULL,lookup_observed_at=?,updated_at=?
           WHERE custody_id=? AND state='dispatching'
        `).run(predecessorGeneration + 1, inspection.evidenceDigest, this.#clock(), this.#clock(), custodyId);
        this.#setFourOwnerNonterminal(custodyId, operationId, generation, "conflict");
        return;
      }
      const next = isPermanentOutcome(inspection.outcome) ? "quarantined" : "ambiguous";
      const eligible = isPermanentOutcome(inspection.outcome) ? 1 : 0;
      const nextLookup = integer(binding, "lookup_generation") + 1;
      this.#database.prepare(`
        UPDATE operator_git_effect_bindings
           SET state=?,state_revision=state_revision+1,lookup_generation=?,lookup_evidence_digest=?,lookup_outcome=?,
               lookup_failure_signature_digest=?,lookup_observed_at=?,resolution_eligible=?,
               resolution_eligible_lookup_generation=?,resolution_eligible_evidence_digest=?,resolution_eligibility_reason=?,updated_at=?
         WHERE custody_id=? AND state='dispatching'
      `).run(
        next, nextLookup, inspection.evidenceDigest, inspection.outcome, inspection.failureSignatureDigest,
        this.#clock(), eligible, eligible === 1 ? nextLookup : null, eligible === 1 ? inspection.evidenceDigest : null,
        eligible === 1 ? inspection.outcome : null, this.#clock(), custodyId,
      );
      this.#setFourOwnerNonterminal(custodyId, operationId, generation, next);
    })();
  }

  #setFourOwnerNonterminal(
    custodyId: string,
    operationId: string,
    generation: number,
    state: "conflict" | "ambiguous" | "quarantined",
  ): void {
    this.#database.prepare("UPDATE operation_admissions SET state=?,revision=revision+1 WHERE operation_id=? AND state='executing'")
      .run(state, operationId);
    this.#database.prepare("UPDATE git_mutation_reservations SET state=?,updated_at=? WHERE custody_id=? AND generation=? AND state='dispatching'")
      .run(state, this.#clock(), custodyId, generation);
    this.#database.prepare("UPDATE operator_effect_custody SET state=?,updated_at=? WHERE custody_id=? AND state='dispatching'")
      .run(state, this.#clock(), custodyId);
  }

  #terminaliseNoEffect(
    custodyId: string,
    bindingState: "rejected" | "failed",
    outcome: OperatorEffectOutcome,
  ): OperatorEffectOutcome {
    this.#database.transaction(() => {
      const binding = this.#binding(custodyId);
      this.#database.prepare("UPDATE operator_git_effect_bindings SET state=?,state_revision=state_revision+1,updated_at=? WHERE custody_id=? AND state='prepared'")
        .run(bindingState, this.#clock(), custodyId);
      this.#database.prepare("UPDATE operator_effect_custody SET state='no-effect',outcome_json=?,updated_at=? WHERE custody_id=? AND state='prepared'")
        .run(canonicalJson(outcome), this.#clock(), custodyId);
      this.#database.prepare("UPDATE operation_admissions SET state='cancelled',revision=revision+1 WHERE operation_id=? AND state='authorised'")
        .run(text(binding, "operation_id"));
      this.#database.prepare("UPDATE git_mutation_reservations SET state='released',updated_at=? WHERE custody_id=? AND state='reserved'")
        .run(this.#clock(), custodyId);
    })();
    return outcome;
  }

  #storedTerminalOutcome(custodyId: string, binding: Row): OperatorEffectOutcome {
    const custody = row(this.#database.prepare("SELECT outcome_json FROM operator_effect_custody WHERE custody_id=?").get(custodyId), "typed Git custody");
    const serialized = nullableText(custody, "outcome_json");
    if (serialized !== null) return JSON.parse(serialized) as OperatorEffectOutcome;
    return { status: "committed", afterState: JSON.parse(text(binding, "expected_terminal_state_json")) as JsonValue };
  }

  #ambiguousOutcome(custodyId: string, binding: Row): OperatorEffectOutcome {
    return {
      status: "ambiguous",
      effectRef: {
        path: parseCanonicalRelativePath(
          `.agent-fabric/operator-effects/${custodyId}.json`,
          "typedGit.ambiguousEffectRef.path",
        ),
        digest: digest({ custodyId, state: text(binding, "state"), revision: integer(binding, "state_revision") }),
      },
    };
  }

  #remoteTarget(binding: Row): string | null {
    const registrationId = nullableText(binding, "remote_registration_id");
    if (registrationId === null) return null;
    const remote = row(this.#database.prepare(`
      SELECT target_identity FROM git_remote_registrations WHERE registration_id=? AND revision=?
    `).get(registrationId, integer(binding, "remote_registration_revision")), "typed Git remote target");
    return text(remote, "target_identity");
  }

  #binding(custodyId: string): Row {
    return row(this.#database.prepare("SELECT * FROM operator_git_effect_bindings WHERE custody_id=?").get(custodyId), "typed Git binding");
  }

  #assertGenericCustody(custodyId: string, request: TypedGitEffectRequest): void {
    const custody = row(this.#database.prepare("SELECT * FROM operator_effect_custody WHERE custody_id=?").get(custodyId), "typed Git generic custody");
    if (
      text(custody, "operator_id") !== request.operatorId || text(custody, "project_id") !== request.projectId ||
      text(custody, "project_session_id") !== request.projectSessionId || integer(custody, "principal_generation") !== request.principalGeneration ||
      text(custody, "command_id") !== request.commandId || text(custody, "operation") !== "git" ||
      text(custody, "intent_digest") !== request.intentDigest || text(custody, "before_state_digest") !== request.beforeStateDigest ||
      text(custody, "intent_json") !== canonicalJson(request.intent) || text(custody, "state") !== "prepared"
    ) throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "typed Git generic custody identity changed");
  }

  #assertBindingReplay(custodyId: string, request: TypedGitEffectRequest): void {
    const binding = this.#binding(custodyId);
    if (
      text(binding, "operation_id") !== request.intent.authorisation.operationId ||
      text(binding, "effect_binding_digest") !== request.intent.authorisation.effectBindingDigest ||
      text(binding, "result_recipe_digest") !== request.intent.resultRecipe.resultRecipeDigest
    ) throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "typed Git custody replay changed");
  }

  #assertPointOfUse(request: TypedGitEffectRequest, binding: Row): void {
    const intent = request.intent;
    const custodyId = this.custodyId(request);
    this.#assertGenericCustody(custodyId, request);
    this.#assertBindingReplay(custodyId, request);
    const authority = this.#authority(intent);
    const profile = this.#profile(intent);
    const remote = this.#remote(intent);
    this.#writerAdmission(intent);
    if (
      integer(binding, "prepared_session_revision") !== authority.sessionRevision ||
      integer(binding, "session_generation") !== authority.sessionGeneration ||
      integer(binding, "prepared_run_revision") !== authority.runRevision ||
      integer(binding, "prepared_dependency_revision") !== authority.dependencyRevision ||
      text(binding, "authority_ref") !== authority.authorityRef ||
      integer(binding, "authority_revision") !== authority.authorityRevision ||
      integer(binding, "git_allowlist_epoch") !== authority.gitAllowlistEpoch ||
      nullableText(binding, "git_allowlist_digest") !== authority.gitAllowlistDigest ||
      text(binding, "execution_profile_id") !== profile.profileId ||
      integer(binding, "execution_profile_revision") !== profile.revision ||
      text(binding, "execution_profile_digest") !== profile.digest ||
      nullableText(binding, "remote_registration_id") !== (remote?.registrationId ?? null) ||
      nullableInteger(binding, "remote_registration_revision") !== (remote?.revision ?? null) ||
      nullableInteger(binding, "remote_generation") !== (remote?.generation ?? null) ||
      nullableText(binding, "remote_target_digest") !== (remote?.targetDigest ?? null)
    ) throw new ProjectFabricCoreError("STALE_GENERATION", "typed Git point-of-use binding changed");
    const decision = intent.authorisation.decision;
    if (decision.kind === "preauthorised") {
      const grant = this.#grant(intent);
      this.#assertGrantWithinAllowlist(grant);
      this.#assertGrantConstraints(intent, grant);
      this.#assertParentWorktreeCreation(intent);
      if (
        nullableText(binding, "grant_id") !== grant.grantId ||
        nullableInteger(binding, "grant_revision") !== grant.revision ||
        nullableText(binding, "draft_id") !== null || nullableText(binding, "gate_id") !== null
      ) throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git grant binding changed before dispatch");
      return;
    }
    this.#assertParentWorktreeCreation(intent);
    const gate = row(this.#database.prepare(`
      SELECT gate.*,draft.state AS draft_state,draft.revision AS draft_revision,
             draft.draft_digest,draft.operation_id AS draft_operation_id
        FROM scoped_gates gate
        JOIN scoped_gate_operations operation ON operation.gate_id=gate.gate_id
        JOIN git_operation_drafts draft ON draft.operation_id=operation.operation_id
       WHERE gate.gate_id=? AND operation.operation_id=? AND draft.draft_id=?
    `).get(decision.gateId, decision.blockedOperationId, decision.draftId), "typed Git point-of-use gate");
    if (
      text(gate, "status") !== "approved" || integer(gate, "human_required") !== 1 ||
      nullableText(gate, "resolved_by_operator_id") === null ||
      text(gate, "project_session_id") !== intent.authorisation.projectSessionId ||
      text(gate, "coordination_run_id") !== intent.authorisation.coordinationRunId ||
      integer(gate, "dependency_revision") !== intent.authorisation.expectedDependencyRevision ||
      integer(gate, "revision") !== decision.expectedGateRevision ||
      text(gate, "draft_state") !== "consumed" || text(gate, "draft_digest") !== decision.draftDigest ||
      text(gate, "draft_operation_id") !== intent.authorisation.operationId ||
      nullableText(binding, "draft_id") !== decision.draftId || nullableText(binding, "gate_id") !== decision.gateId ||
      nullableInteger(binding, "gate_revision") !== decision.expectedGateRevision || nullableText(binding, "grant_id") !== null
    ) throw new ProjectFabricCoreError("GATE_BLOCKED", "typed Git gate changed before dispatch");
  }

  #assertParentWorktreeCreation(intent: OperatorGitIntent): void {
    if (!isWorktreeCreateVariant(intent.operation.variant)) return;
    if (!isRow(this.#database.prepare(`
      SELECT 1 FROM run_git_allowlists
       WHERE project_session_id=? AND coordination_run_id=? AND authority_revision=?
         AND git_allowlist_epoch=? AND git_allowlist_digest=? AND allow_worktree_creation=1
    `).get(
      intent.authorisation.projectSessionId,
      intent.authorisation.coordinationRunId,
      intent.authorisation.expectedAuthorityRevision,
      intent.authorisation.expectedGitAllowlistEpoch,
      intent.authorisation.gitAllowlistDigest,
    ))) throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed Git parent authority does not permit worktree creation");
  }

  #assertFourOwnerState(
    custodyId: string,
    bindingState: string,
    custodyState: string,
    admissionState: string,
    reservationState: string,
  ): void {
    const value = row(this.#database.prepare(`
      SELECT binding.state AS binding_state,custody.state AS custody_state,
             admission.state AS admission_state,reservation.state AS reservation_state
        FROM operator_git_effect_bindings binding
        JOIN operator_effect_custody custody ON custody.custody_id=binding.custody_id
        JOIN operation_admissions admission ON admission.operation_id=binding.operation_id
        JOIN git_mutation_reservations reservation
          ON reservation.custody_id=binding.custody_id
         AND reservation.generation=binding.mutation_reservation_generation
       WHERE binding.custody_id=?
    `).get(custodyId), "typed Git four-owner state");
    if (
      text(value, "binding_state") !== bindingState || text(value, "custody_state") !== custodyState ||
      text(value, "admission_state") !== admissionState || text(value, "reservation_state") !== reservationState
    ) throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "typed Git four-owner state is inconsistent");
  }
}

type AuthorityState = {
  projectId: string;
  sessionRevision: number;
  sessionGeneration: number;
  runRevision: number;
  dependencyRevision: number;
  authorityRef: string;
  authorityRevision: number;
  gitAllowlistEpoch: number;
  gitAllowlistDigest: string | null;
};

function digest(value: unknown): Sha256Digest {
  return `sha256:${sha256(canonicalJson(value))}` as Sha256Digest;
}

function stripRecipeDigest(recipe: GitResultRecipeV1): Omit<GitResultRecipeV1, "resultRecipeDigest"> {
  const { resultRecipeDigest: _ignored, ...value } = recipe;
  return value;
}

function stripGrantDigest(grant: GitActionGrant): Omit<GitActionGrant, "grantDigest"> {
  const { grantDigest: _ignored, ...value } = grant;
  return value;
}

type AdministrativeAuthorityBinding = {
  projectId: string;
  projectSessionId: string;
  expectedSessionRevision: number;
  expectedSessionGeneration: number;
  coordinationRunId: string;
  expectedRunRevision: number;
  expectedDependencyRevision: number;
  authorityRef?: Sha256Digest;
  expectedAuthorityRevision?: number;
  gitAllowlistEpoch?: number;
  gitAllowlistDigest?: Sha256Digest;
};

function administrativeBinding(intent: TypedGitAdministrativeIntent): AdministrativeAuthorityBinding {
  if (intent.kind === "git-authorise") {
    return {
      ...intent,
      gitAllowlistEpoch: intent.expectedGitAllowlistEpoch,
      gitAllowlistDigest: intent.gitAllowlistDigest,
    };
  }
  if (intent.kind === "git-custody-resolve" || intent.action === "cancel") return intent;
  if (intent.binding.kind === "mutation") {
    const value = intent.binding.authorisation;
    return {
      projectId: value.projectId,
      projectSessionId: value.projectSessionId,
      expectedSessionRevision: value.expectedSessionRevision,
      expectedSessionGeneration: value.expectedSessionGeneration,
      coordinationRunId: value.coordinationRunId,
      expectedRunRevision: value.expectedRunRevision,
      expectedDependencyRevision: value.expectedDependencyRevision,
      authorityRef: value.authorityRef,
      expectedAuthorityRevision: value.expectedAuthorityRevision,
      gitAllowlistEpoch: value.expectedGitAllowlistEpoch,
      ...(value.gitAllowlistDigest === null ? {} : { gitAllowlistDigest: value.gitAllowlistDigest }),
    };
  }
  return intent.binding;
}

function administrativeDraftIdentity(
  intent: Extract<GitOperationDraftIntent, { action: "create" }>,
): AdministrativeAuthorityBinding {
  return administrativeBinding(intent);
}

function deriveGitDraftId(identity: AdministrativeAuthorityBinding, draftRequestId: string): string {
  return `git-draft-${sha256(canonicalJson({
    domain: "git-operation-draft-identity-v1",
    projectId: identity.projectId,
    projectSessionId: identity.projectSessionId,
    coordinationRunId: identity.coordinationRunId,
    draftRequestId,
  })).slice(0, 48)}`;
}

function deriveGitDraftOperationId(value: AdministrativeAuthorityBinding & {
  draftId: string;
  payloadDigest: Sha256Digest;
}): string {
  return `git-draft-operation-${sha256(canonicalJson({ domain: "git-draft-operation-v1", ...value })).slice(0, 48)}`;
}

function resolutionBindingFromIntent(intent: GitCustodyResolveIntent) {
  return {
    kind: "custody-resolution" as const,
    projectId: intent.projectId,
    projectSessionId: intent.projectSessionId,
    expectedSessionRevision: intent.expectedSessionRevision,
    expectedSessionGeneration: intent.expectedSessionGeneration,
    coordinationRunId: intent.coordinationRunId,
    expectedRunRevision: intent.expectedRunRevision,
    expectedDependencyRevision: intent.expectedDependencyRevision,
    authorityRef: intent.authorityRef,
    expectedAuthorityRevision: intent.expectedAuthorityRevision,
    custodyId: intent.custodyId,
    expectedCustodyState: intent.expectedCustodyState,
    expectedLookupGeneration: intent.expectedLookupGeneration,
    lookupEvidenceDigest: intent.lookupEvidenceDigest,
    resolutionEligibilityReason: intent.resolutionEligibilityReason,
    adjudication: intent.adjudication,
    reason: intent.reason,
  };
}

function mutationDraftBinding(intent: OperatorGitIntent) {
  const { operationId: _operationId, decision: _decision, ...authorisation } = intent.authorisation;
  return {
    kind: "mutation" as const,
    authorisation,
    repository: intent.repository,
    executionProfile: intent.executionProfile,
    operation: intent.operation,
    resultRecipe: intent.resultRecipe,
  };
}

function unique<T extends string>(values: readonly T[], label: string): readonly T[] {
  if (new Set(values).size !== values.length) throw new ProjectFabricCoreError("CONFLICT", `typed Git ${label} set is not unique`);
  return values;
}

function uniqueBy<T>(values: readonly T[], key: (value: T) => string, label: string): readonly T[] {
  if (new Set(values.map(key)).size !== values.length) {
    throw new ProjectFabricCoreError("CONFLICT", `typed Git ${label} set is not unique`);
  }
  return values;
}

function operationPaths(operation: GitOperation): readonly string[] {
  return operation.variant === "stage" || operation.variant === "unstage" ? operation.paths : [];
}

function operationRefs(operation: GitOperation): readonly string[] {
  switch (operation.variant) {
    case "fetch": case "pull-fast-forward-only": case "push-fast-forward-only": return [operation.sourceRef, operation.destinationRef];
    case "branch-create": return [operation.destinationRef];
    case "branch-rename": return [operation.sourceRef, operation.destinationRef];
    case "branch-delete-merged-only": return [operation.sourceRef];
    case "worktree-create-existing-branch": case "worktree-create-new-branch": return [operation.branchRef];
    case "upstream-set": case "upstream-unset": return [operation.localBranchRef, operation.remoteBranchRef];
    default: return [];
  }
}

function operationRemote(operation: GitOperation): GitRemoteBinding | null {
  return "remote" in operation ? operation.remote : null;
}

function isWorktreeCreateVariant(variant: string): boolean {
  return variant === "worktree-create-detached" ||
    variant === "worktree-create-new-branch" ||
    variant === "worktree-create-existing-branch";
}

function conflictPredecessor(operation: GitOperation): { custodyId: string; generation: number } | null {
  if (
    operation.variant !== "merge-continue" && operation.variant !== "merge-abort" &&
    operation.variant !== "rebase-continue" && operation.variant !== "rebase-abort"
  ) return null;
  return { custodyId: operation.predecessorCustodyId, generation: operation.predecessorConflictGeneration };
}

function isGitTerminalState(state: string): boolean {
  return ["applied", "no-effect", "rejected", "failed", "human-resolved", "conflict-transferred"].includes(state);
}

function isPermanentOutcome(outcome: GitMutationInspection["outcome"]): boolean {
  return [
    "inspector-unavailable", "remote-proof-permanently-unavailable", "mixed-local-remote-evidence",
    "evidence-integrity-failure", "conflict-state-unverifiable",
  ].includes(outcome);
}

function ownerCustodyState(
  bindingState: "prepared" | "dispatching" | "conflict" | "ambiguous" | "quarantined",
): "prepared" | "dispatching" | "conflict" | "ambiguous" | "quarantined" {
  return bindingState;
}

function ownerAdmissionState(
  bindingState: "prepared" | "dispatching" | "conflict" | "ambiguous" | "quarantined",
): "authorised" | "executing" | "conflict" | "ambiguous" | "quarantined" {
  if (bindingState === "prepared") return "authorised";
  if (bindingState === "dispatching") return "executing";
  return bindingState;
}

function ownerReservationState(
  bindingState: "prepared" | "dispatching" | "conflict" | "ambiguous" | "quarantined",
): "reserved" | "dispatching" | "conflict" | "ambiguous" | "quarantined" {
  if (bindingState === "prepared") return "reserved";
  return bindingState;
}

function unavailableInspection(
  repository: GitRepositoryBinding,
  custodyId: string,
  failureClass: string,
): GitMutationInspection {
  return {
    outcome: "unavailable",
    repository,
    evidenceDigest: digest({ custodyId, outcome: "unavailable", failureClass }),
    failureSignatureDigest: digest({ failureClass }),
    conflict: null,
  };
}

function nullableInteger(value: Row, field: string): number | null {
  const candidate = value[field];
  if (candidate !== null && (typeof candidate !== "number" || !Number.isSafeInteger(candidate))) {
    throw new Error(`${field} is not nullable integer`);
  }
  return candidate as number | null;
}

function custodyStatus(value: Row): GitCustodyStatus {
  const eligible = integer(value, "resolution_eligible") === 1;
  return {
    custodyId: text(value, "custody_id"),
    bindingStateRevision: integer(value, "state_revision"),
    reservationGeneration: integer(value, "mutation_reservation_generation"),
    commonDirectoryIdentityDigest: JSON.parse(text(value, "before_git_state_json")).commonDirectoryIdentityDigest as Sha256Digest,
    predecessorCustodyId: nullableText(value, "predecessor_custody_id"),
    predecessorConflictGeneration: nullableInteger(value, "predecessor_conflict_generation"),
    ownedConflictGeneration: nullableInteger(value, "owned_conflict_generation"),
    lookupGeneration: integer(value, "lookup_generation"),
    lookupEvidenceDigest: nullableText(value, "lookup_evidence_digest") as Sha256Digest | null,
    lookupOutcome: nullableText(value, "lookup_outcome") as GitLookupOutcome | null,
    lookupFailureSignatureDigest: nullableText(value, "lookup_failure_signature_digest") as Sha256Digest | null,
    lookupObservedAt: nullableInteger(value, "lookup_observed_at") === null
      ? null
      : new Date(nullableInteger(value, "lookup_observed_at") ?? 0).toISOString() as never,
    resolutionEligibility: eligible
      ? {
          kind: "eligible",
          lookupGeneration: integer(value, "resolution_eligible_lookup_generation"),
          evidenceDigest: text(value, "resolution_eligible_evidence_digest") as Sha256Digest,
          reason: text(value, "resolution_eligibility_reason") as never,
        }
      : { kind: "none" },
  };
}
