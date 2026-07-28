# Agent Fabric effects

## Recovery and operator receipt definitions

~~~yaml
recovery:
  required: [actionRef, recoveryGeneration, reason, state, disposition,
    reservationDigest, routeState, routeReceiptDigest, lookupState,
    lookupEvidenceDigest, settlementDigest, recoveryEvidenceDigest]
  routeStateEnum: [present, missing, integrity-failed]
  routeReceiptDigestInvariant: nonnull-iff-routeState-present
  nullOnly: [disposition, routeReceiptDigest, lookupEvidenceDigest,
    settlementDigest]
  stateEnum: [detected, inspecting, terminal-proved-no-effect,
    terminal-proved-usage, awaiting-human-retire, terminal-retired-unknown]
  reasonEnum: [intact-effect-ambiguity, route-row-missing,
    route-row-conflict, route-receipt-mismatch, target-binding-invalid,
    bundle-binding-invalid, prompt-binding-invalid, profile-binding-invalid,
    lineage-binding-invalid]
  dispositionEnum: [proved-no-effect-release, exact-usage-settled,
    conservative-full-ceiling-settled, full-ceiling-retired, null]
  lookupStateEnum: [not-attempted, in-flight, completed]
  actionRef: ProviderActionRefV1
  positive: [recoveryGeneration]
  digests: [reservationDigest, recoveryEvidenceDigest]
  nullableDigests: [routeReceiptDigest, lookupEvidenceDigest, settlementDigest]
  conditional: lookupEvidenceDigest is nonnull iff lookupState completed;
    detected/inspecting/awaiting-human-retire require null disposition and
    settlementDigest; terminal-proved-no-effect requires disposition proved-no-
    effect-release and nonnull settlementDigest; terminal-proved-usage requires
    exact-usage-settled or conservative-full-ceiling-settled and nonnull
    settlementDigest; terminal-retired-unknown requires full-ceiling-retired and
    nonnull settlementDigest

objectiveCheck:
  required: [taskId, checkId, kind, state, evidenceRef, evidenceDigest,
    observedSourceStateDigest]
  stateEnum: [pass, fail, not-run]
  nullOnly: [evidenceRef, evidenceDigest]
  conditional: evidenceRef and evidenceDigest are both nonnull iff state pass-or-fail;
    both null iff not-run
  ids: [taskId, checkId, evidenceRef]
  kindEnum: objectiveCheckKindEnum
  digests: [observedSourceStateDigest]
  nullableDigests: [evidenceDigest]

providerFailureOrSubstitutionEvent:
  required: [actionRef, eventGeneration, requestedFamily, requestedModel,
    resolvedAdapterId, resolvedFamily, resolvedModel, code, evidenceDigest]
  nullOnly: [resolvedAdapterId, resolvedFamily, resolvedModel]
  conditional: resolvedAdapterId/resolvedFamily/resolvedModel are all nonnull or all null
  actionRef: ProviderActionRefV1
  positive: [eventGeneration]
  ids: [requestedFamily, requestedModel,
    resolvedAdapterId, resolvedFamily, resolvedModel]
  codeEnum: providerFailureOrSubstitutionEnum
  invariant: resolvedAdapterId-is-null-or-equals-actionRef.adapterId
  digests: [evidenceDigest]

intervention:
  required: [commandId, operation, operatorId, targetRef, targetRevision,
    directInputAttestationId, resultDigest]
  nullOnly: [directInputAttestationId]
  ids: [commandId, operatorId, targetRef,
    directInputAttestationId]
  operationEnum: operatorInterventionOperationEnum
  positive: [targetRevision]
  digests: [resultDigest]
~~~

`providerFailuresAndSubstitutions` is an append-only event stream. Generation starts at one and is contiguous per canonical action pair; one event never overwrites another. A substitution event may therefore precede a later route, provider or quota failure for the same pair. Event order, not a single current code, is the receipt truth.

### Typed Git action authority and exact semantics

Possession of a session-bound operator capability containing `git` admits only the typed Git action family. It never authorises a mutation by itself. Every `OperatorGitIntent` shall additionally carry one closed `GitActionAuthorisation` whose common binding is:

```yaml
git_action_authorisation:
  project_id: exact-authenticated-project
  project_session_id: exact-session
  expected_session_revision: compare-and-set-integer
  expected_session_generation: fenced-generation
  coordination_run_id: exact-accountable-run
  expected_run_revision: compare-and-set-integer
  expected_dependency_revision: compare-and-set-integer
  authority_ref: exact-active-run-authority-sha256
  expected_authority_revision: compare-and-set-integer
  expected_git_allowlist_epoch: compare-and-set-integer
  git_allowlist_digest: null-or-exact-sha256
  repository_root: exact-canonical-trusted-root
  worktree_path: exact-canonical-admitted-worktree
  repository_state_digest: exact-sha256
  execution_profile_id: exact-trusted-profile
  execution_profile_revision: compare-and-set-integer
  execution_profile_digest: exact-sha256
  operation_variant: exact-closed-variant
  remote_binding: null-or-exact-registered-target
  result_recipe_digest: exact-sha256
  operation_id: daemon-derived-stable-id
  effect_binding_digest: exact-sha256
  decision: preauthorised-or-gate-variant
```

The common binding and each variant reject unknown or missing fields. The daemon derives project and operator identity from the authenticated connection, then cross-checks every duplicated session, run, repository, worktree, revision and digest against the intent, current Fabric records and a fresh typed Git observation. `effect_binding_digest` is the canonical SHA-256 of the complete Git effect, repository and remote bindings, execution profile, closed operation variant, canonical before state and complete expected result recipe, excluding only the operator credential, command ID, `operation_id` and the `decision` variant. The preauthorised variant derives `operation_id` from authenticated operator, project session, stable Preview ID and `effect_binding_digest`. A gate variant obtains both immutable values from the pre-effect draft below; it never derives gate identity from the later final Preview. Exact replay is stable, while a later preauthorised Preview or new gate draft has a distinct operation ID. A changed action, path, ref, remote, mode, expected object or authority-bound state therefore requires a new Preview and decision.

`coordination_run.authority_revision` is the canonical revision owner for the run's current authority tuple. It starts at one. Each historical `authority_ref` value is immutable. Authority rotation appends a history row and atomically changes the current `authority_ref`, increments `authority_revision` and the run revision, and invalidates every grant issued under the prior tuple. The common `expected_dependency_revision` is the same run-owned dependency revision used by scoped gates. No implementation may invent an authority revision from an operator command, grant row or artifact timestamp.

`git_allowlist_epoch` is the monotonic revision of `git_allowlist_v1` inside that same run-authority history; `git_allowlist_digest` is null only while the allow-list is absent. Adding, replacing or removing the allow-list is an authority rotation that advances the authority tuple and allow-list epoch in one transaction. It is not an independent mutable policy owner.

The preauthorised variant is:

```yaml
decision:
  kind: preauthorised
  grant_id: stable-id
  expected_grant_revision: compare-and-set-integer
  grant_digest: exact-sha256
```

The coordination-run authority may contain one closed positive `git_allowlist_v1`. Absence means that no Git grant can be issued. It names the maximum operation variants, execution profiles, remote registrations, refs, canonical path prefixes, worktree-creation permission, expiry and deterministic rewrite bounds. Denies still dominate. Only launch custody materialising an already human-approved session envelope, or a separately capable `git-authorise` operator action with independently attested direct human input, may issue or revoke a grant. The requested grant shall be a positive subset of every allow-list dimension; a capability, empty parent field or omission can never be treated as wildcard authority.

`git-authorise` is itself a closed Preview/Commit operator intent. It selects `issue`, `revise` or `revoke`; binds the exact project/session/generation and session/run/dependency/authority revisions; names the current allow-list epoch and digest; and carries either the complete proposed canonical grant or the exact current grant ID/revision/digest. Revise binds both. The daemon derives the canonical child constraints and proposed digest, shows the complete old/new authority diff, and binds the independently attested direct-human decision to that Preview. The caller cannot submit opaque constraints, self-attest, reuse the decision for another grant or receive a bearer credential in the result.

The referenced `GitActionGrant` is an immutable revisioned narrowing of that active allow-list:

```yaml
git_action_grant:
  grant_id: stable-id
  revision: compare-and-set-integer
  project_id: exact-project
  project_session_id: exact-session
  session_generation: fenced-generation
  issuing_session_revision: exact-revision
  coordination_run_id: exact-run
  issuing_run_revision: exact-revision
  issuing_dependency_revision: exact-revision
  authority_ref: exact-active-run-authority-sha256
  authority_revision: exact-revision
  git_allowlist_epoch: exact-issuance-epoch
  git_allowlist_digest: exact-sha256
  repository_root: exact-canonical-root
  worktree_path: exact-canonical-worktree
  execution_profile_id: exact-trusted-profile
  execution_profile_revision: exact-revision
  execution_profile_digest: exact-sha256
  operation_variants: closed-non-empty-concrete-set
  remote_bindings: closed-registered-target-set
  refs: closed-fully-qualified-ref-set
  path_prefixes: closed-canonical-relative-prefix-set
  source_authority:
    kind: launch-envelope-or-operator-command
    digest: exact-sha256
  expires_at: bounded-timestamp
  revoked_at: null-or-timestamp
```

The daemon hashes the immutable identity, issuing session/run/dependency provenance, authority and allow-list tuple, repository, constraint and expiry fields of the closed canonical grant to `grant_digest`; `revoked_at` is excluded because it is a later lifecycle fact. Empty constraint sets mean that category is unavailable to an action requiring it, not unconstrained. `operation_variants` uses the exhaustive action-and-mode vocabulary below; coarse `branch`, `worktree`, `pull`, `merge`, `rebase` or `push` values are invalid. A concrete action must match one exact variant, execution profile, registered remote target, every fully qualified ref and every canonical repository-relative path. The exact worktree shall retain an active writer admission for the same project session and run when the effect can write files.

`issuing_session_revision`, `issuing_run_revision` and `issuing_dependency_revision` are immutable, hash-bound issuance provenance, not point-of-use equality fences. They prove where the reusable grant came from. Ordinary later session, run or dependency revision advances, including revision changes caused by Git custody/audit activity, neither alter nor invalidate it. Each action still carries current expected revisions and a stale Preview fails its own compare-and-set; a new Preview may reuse the same grant. Ordinary HEAD/ref/index/worktree-content changes likewise stale only the action Preview, not the grant, while canonical repository and admitted-worktree identity remain unchanged.

Point-of-use grant equality is required for session generation, current authority revision/ref, current `git_allowlist_epoch`/digest, execution-profile revision/digest and every remote registration revision/generation/target digest. Grant expiry, revocation/non-active state, authority or allow-list rotation, session-generation change, profile/remote-target change, repository/worktree identity change or constraint mismatch fails before Git lock acquisition or process I/O. No action Preview may rewrite issuance provenance or silently refresh any live authority fence.

The daemon owns a secret-free remote registry independently of `.git/config`:

```yaml
git_remote_registration:
  registration_id: stable-id
  revision: compare-and-set-integer
  generation: target-rotation-fence
  project_id: exact-project
  remote_name: bounded-display-name
  transport_kind: allow-listed-kind
  target_identity: normalised-secret-free-host-port-repository
  target_digest: exact-sha256
  adapter_id: trusted-remote-port
  adapter_contract_digest: exact-sha256
  credential_selector_digest: secret-free-sha256
  state: active-or-revoked
```

For a remote action, `remote_binding` and the grant's `remote_bindings` contain the registration ID, revision, generation, name, target digest, adapter and contract digest. A name is display metadata, never authority. Retargeting a name appends a registration revision, advances generation and invalidates all prior grants, Previews and custody. Project Git configuration cannot select a target, remote helper, credential helper or transport executable.

The trusted `GitExecutionProfile` is also closed and digest-bound. It records the exact Git binary path/version/digest and object format; built-in merge and rebase algorithm IDs; a sanitised configuration/environment policy; sealed empty hooks; permitted raw attribute behaviour; the trusted remote-port/helper registry; and hard result bounds. System, global and repository configuration cannot select an alias, hook, clean/process filter, custom merge/diff driver, editor, pager, signing programme, credential helper, remote helper, SSH command or executable. A profile may instead name an explicitly registered absolute helper binary plus digest, fixed argument template, credential selector and enforced sandbox. Unknown attributes, includes, helpers or drivers make the affected operation unavailable before Preview. Stage uses exact raw bytes, and merge/rebase use only the profile's built-in deterministic backend.

The exhaustive V1 operation variants are:

| Effect family | Exact operation variants | Preauthorised grant permitted |
| --- | --- | --- |
| fetch | `fetch` | yes |
| pull | `pull-fast-forward-only`, `pull-merge-commit-start`, `pull-rebase-start` | fast-forward only |
| index | `stage`, `unstage` | yes |
| commit | `commit` | yes |
| merge | `merge-fast-forward-only-start`, `merge-commit-start`, `merge-continue`, `merge-abort` | no |
| rebase | `rebase-current-branch-no-autostash-start`, `rebase-continue`, `rebase-abort` | no |
| push | `push-fast-forward-only`, `push-force-with-lease` | fast-forward only |
| branch | `branch-create`, `branch-rename`, `branch-delete-merged-only`, `branch-delete-force` | all except force delete |
| worktree | `worktree-create-detached`, `worktree-create-new-branch`, `worktree-create-existing-branch`, `worktree-move`, `worktree-remove-clean`, `worktree-remove-force` | all except force remove |
| upstream | `upstream-set`, `upstream-unset` | yes |

Each `OperatorGitIntent` discriminator/action/mode/strategy/policy maps to exactly one row value and vice versa. A grant containing a gate-only variant is invalid. Pull merge/rebase, all standalone merge/rebase variants, force-with-lease push, destructive branch deletion and forced worktree removal always use the gate variant. A grant for one sibling operation, such as `branch-create`, can never authorise `branch-rename` or either delete mode.

A gate-only operation first creates one typed pre-effect reservation. `GitOperationDraftIntent` is a closed `OperatorActionIntent` with `create` and `cancel` discriminators routed only through the existing `fabric.v1.operator-action.preview`/`commit` owner. Its Preview is read-only; confirmed Commit creates or cancels the no-authority draft and prepared admission. Cancel binds the exact draft ID/revision/digest and accepts only `open`/`gate-bound`. It is not the final Git action Preview/Commit.

```yaml
git_operation_draft:
  draft_id: daemon-generated-stable-id
  revision: compare-and-set-integer
  kind: mutation-or-custody-resolution
  project_id: exact-authenticated-project
  project_session_id: exact-session
  observed_session_revision: draft-cas-fence
  session_generation: fenced-generation
  coordination_run_id: exact-run
  observed_run_revision: draft-cas-fence
  observed_dependency_revision: draft-cas-fence
  authority_ref: exact-current-sha256
  authority_revision: exact-current-revision
  git_allowlist_epoch: exact-current-epoch
  git_allowlist_digest: null-or-exact-sha256
  operation_id: daemon-derived-immutable-id
  operation_kind: exact-gate-only-variant
  payload_digest: exact-binding-sha256
  binding:
    kind: mutation
    effect_binding_digest: exact-sha256
    repository_state_digest: exact-sha256
    result_recipe_digest: exact-sha256
  state: open-or-gate-bound-or-consumed-or-stale-or-expired-or-cancelled
  expires_at: bounded-timestamp
```

The closed `binding` discriminator is `mutation` with the complete repository, worktree, execution-profile, target, before-state and result-recipe binding, or `custody-resolution` with the complete resolution binding defined below. Draft creation requires the corresponding `git` or `git-custody-resolve` capability and validates current authority, syntax and typed state through read-only inspection. The daemon derives `operation_id` from authenticated operator/project/session identity, the random stable `draft_id` and `payload_digest`, then atomically persists the immutable draft and one `prepared` `operation_admissions` row whose kind and payload digest match. It creates no generic effect custody, Git mutation reservation, grant consumption or mutation authority; makes no mutating Git/remote call; and does not block session closure or daemon idle stop.

Gate creation may bind only that exact prepared operation ID and draft payload. The later final Preview names the draft and approved gate, repeats every current session/run/dependency revision, session generation, authority and typed repository/custody observation, and requires the recomputed binding digest to equal the immutable draft; Preview remains read-only. Only a separately confirmed Commit may atomically consume the draft once, authorise the admission and write the action's typed rows. A mutation Commit creates effect custody/reservation; a custody-resolution Commit writes only the adjudication and target lifecycle changes below. Preview reports a changed binding without writing; draft reconciliation or a confirmed Commit terminalises the draft as `stale`, cancels its admission and supersedes the associated gate without creating custody. It cannot be rebound or refreshed under an earlier human decision. Expiry, explicit cancellation or rejection/cancellation/supersession of its gate is likewise terminal/no-authority, cancels the unconsumed admission and supersedes any remaining association without Git I/O; gate deferral leaves the bounded draft `gate-bound`. Exact draft-request replay returns the same identity and operation ID; a new request receives a new operation ID even for identical repository state.

The gate variant is:

```yaml
decision:
  kind: gate
  draft_id: exact-pre-effect-draft
  expected_draft_revision: compare-and-set-integer
  draft_digest: exact-sha256
  gate_id: exact-id
  expected_gate_revision: compare-and-set-integer
  expected_gate_status: approved
  blocked_operation_id: exact-operation-id
```

The gate shall belong to the draft's project session and coordination run, have an `operation` enforcement point, bind `blocked_operation_id` exactly to the draft's `operation_id`, bind the current dependency revision and have an authenticated human resolver. The preauthorised confirmed Commit creates one `authorised` admission. The gate draft already owns one `prepared` admission; final Commit compare-and-sets it to `authorised`. In both cases `operation_kind` is the exact operation variant and `payload_digest` is the immutable binding digest. The gate variant additionally requires the persisted exact `(gate_id, operation_id)` association. An operation kind is classification data and can never substitute for the unique operation ID. Policy approval, a gate for another action/draft, a stale/superseded gate or a general consequential-action capability is insufficient. Commit rechecks the draft, gate, association, admission and every current common binding after Preview and immediately before effect preparation. The gate never supplies release or deployment authority; those retain the exact release binding in the revisioned intake and scoped-gate contract.

`OperatorGitIntent.operation` is extended only as needed to close these semantics:

- **Fetch:** names one registered remote plus exact source and tracking refs.
  It cannot accept a URL, refspec, executable, transport or arbitrary option.
- **Pull:** names the same exact remote/ref pair and selects
  `fast-forward-only`, `merge-commit` or `rebase`. Merge/rebase pull is   gate-only. Its exact remote observation, tracking-ref update and integration   recipe share one custody record; a partial fetch is an observable   non-terminal outcome, not permission to repeat the pull.
- **Stage and unstage:** contain a non-empty, unique set of canonical
  repository-relative paths. Absolute paths, traversal, NUL, pathspec magic,   option interpretation and paths outside the bound worktree are rejected.
- **Commit:** binds the exact source index, parent and tree object digests, a
  bounded non-empty message, explicit author/committer identities and timestamp,   and the deterministically derived resulting commit object. Dispatch cannot   substitute current time or mutable Git identity configuration. It does not   invoke an editor, pager, signing programme or repository hook, and it cannot   include unreviewed worktree content.
- **Merge:** names exact source and destination objects and selects either
  `fast-forward-only` or `merge-commit`. `merge-commit` binds the exact backend,   ordered parents, output tree, author/committer identities and timestamp,   message and resulting commit object. It is one non-interactive   non-fast-forward merge with no implicit strategy, editor, autostash or   project-configured command execution.
- **Rebase:** always uses the gate variant. The source shall be the exact
  currently checked-out local branch and HEAD object in the bound worktree; the   destination is one exact object/ref. V1 permits only a non-interactive,   current-branch, `no-autostash` rebase. It forbids `--onto`, root,   rebase-merges, interactive/exec and rebasing another branch. Its recipe maps   every bounded source commit to exact new parent/tree, preserved author   identity/time, explicit committer identity/time and resulting object. Dirty,   conflicted or detached state fails before preparation.
- **Merge/rebase conflict exit:** a start may produce only the recipe's exact
  success state or exact bounded conflict state. A conflict persists the   predecessor custody ID, operation variant, conflict generation, index stages,   affected paths and original before state. `merge-continue` and   `rebase-continue` are new gate-bound actions over that exact conflict and a   newly reviewed resolution index/worktree plus complete deterministic result   recipe. `merge-abort` and `rebase-abort` are also typed gate-bound actions and   may restore only the predecessor's exact before state. No generic command,   automatic abort/continue or reuse of the start gate is permitted.   Startup performs no automatic inspection of a current conflict owner. An   explicit authenticated reconciliation of the exact custody and conflict   lineage generation may use only the   sealed no-process typed local reader. Exact complete conflict proof retains   the conflict for those typed successors. Complete proof that the persisted   native operation state, index stages or conflict-path manifest was destroyed   or altered out of band atomically moves the four custody owners to   `quarantined`, retains the common-directory reservation, records the new   lookup evidence and marks only that generation   `conflict-state-unverifiable`. One transient incomplete, unavailable or   inconsistent observation retains the conflict without an eligibility marker.   A closed machine-classified permanent `inspector-unavailable` or   `evidence-integrity-failure` outcome instead uses the same all-four-owner   quarantine/eligibility mapping with its exact reason. Reconciliation never   continues, aborts, restores or otherwise mutates Git.   The existing observe-only operator-action reconciliation surface adds two   Git-only `git_conflict` discriminators: `owned-conflict` for a current   conflict owner and `inherited-successor` for the typed continue/abort custody   holding a transferred reservation before it has proved itself the new owner.   Each requires the original target command, exact custody lineage, binding   state revision, reservation generation, common-directory identity digest,   lookup generation and nullable prior evidence digest. Both reauthenticate the   exact project/session and require the distinct `git-custody-resolve`   capability; the target action's `git` capability alone is insufficient.   Missing or stale fields change nothing, and no other action family may use   either discriminator.

  The additive closed request variants are:

  ```yaml
  operator_action_reconcile:
    command: exact-operator-mutation-context
    project_id: exact-authenticated-project
    target_command_id: exact-original-git-command
    expected_status: conflict
    expected_attempt_generation: compare-and-set-integer
    mode: observe-only
    git_conflict:
      kind: owned-conflict
      custody_id: exact-target-custody
      expected_binding_state: conflict
      expected_binding_state_revision: compare-and-set-integer
      expected_owned_conflict_generation: compare-and-set-integer
      expected_predecessor_custody_id: null-or-exact-custody
      expected_predecessor_conflict_generation: null-or-compare-and-set-integer
      expected_reservation_generation: compare-and-set-integer
      expected_common_directory_identity_digest: exact-sha256
      expected_lookup_generation: compare-and-set-non-negative-integer
      expected_lookup_evidence_digest: null-or-exact-sha256
      expected_resolution_eligibility: none
  ```

  ```yaml
  operator_action_reconcile:
    command: exact-operator-mutation-context
    project_id: exact-authenticated-project
    target_command_id: exact-typed-successor-git-command
    expected_status: pending-or-ambiguous-or-quarantined
    expected_attempt_generation: compare-and-set-integer
    mode: observe-only
    git_conflict:
      kind: inherited-successor
      custody_id: exact-target-custody
      expected_binding_state: prepared-or-ambiguous-or-quarantined
      expected_binding_state_revision: compare-and-set-integer
      expected_owned_conflict_generation: null
      expected_predecessor_custody_id: exact-predecessor-custody
      expected_predecessor_conflict_generation: compare-and-set-integer
      expected_reservation_generation: compare-and-set-integer
      expected_common_directory_identity_digest: exact-sha256
      expected_lookup_generation: compare-and-set-non-negative-integer
      expected_lookup_evidence_digest: null-or-exact-sha256
      expected_resolution_eligibility: none
  ```

  The outer status and binding state must map `pending -> prepared`,   `ambiguous -> ambiguous` or `quarantined -> quarantined`. An   `owned-conflict` request instead requires `conflict -> conflict`, positive   owned generation and the exact nullable predecessor generation. The existing   generic `pending`/`ambiguous` request rejects `git_conflict`; `quarantined` is   accepted only for the inherited-successor form. Both Git forms reject their   absence, crossed lineage fields, an existing eligibility marker or any   unknown field. The nullable expected evidence   value shall exactly equal the stored value and is null only before the first   lookup. An accepted inspection increments lookup and binding-state revision,   advances attempt generation exactly once, persists a bounded   outcome/evidence/timestamp and returns the exact current target-command status   below. It does not change reservation generation,   conflict-lineage generations or common-directory identity. Exact conflict   retains an owned conflict or atomically promotes an intact inherited   successor to the next owned conflict generation. Incomplete, unavailable or   inconsistent observation retains an existing conflict, otherwise leaves or   moves the inherited successor to `ambiguous`/`quarantined`, and creates no   resolution eligibility while transient. Complete proof that the persisted   owned or inherited conflict no longer holds, or one closed permanent   inspector/integrity outcome, returns `quarantined` with the matching   eligibility tuple and retained reservation.

  ```yaml
  operator_action_git_custody_status:
    status: pending-or-ambiguous-or-conflict-or-quarantined
    phase: prepared-when-status-pending-otherwise-absent
    command_id: exact-original-git-command
    intent_digest: exact-sha256
    attempt_generation: positive-integer
    git_custody:
      custody_id: exact-target-custody
      binding_state_revision: positive-integer
      reservation_generation: positive-integer
      common_directory_identity_digest: exact-sha256
      predecessor_custody_id: exact-custody-or-null
      predecessor_conflict_generation: positive-integer-or-null
      owned_conflict_generation: positive-integer-or-null
      lookup_generation: non-negative-integer
      lookup_evidence_digest: null-or-exact-sha256
      lookup_outcome: null-or-closed-outcome-code
      lookup_failure_signature_digest: null-or-exact-sha256
      lookup_observed_at: null-or-timestamp
      resolution_eligibility:
        kind: none-or-eligible
        lookup_generation: absent-unless-eligible-current-generation
        evidence_digest: absent-unless-eligible-current-sha256
        reason: absent-unless-eligible-closed-permanent-unprovability-code
  ```

  The `pending` form is allowed only for an inherited typed successor whose   binding is `prepared`; it requires `phase: prepared`, the complete positive   predecessor custody/generation pair, null owned generation and   `resolution_eligibility.kind: none`. No other generic pending action receives   `git_custody`, and `phase` is absent from the other three forms. A `conflict`   status requires a positive owned conflict generation and   `resolution_eligibility.kind: none`. A `quarantined` status may retain the   positive predecessor conflict generation;   `conflict-state-unverifiable` requires at least one positive owned or   predecessor conflict generation. Predecessor custody ID and generation are   both null or both present. An inherited successor has their exact positive   pair and null owned generation until exact intact proof assigns the next owned   generation. Lookup   evidence, outcome and observed time are all null exactly at generation zero   and all present thereafter. Eligibility, when present, exactly equals the   latest lookup generation/evidence and its outcome code. The closed lookup   outcomes are `exact-conflict`, `exact-applied`, `exact-no-effect`,   `incomplete`, `unavailable`, `inconsistent`, `inspector-unavailable`,   `remote-proof-permanently-unavailable`, `mixed-local-remote-evidence`,   `evidence-integrity-failure` and `conflict-state-unverifiable`.   `lookup_failure_signature_digest` is present only for `incomplete`,   `unavailable`, `inconsistent`, `inspector-unavailable`,   `remote-proof-permanently-unavailable`, `mixed-local-remote-evidence` or   `evidence-integrity-failure`; it is null at generation zero and for exact   state outcomes. It hashes only the normalised bounded failure class and   stable machine facts, excluding time, command/operator identity and text.

  `inspector-unavailable` is permanent only when the digest-pinned reader or its   trusted execution-profile contract is absent/revoked for the target   generation, or after three consecutive accepted `unavailable` observations   for the same custody/lineage and normalised failure-signature digest under   distinct reconciliation commands spanning at least 60 seconds. An   `evidence-integrity-failure` is permanent only when the sealed reader can read   the bounded canonical files but proves their format/hash relationship cannot   yield any complete observation, or after the equivalent three-observation   rule for one identical normalised `inconsistent` signature. The daemon   derives the streak from immutable reconciliation-command results; project   files, operator text and the caller cannot select a permanent code. Any   intervening different outcome/signature resets the streak. Attempts one and   two remain non-eligible and retain the blocker. The third final-CAS   transaction persists the permanent outcome, moves all four owners to   `quarantined` and sets only the matching latest-generation eligibility tuple.

  Reconcile response and exact command replay return the same closed status   snapshot without another inspection. Reuse of the reconciliation command ID   with any changed field is a dedupe conflict. Status query by the original   target command returns its current custody status; status query by the   reconciliation command returns that command's immutable result snapshot,   not an operator-action receipt. Both queries require `read` and perform no   inspection or state change. A new reconciliation command must compare-and-set   the latest returned tuple.

  If any requested authority or custody field becomes stale after the bounded   read but before final compare-and-set, the final transaction changes no   custody, admission or reservation row and terminalises the reconciliation   command as a closed rejection: `state-changed` for a custody tuple mismatch,   `generation-stale` for a principal/session generation mismatch or   `authority-insufficient` for expired/revoked/insufficient capability. Its command ID,   target intent digest and original evidence references are preserved. Exact   replay and status query return that immutable rejection with no inspection;   changed replay conflicts, and another inspection requires a new command over   the latest target tuple.
- **Push:** names one registered remote, one exact local source ref and one
  exact remote destination ref. `fast-forward-only` relies on the remote's   atomic non-fast-forward rejection. `force-with-lease` additionally binds the   exact expected remote object and always uses the gate variant. Neither form   implies pull-request merge, release or deployment authority.
- **Branch create/rename/delete:** use fully qualified local refs and exact
  objects. Safe deletion is `merged-only` against an exact base and refuses a   branch checked out in any worktree. Destructive deletion is an explicit   `force` mode, binds the deleted object and consequence in the gate, and never   follows from a broad branch grant.
- **Worktree create:** selects `detached`, `new-branch` or `existing-branch`,
  binds the exact source object and any fully qualified branch ref, and places   the destination at one absent direct child of   `<repository>/.worktrees/<task-agent>`. It requires the session envelope's   worktree-creation grant and cannot force an already checked-out branch.
- **Worktree move:** binds the exact current worktree digest and moves only to
  one absent direct child of the same repository-owned `.worktrees` directory.
- **Worktree remove:** selects `clean` or `force`, binds the exact worktree and
  worktree-state digest, and refuses the primary worktree or a locked worktree.   `clean` rejects modified, untracked, conflicted or unmerged state. `force`   always uses the gate variant and binds those consequences; no grant can   silently enable it.
- **Upstream tracking:** `upstream-set` binds one exact local branch and one
  exact registered remote target/ref plus the current local-config digest;   `upstream-unset` binds the exact existing association and digest. The fixed   port changes only the branch's remote and merge keys through a locked atomic   config update. It cannot set an arbitrary Git config key or remote URL.

`git-custody-resolve` is a separate zero-Git-effect `OperatorActionIntent`, not an `OperatorGitIntent` or mutation variant:

```yaml
git_custody_resolve:
  project_id: exact-authenticated-project
  project_session_id: exact-session
  expected_session_revision: compare-and-set-integer
  expected_session_generation: fenced-generation
  coordination_run_id: exact-run
  expected_run_revision: compare-and-set-integer
  expected_dependency_revision: compare-and-set-integer
  authority_ref: exact-current-sha256
  expected_authority_revision: compare-and-set-integer
  draft_id: exact-custody-resolution-draft
  expected_draft_revision: compare-and-set-integer
  draft_digest: exact-sha256
  operation_id: exact-draft-operation-id
  custody_id: exact-unresolved-git-custody
  expected_custody_state: ambiguous-or-quarantined
  expected_lookup_generation: compare-and-set-integer
  lookup_evidence_digest: exact-sha256
  resolution_eligibility_reason: exact-daemon-reason-code
  adjudication: applied-or-no-effect-or-quarantine-accepted
  reason: bounded-non-empty-human-reason
  gate_id: exact-human-approved-operation-gate
  expected_gate_revision: compare-and-set-integer
  expected_gate_status: approved
```

The target must already carry a daemon-persisted `resolution_eligible` marker for that lookup generation and evidence digest after the bounded inspector has declared machine proof permanently unavailable. Ordinary pending lookup and an intact, still-observable typed conflict are ineligible. A prior conflict is eligible only after the exact observe path atomically transitions every custody owner to `quarantined` and records either complete destroyed/altered proof with `conflict-state-unverifiable`, or one of the closed machine-derived permanent `inspector-unavailable`/`evidence-integrity-failure` outcomes above. Transient incomplete/unavailable/inconsistent evidence never suffices. Draft creation uses the `custody-resolution` binding. Its payload digest covers the exact current project/session/run authority, target custody/state, lookup generation/evidence, eligibility reason, adjudication and human reason, but excludes the later daemon-assigned draft/operation identity, gate identity and credential. It follows the exact operation-draft/ gate flow. Final Preview is read-only. Confirmed Commit requires the distinct `git-custody-resolve` capability and an independently attested direct-human approval of the exact adjudication/reason; `git`, `git-authorise`, `decide`, policy or a gate for another operation is insufficient.

Commit performs no Git process, filesystem, ref, index, worktree, configuration or remote mutation. In one transaction it preserves the machine evidence, appends one immutable human-adjudication record, terminalises the target custody and admission, releases its reservation for `applied`/`no-effect` or retires it for `quarantine-accepted`, and terminalises the resolution command/admission. The receipt says `human-adjudicated-applied`, `human-adjudicated-no-effect` or `human-adjudicated-quarantine-accepted`; it never rewrites or presents the machine outcome as proved. Exact replay returns that record. A changed custody state, lookup generation, evidence digest, reason, adjudication, gate or operation ID conflicts with zero state change.

A human-adjudicated result removes only that custody's liveness/reservation blocker. It does not restore repository state, authorise another Git action, advance a project session automatically or imply acceptance. For project- session closure, `quarantine-accepted` is the explicit abandonment with reason record; other closure predicates and an explicit lifecycle transition still apply.

Every mutation variant owns one closed `git_result_recipe_v1`. It includes the execution profile and algorithm IDs; canonical before state; exact expected success and, where admitted, conflict states; no-effect proof fields; at most 64 atomic ref/ config updates; at most 128 input/output commit mappings; at most 4,096 conflict paths/index stages; and bounded index/worktree/config digests. Every produced commit mapping contains ordered parents, tree, author and committer identities and timestamps, message and derived object digest. The recipe and its digest are part of `effect_binding_digest` and expected terminal custody state. A backend, Git binary, configuration, selected identity/timestamp or result-bound change therefore requires a new Preview and gate/grant decision. An effect whose exact result or conflict state cannot be computed within these limits is unavailable in V1; it does not dispatch with an open-ended post-state.

All ref names are fully qualified, validated data. The fixed Git port resolves them to current native objects and verifies the protocol object digests under the bound execution profile; no abbreviated revision, caller argument vector, shell, alias, hook, editor, pager, unregistered helper, environment override or arbitrary Git subcommand is accepted. Immediately before the durable `prepared -> dispatching` transition it shall hold the operation-specific repository/index/ref/config/worktree fence, re-observe every bound filesystem and Git state, and retain that fence or a native compare-and-set through the first mutation. A platform that cannot supply the required fence exposes the variant as unavailable. Quarantining an already unauthorised stale-state mutation is not a substitute.

The Preview shows repository, worktree, execution profile, current branch, exact affected paths, source/destination refs and objects, registered remote target, operation variant, deterministic result recipe and bounds, expected session/run/dependency/authority revisions, and the grant or gate evidence before confirmation.

Added requirements are:

- **FR-038:** Every Git mutation shall carry one closed, current
  `GitActionAuthorisation` whose preauthorised or gate variant binds the exact   project session, run, dependency and authority revisions, repository,   worktree, execution profile, concrete operation variant, registered remote   target, refs, paths and result recipe; a broad `git` capability alone shall   have zero effect.
- **FR-039:** Merge and rebase shall implement only the closed start,
  continue and abort variants above. Branch deletion and worktree create/remove   shall implement only their named safe/force variants. Pull merge/rebase,   history rewriting or destructive force shall require the exact human-approved   gate variant.
- **FR-040:** A Git grant shall be issued or revoked only through launch custody
  materialising an approved positive run allow-list or a distinct   `git-authorise` capability. It shall hash the exact issuing session/run/   dependency revisions as provenance and fence use by the current session   generation, run-authority tuple and allow-list epoch/digest; unrelated later   orchestration revisions shall not invalidate it.
- **FR-041:** Remote Git and upstream-tracking actions shall bind a daemon-owned
  secret-free remote registration identity, target digest and generation; a   reused display name or project Git configuration shall confer no authority.
- **FR-042:** Every gate-only Git mutation or custody resolution shall allocate
  one immutable operation ID and binding digest in a typed no-authority draft   before gate creation; only confirmed final Commit may consume that draft and   create or resolve effect custody.
- **FR-043:** Permanently unprovable ambiguous/quarantined Git custody,
  including a persisted conflict whose exact bounded state is proved destroyed   or altered out of band, shall   remain blocking until machine proof or one exact, independently attested,   gate-bound `git-custody-resolve` adjudication; human adjudication shall remain   distinguishable from machine proof in every receipt and projection. Only the   exact `git-custody-resolve`-capable Git-only observe reconciliation may   classify a persisted conflict as `conflict-state-unverifiable`.
- **NFR-022:** Typed Git execution shall use one fixed bounded port with no
  arbitrary shell, command, option, hook, editor, pager, executable or   environment injection surface.
- **NFR-023:** Each Git effect shall be prepared durably before process I/O and
  shall use evidence-only lookup after ambiguity; restart shall never blindly   repeat a Git mutation.
- **NFR-024:** Every Git mutation shall acquire its declared native lock/CAS
  plan, recheck bound Git and filesystem state immediately before dispatch and   preserve that fence through the first mutation; an unfenceable variant shall   fail unavailable before mutation.
- **NFR-025:** Commit-producing actions shall bind one pinned deterministic
  backend and complete bounded output recipe into the authority digest; hostile   Git configuration, attributes and helpers shall be disabled or rejected   before Preview.
- **NFR-026:** Typed Git binding, generic custody, operation admission and
  common-directory reservation states shall transition through one enforced   atomic mapping; restart shall fail closed on an impossible combination.

Acceptance additionally requires:

- **AC-031:** a matrix over missing, expired, revoked, wrong-project,
  wrong-session/generation, tampered or nonexistent issuing provenance, stale   run-authority/allow-list or grant revision, wrong repository/worktree/   execution profile, sibling operation variant, remote target/generation, ref   and path proves zero Git process I/O. A valid grant remains usable through   unrelated session/run/dependency revision advances after a fresh action   Preview; pull merge/rebase, push and every gate-only mode reject broad   authority and a gate for another operation ID.
- **AC-032:** real temporary-repository tests cover every closed Git effect and
  mode, including current-branch/no-autostash rebase, merged-only versus force   deletion, the three worktree-create modes, clean versus force removal,   merge/rebase continue/abort and upstream set/unset. Exact command replay has   one effect; stale repository/filesystem state at every lock/recheck/CAS point   fails before mutation; crash at each custody boundary performs no blind retry   and exposes any partial merge, rebase, pull or remote effect as ambiguous or   quarantined evidence.
- **AC-033:** grant issuance rejects absence of `git-authorise`, an absent or
  negative parent allow-list, every widened operation/profile/remote/ref/path/   bound and a concurrent session/run/dependency/authority rotation. Direct SQL   and public protocol tests prove preauthorised final Commit creates one exact   authorised admission, while gate-draft creation makes one no-authority   prepared admission and only final Commit may consume it under the same-   session/run `(gate_id, operation_id)` association and human-approved current   dependency revision.
- **AC-034:** hostile hook, filter, process, merge/diff driver, include, alias,
  editor, pager, signer, credential/remote helper and SSH command canaries never   execute. Preview is byte-identical across wall time and mutable Git config for   one pinned profile; merge, pull and rebase recover only the exact bounded   commit mapping or conflict state. Retargeting `origin` under the same name   invalidates the old grant, and upstream tracking can change only through the   target-bound typed variants.
- **AC-035:** exact gate-draft replay returns one operation ID; changed payload,
  operation-kind substitution, early custody/reservation creation, gate binding   by kind, final Preview writes and expired/cancelled/stale draft reuse all fail.   Confirmed Commit atomically consumes one exact approved draft or changes   nothing, and crash at every draft/gate/Commit boundary grants no mutation.
- **AC-036:** every conflict, ambiguity, quarantine, typed successor and
  terminal outcome matches the four-owner persistence table across restart.   `git-custody-resolve` rejects stale generation/evidence, ineligible custody,   every custody still in `conflict`, wrong gate/capability/provenance and   replay with changed reason or adjudication. An explicit reconcile of an   intact owned or inherited conflict retains/promotes exactly one owner with   zero Git mutation; complete proof that its   persisted conflict state was destroyed or altered out of band, including   after successor Commit but before dispatch, atomically   quarantines all four owners, retains the reservation and records   `conflict-state-unverifiable`; incomplete observation advances only the   bounded lookup evidence/audit revision and retains every owner without   eligibility while transient. Immediate machine proof of pinned-inspector   absence/revocation or canonical-evidence integrity failure, and exactly the   third identical unavailable/inconsistent failure signature under the bounded   rule, instead quarantines every owner with the matching permanent eligibility   reason; a different signature resets the streak.   The subsequent exact adjudication Commit makes zero Git call, atomically   releases/retires the reservation, preserves machine evidence, records the   human-labelled result and removes only the exact closure blocker. Closed   request/status codecs reject missing, extra, crossed-lineage and cross-   variant fields; target and reconciliation command queries expose exact   target `pending/prepared`, `ambiguous`, `conflict`, `quarantined` or   reconciliation-command `pending/observing` state. Exact   reconcile replay is read-only and stable, changed replay conflicts, and   incomplete evidence cannot set eligibility. A transfer or competing lookup   between inspection and final CAS terminalises only the reconciliation command   with that exact closed rejection; exact replay makes no inspection, no   custody row changes and a new command must bind the current tuple.

### Admission-bound provider routes and certifying review

This contract closes the current Console review path. It does not add a continuity router, automatic context-pressure controller, Pareto selector, native-routing mode or capability-snapshot policy. The existing trusted model router remains a structural resolver; Fabric owns review currency, immutable source custody and certification.

#### Publication-time publisher custody

Every artifact registration receives one immutable publication-lineage snapshot in the registration transaction. For an agent publisher, the only proved provider join is:

~~~text
authenticated publishing agent + principal generation
  -> one current active retained bridge generation
  -> one immutable active provider-session lineage
       -> launched-chair bridge + project-session launch custody
       or retained-child bridge + provider-agent custody
  -> exact provider-session generation
  -> activated adapter ID/contract + admitted family/model + route when owned
~~~

The daemon derives the bridge and action from the authenticated connection. The publish request carries no custody, family, model, route or independence claim. The complete closed snapshot is:

~~~yaml
artifactPublicationLineageV1:
  schemaVersion: 1
  artifactId: exact-registration
  artifactRevision: positive-publication-revision
  publisherKind: agent-or-operator-or-fabric-or-project
  publisherRef: exact-registration-publisher
  publisherAgentId: null-or-exact-agent
  publisherPrincipalGeneration: null-or-positive-generation
  publisherBridgeGeneration: null-or-positive-active-generation
  providerCustodyRef:
    oneOf:
      - null
      - ownerKind: launched-chair-or-retained-child
        adapterId: exact-activated-adapter
        actionId: exact-provider-agent-custody-action
        providerSessionGeneration: positive-generation
        adapterContractDigest: sha256-prefixed-digest
        routeReceiptDigest: null-or-sha256-prefixed-owned-route
        modelFamily: canonical-family
        model: exact-admitted-model
  state: proved-or-unproved
  reason: proved-or-non-agent-or-no-active-bridge-or-no-session-lineage-or-ambiguous-session-lineage-or-crossed-generation
  lineageDigest: sha256-prefixed-canonical-snapshot-digest
~~~

A proved row requires one exact same-run, same-agent, same-principal-generation and same-provider-session join. Chair activation writes its session-lineage row from launched-chair bridge plus launch custody; child activation writes the same closed row from retained-child bridge plus provider-agent custody. Adapter contract, family and model are mandatory. Route digest is mandatory only when that custody owns a route and is otherwise null; reviewer-family eligibility requires proved family, not an invented route. Zero, multiple, stale or crossed joins are unproved. Later bridge rotation, provider action, route, registry revision or artifact-kind change cannot rewrite the snapshot.

A certifying the Console contract target is eligible only when its root evidence registration is an agent-published project-file or run-file of kind implementation-delivery-manifest.v1, its lineage is proved, and its publisher family equals the target chair family. Operator-, Fabric- and project-published artifacts and git-private-diff registrations retain honest unproved lineage. They may be covered objects in a review bundle but cannot be the root target.

#### Linear slot heads and immutable mutation receipts

Each target owns exactly four review_slot_heads keyed by run, target generation and slot. A new target creates generation-zero heads with no current evidence and carries forward the predecessor's complete open finding records and repair-required set. Head and attempt generations are contiguous.

Certifying dispatch atomically reserves the exact target/slot/head generation, increments its attempt generation and snapshots prior evidence/open findings into the action/prompt. One nonterminal attempt may own that tuple. Target prepare rejects while any prior-target attempt is nonterminal; it cannot leapfrog a late result. Provider failure/no-effect/integrity retirement closes the attempt without advancing evidence, after which a later action may reserve the same evidence head under the next attempt generation.

A provider-terminal-failure therefore exports the receipt's closed `reviewTerminalFailureRecord`, not a review-evidence record. It binds the action/target/slot/task/attempt and terminal sequence, exact four-code failure and digest, terminal/route/prompt/provider/model/bundle/profile identities, and the unchanged head/open/repair set digests. It has no answer, verdict, evidence ID, prior/new head or mutation receipt. Completion may project that terminal action with evidence null and the unchanged head; review-evidence read/list remain evidence-only.

A safe or UNUSABLE provider terminal transaction is different: it automatically creates one daemon-derived immutable evidence ID and reviewEvidenceMutationReceiptV1 and CAS-advances exactly that slot head before the terminal result becomes visible. There is no terminal-unrecorded state and no chair choice to discard an adverse result. The transaction validates provider-reportedResolvedFindingDigests against the action's complete prior set. It stores both that reported set and daemon-acceptedResolvedFindingDigests. The accepted set equals the reported set only when the answer is safe, the mandatory-read predicate is satisfied, the target/source/delivery/chair/profile snapshot is still current at terminalisation, and each finding is eligible for resolution on this successor target. Otherwise it is empty. In particular an in-flight result against a logically stale target, an insufficient-coverage result and a same-target repair-required finding resolve nothing. The daemon computes:

~~~text
new open set =
  sorted unique((prior open set - daemon-accepted-resolved set)
                + new safe finding digests)
~~~

UNUSABLE resolves none. Insufficient-coverage CLEAN is UNUSABLE; insufficient-coverage FINDINGS remains visible FINDINGS/noncertifying and adds all safe findings while accepting no resolution. Every safe P0-P2 finding enters the repair-required set automatically and cannot resolve on the same target. A stale-target terminal result is still settled, recorded and head- advancing against its reserved tuple; its currency is stale/noncertifying, its accepted resolution set is empty and its safe new findings remain open for the successor bundle. The record/receipt carry prior/new head and attempt generations, prior evidence, complete prior open records, reported and accepted resolved subsets, current findings, complete new open records, readCoverageDigest and immutable gap summary. A second FINDINGS action begins from the returned head and advances normally. A repaired target carries the full safe ID/severity/summary/evidence plus origin target/action/result and lets a fresh current, sufficient-coverage CLEAN resolve their digests.

~~~yaml
reviewEvidenceMutationReceiptV1:
  schemaVersion: 1
  evidenceId: exact-daemon-derived-id
  actionRef:
    adapterId: exact-adapter
    actionId: exact-action
  terminalSequence: positive-run-sequence
  targetGeneration: positive-generation
  slot: exact-profile-slot
  attemptGeneration: positive-generation
  priorHeadGeneration: nonnegative-generation
  newHeadGeneration: positive-generation
  priorEvidenceId: null-or-exact-id
  terminalResultDigest: sha256-prefixed-digest
  terminalInputDigest: sha256-prefixed-private-journal-digest
  reportedResolvedSetDigest: sha256-prefixed-digest
  acceptedResolvedSetDigest: sha256-prefixed-digest
  findingSetDigest: sha256-prefixed-digest
  newOpenSetDigest: sha256-prefixed-digest
  repairRequiredSetDigest: sha256-prefixed-digest
  readCoverageDigest: sha256-prefixed-digest
  coverageSummaryDigest: sha256-prefixed-digest
  findingWindowDigest: sha256-prefixed-digest
  certificationBasisAtTerminalDigest: sha256-prefixed-digest
  mutationReceiptDigest: sha256-prefixed-canonical-receipt-digest
~~~

The receipt contains no live currency, usage, raw answer or mutable annotation.

`fabric.v1.review-evidence.annotate` is the current chair's optional non-gating annotation of an already automatic evidence record. Its disposition is exactly one of `substantiated`, `unsubstantiated`, `duplicate` or `needs-more-evidence`; no free-form or provider-specific disposition is valid. The request supplies exact evidence/result/head equality and one bounded inert note. The daemon appends this separate record and advances one annotation head:

~~~yaml
reviewEvidenceAnnotationV1:
  schemaVersion: 1
  evidenceId: exact-evidence
  annotationRevision: positive-contiguous-revision
  priorAnnotationRevision: null-for-one-otherwise-exact-prior
  commandId: exact-chair-command
  chairBindingGeneration: exact-active-target-binding
  disposition: substantiated-or-unsubstantiated-or-duplicate-or-needs-more-evidence
  note: bounded-inert-utf8-at-most-512-bytes
  noteDigest: sha256-prefixed-digest
  annotationDigest: sha256-prefixed-canonical-record-digest
~~~

Annotation rows are append-only; the head is a compare-and-set pointer, so one current annotation projection exists per evidence while history remains immutable. `review-evidence.read/list` returns current `annotation` as a sibling of immutable `record` and live `currency`. The Console displays its disposition, note digest/revision and note when safe. Annotation cannot create evidence, change a head, verdict, findings, repair-required set, reviewer-family relation, currency or completion. Fabric receipt v2 and `reviewCompletionV1` contain no annotation field or count. Exact command replay returns its immutable annotation receipt before any live-chair check; changed replay conflicts.

The original dispatch command receipt never changes: exact dispatch replay always returns its committed prepared/dispatched receipt. Terminalisation uses the internal idempotency key action pair/target/slot/attempt-generation and stores a separate immutable terminal journal plus a canonical terminal-input digest over the terminal discriminator, private answer/adapter-result digests, authenticated usage and read-coverage journal digest. An exact duplicate returns the stored terminal projection. A changed live-callback/lookup input digest is an integrity conflict: it appends a quarantine record, cannot overwrite terminal result, evidence, head or settlement, and makes the reducer emit integrity-failure. provider-action.read exposes the terminal result plus automatic evidence mutation receipt. Neither that nor annotation receipt contains currency. review-evidence.read/list return immutable record plus fresh reviewEvidenceCurrencyV1. No command replay calls that reducer.

Only a succeeded target-preparation Phase B supersedes a target, and it first proves every old-target attempt terminal and every safe/UNUSABLE terminal already atomically reflected in its head. It then carries the complete open records forward in the successor bundle. A source change can never launder a late finding.

An ambiguous certifying action is nonterminal and owns the target/slot attempt, reservation and head fence. While it remains ambiguous or awaiting-human- retire, the daemon rejects every new action for that slot, every successor Phase-B supersession and review/run acceptance or close. Preparation may be accepted and built, but remains fenced at Committing until recovery terminalises the action or retirement succeeds. It is therefore an explicit review-and-liveness recovery gate as well as a budget hold. Only proved terminal reconciliation or confirmed provider-route-integrity-retire releases the fence; ordinary retry, Resume, annotation or source change cannot.

#### Completion reducer and deterministic blockers

fabric.v1.review-completion.read is the sole agent/operator reducer. It reads the one current target, its resolved profile and the four slot heads, not unsuperseded timestamps or a latest-row guess. Operator calls require exact project/session/run read authority. The Console receives the same result through Evidence/System projection.

The public read/annotation wires are exact closed objects:

~~~yaml
reviewEvidenceReadRequestV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  evidenceId: exact-evidence

reviewEvidenceListRequestV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  targetGeneration: null-or-positive-generation
  slot: null-or-native-or-other-primary-or-cursor-grok-or-agy-gemini
  pageSize: integer-1-through-100
  cursor: null-or-daemon-issued-opaque-cursor-at-most-256-bytes

reviewEvidenceListResultV1:
  schemaVersion: 1
  entries: ordered-reviewEvidenceReadV1-at-most-pageSize
  nextCursor: null-or-daemon-issued-opaque-cursor-at-most-256-bytes

reviewCompletionReadRequestV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run

reviewEvidenceAnnotationAppendRequestV1:
  schemaVersion: 1
  commandId: stable-command-id
  projectSessionId: exact-session
  coordinationRunId: exact-run
  evidenceId: exact-evidence
  expectedResultDigest: sha256-prefixed-digest
  expectedHeadGeneration: nonnegative-generation
  expectedAnnotationRevision: nonnegative-zero-if-none
  disposition: substantiated-or-unsubstantiated-or-duplicate-or-needs-more-evidence
  note: inert-UTF8-at-most-512-bytes

reviewEvidenceAnnotationCurrentReadRequestV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  evidenceId: exact-evidence

reviewEvidenceAnnotationCurrentReadResultV1:
  schemaVersion: 1
  evidenceId: exact-evidence
  annotation: null-or-reviewEvidenceAnnotationV1

reviewFindingPageReadRequestV1:
  schemaVersion: 1
  projectSessionId: exact-session
  coordinationRunId: exact-run
  findingSetDigest: exact-authorised-finding-set-digest
  pageDigest: exact-page-digest-listed-by-that-set

reviewFindingPageReadResultV1:
  schemaVersion: 1
  findingSetDigest: exact-request-digest
  pageDigest: exact-request-digest
  members: ordered-nonempty-safeFinding-records
  nextPageDigest: null-or-next-page-digest-in-set-order

reviewReadErrorV1:
  schemaVersion: 1
  code: NOT_FOUND-or-AUTHORITY_DENIED-or-SCOPE_MISMATCH-or-STALE_CURSOR-or-STALE_REVISION-or-INTEGRITY_FAILURE
  currentRevision: null-or-nonnegative-integer
  evidenceDigest: null-or-sha256-prefixed-digest
~~~

`review-evidence.read/list`, `review-finding-page.read`, `review-completion.read`, `review-evidence.annotate` and `review-evidence-annotation.current.read` accept only their displayed request, return only their displayed success shape or `reviewReadErrorV1`, and reject unknown fields. List order is target generation, slot profile rank, new-head generation and evidence ID. Cursors bind the exact scope/filter/watermark and never carry authority. Annotation append returns the immutable `reviewEvidenceAnnotationV1`; exact command replay precedes live CAS. The finding-page read requires its set to be reachable from authorised evidence, completion or receipt state and the page to occur in that set's ordered vector. Its members hash to `pageDigest`; `nextPageDigest` is the next vector member or null. Missing/cross-set/orphan/digest-mismatch reads return no partial members.

The result names target/chair/bundle/coverage/profile digests and one row per slot. A slot is clean only when its head names one current terminal safe certifying CLEAN evidence record, its complete open-finding set is empty and every profile requirement matches. A proved provider terminal failure is noncertifying and yields provider-terminal-failure; it is never ambiguous.

review-evidence.read/list return this closed shape (list repeats entries under one page envelope); completion returns the same immutable identities rather than a lossy Console-only model. `record` is byte-shape-identical to receipt `$defs.reviewEvidenceRecord`, every finding is receipt `$defs.safeFinding`, `coverageSummary` is receipt `$defs.coverageSummary`, and `reviewCompletionV1` is byte-shape-identical to receipt `$defs.reviewCompletion`; implementation defines each once and reuses it:

~~~yaml
reviewEvidenceReadV1:
  schemaVersion: 1
  record: receipt.$defs.reviewEvidenceRecord
  currency:
    target: current-or-stale-or-superseded
    source: current-or-stale
    chair: current-or-stale
    profile: current-or-stale
    currentCertificationBasis: null-or-reviewCertificationBasis
    certifying: true-or-false
    blockerCodes: ordered-closed-codes
  annotation: null-or-current-reviewEvidenceAnnotationV1

reviewCompletionV1:
  schemaVersion: 1
  blockers: ordered-unique-target-wide-blocker-codes
  targetGeneration: null-or-positive-generation
  targetChair: null-or-exact-target-chair-snapshot
  reviewedArtifactRef: null-or-exact-artifact-revision
  publicationLineageDigest: null-or-sha256-prefixed-digest
  bundleDigest: null-or-sha256-prefixed-digest
  manifestRootDigest: null-or-sha256-prefixed-digest
  coverageDigest: null-or-sha256-prefixed-digest
  riskReadMapDigest: null-or-sha256-prefixed-digest
  mandatoryReadSetDigest: null-or-sha256-prefixed-digest
  profileDigest: null-or-sha256-prefixed-digest
  unavailableSlots: ordered-certifyingSlotUnavailable-records
  slots:
    oneOf:
      - empty
      - exactlyFour:
          - slot: exact-profile-slot
            headGeneration: nonnegative-generation
            attemptGeneration: nonnegative-generation
            actionRef: null-or-ProviderActionRefV1
            evidenceId: null-or-exact-evidence
            terminalKind: null-or-safe-answer-or-unusable-answer-or-provider-terminal-failure-or-terminal-no-effect-or-integrity-terminal-or-retired-unknown
            verdict: null-or-CLEAN-or-FINDINGS-or-UNUSABLE
            resultDigest: null-or-sha256-prefixed-digest
            providerFailureCode: null-or-max-turns-exhausted-or-provider-rejected-or-terminal-no-answer-or-adapter-terminal-failure
            providerFailureDigest: null-or-sha256-prefixed-digest
            routeReceiptDigest: null-or-sha256-prefixed-digest
            adapterId: exact-resolved-adapter
            endpointProvider: exact-required-provider
            providerFamily: exact-resolved-family
            model: exact-resolved-model
            routeObservationDigest: null-or-sha256-prefixed-digest
            actualRouteIdentityDigest: null-or-sha256-prefixed-digest
            readCoverageDigest: null-or-sha256-prefixed-digest
            reviewerFamilyRelation: same-family-exempt-or-distinct-family-proved-or-same-family-forbidden-or-family-unproved
            currentCertificationBasis: null-or-reviewCertificationBasis
            certifying: true-or-false
            openFindingSet: findingSetRef
            blockers: ordered-slotReviewBlockerEnum
  finalReviewComplete: true-or-false
~~~

The operator Evidence row/detail projection is also closed; it does not require the Console to join private tables:

~~~yaml
operatorReviewEvidenceRowV1:
  schemaVersion: 1
  oneOf:
    - rowKind: evidence
      required: [rowKind, evidence, targetChair, reviewedArtifactRef,
        publicationLineageDigest, headGeneration, p0Count, p1Count, p2Count,
        openFindingCount]
      evidence: reviewEvidenceReadV1
    - rowKind: terminal-action
      required: [rowKind, terminal, targetGeneration, targetChair,
        reviewedArtifactRef, publicationLineageDigest, slot, headGeneration,
        attemptGeneration, taskId, openFindingSet]
      terminal: providerActionTerminalProjectionV1
    - rowKind: recovery-action
      required: [rowKind, recovery, targetChair, reviewedArtifactRef,
        publicationLineageDigest, openFindingSet]
      recovery: providerRouteIntegrityRecoveryProjectionV1
  sharedCounts: p0Count/p1Count/p2Count/openFindingCount-are-nonnegative
~~~

An evidence row nests the exact evidence read and its current annotation. A terminal-action row exists only for a terminal kind that creates no review evidence and carries the unchanged head/open set. A recovery-action row nests the live recovery projection, including current CAS generation/state and retirement eligibility; it is the only row from which the Console may prepare retirement. Counts derive from the nested finding sets. Every arm rejects fields owned by another arm and equality-joins action/result/task/prompt, target, route, profile and reviewer-family identities. Raw answer, prompt, diagnostics and usage remain absent.

Top-level and slot blocker domains are disjoint. Top-level precedence is exactly: `certifying-review-capability-unavailable`, `finding-capacity-exhausted`, `missing-target`, `stale-target`, `profile-unavailable`, `integrity-failure`. Slot precedence is exactly: `missing-evidence`, `nonterminal-action`, `ambiguous-action`, `provider-terminal-failure`, `terminal-no-effect`, `retired-unknown`, `route-integrity`, `insufficient-read-coverage`, `noncertifying`, `actual-route-mismatch`, `actual-route-unproved`, `unusable`, `wrong-artifact`, `wrong-bundle`, `wrong-route`, `wrong-provider`, `wrong-model`, `wrong-chair-generation`, `reviewer-family-distinctness`, `open-findings`. A code cannot appear in both places. `superseded` is only a historical `reviewEvidenceCurrencyV1.target` value and is not a completion blocker.

Capability and finding-capacity checks run first and return their typed branch even before target creation. Otherwise zero current targets returns exactly missing-target. Multiple/no trustworthy targets returns the target-null integrity arm. One trustworthy immutable target with a missing/broken binding, profile/head cardinality, CAS chain or contradictory immutable join returns the target-present structural-integrity arm: target immutable fields remain exact, targetChair/profile are null and slots is empty. A merely unavailable profile uses its dedicated target-present arm. With a structurally valid target/profile, slots contains exactly four rows; stale-target is top-level only.

`actual-route-mismatch` takes precedence over `actual-route-unproved` when any observed route field is unequal; otherwise incomplete required identity proof emits only `actual-route-unproved`. `open-findings` is emitted iff the slot head's complete paged open set is nonempty. A provider failure row uses `provider-terminal-failure` and unchanged head/open/repair sets; it never masquerades as missing evidence. The generated reducer truth-table fixture enumerates every top arm and every slot cause, proves domain disjointness/precedence and rejects duplicates or impossible cross-arm fields. `finalReviewComplete` is true only when top blockers are empty, the current trustworthy target/profile and four slots exist, every slot blocker array is empty and finding capacity admits a normal action.

The operator Evidence row/detail and fabric-receipt.json expose exact safe records, slot heads, route, target, chair, bundle/coverage/profile and recovery digests. Raw answer, provider error, private diagnostics, bundle objects/chunks, prompt content, secret-set HMAC, adapter result and usage are absent. Current annotations are available only through live Evidence read/projection and remain absent from review completion and fabric-receipt.json.

#### Requirements and acceptance

- **FR-053:** Certifying review shall bind a daemon-generated complete
  review-bundle, coverage digest, current target/chair/profile snapshot and   action-pair-only content-addressed portal before provider I/O.
- **FR-054:** New certifying dispatch and optional evidence annotation shall
  require the current target chair, while the action-bound safe/UNUSABLE   terminal transaction shall always settle, create evidence and advance its   reserved head despite later currency drift; exact immutable replay precedes   live fences.
- **FR-055:** Publication lineage shall bind the authenticated publishing
  principal generation to one exact active bridge/provider custody/route at   publication; unproved or non-seal root artifacts shall remain ineligible.
- **FR-056:** One checked-in four-slot profile and one linear per-target/slot
  head shall own provider requirements, reviewer-family relation, evidence order   and open findings.
- **FR-057:** Raw review answers and terminal provider diagnostics shall remain
  private; safe results, proved failures and digests alone are public.
- **FR-058:** Route resolution shall remain structural, bounded and
  side-effect-free; durable replay and stable-key single-flight shall precede   router execution.
- **FR-059:** One recovery service shall own every certifying action before
  generic recovery and close intact ambiguity or route-integrity budget custody   by proved release, exact-or-conservative proved-effect settlement, or exact   direct-human full-ceiling retirement without route reconstruction or provider   replay.
- **FR-060:** Mutation receipts shall remain immutable; live review currency
  shall appear only on read/list/projection results.
- **FR-062:** Review-target preparation shall return one immutable bounded
  accepted receipt, continue through the durable preparation state machine and   commit or conflict exactly one reserved target generation under crash-safe   recovery.
- **FR-063:** A proved same-agent lifecycle adoption shall advance one
  append-only target-chair binding without changing the review subject, heads,   evidence or findings; late old-binding output shall remain adverse but   noncertifying.
- **FR-064:** Portal-helper transport shall use the pinned Rust supervisor,
  non-secret Unix-socket locators, authenticated peer/process identity and one   TypeScript semantic/ledger owner; no provider inherited descriptor or bearer   handoff shall exist.
- **FR-065:** Every provider action reference, pre-router flight, durable
  preflight, adapter/recovery journal, receipt, sort, join and Console row shall   use the daemon-global `(adapterId, actionId)` pair.
- **FR-066:** Review annotations shall use the exact four-value vocabulary in a
  separate append-only relation and live projection, with zero effect on   receipts or completion.
- **FR-067:** Completion shall expose unavailable certifying slots and the
  target-wide capability blocker even before a target exists.
- **FR-069:** Receipt v2 shall represent evidence and terminal-failure review
  records separately, keep provider route/substitution history append-only and   validate every current wire without an external resolver or legacy alias.
- **FR-070:** Review-bundle coverage, requirement/evidence/source/mandatory
  digests and every array order shall have one exact JCS preimage and golden   permutation fixtures.
- **FR-071:** Finding custody shall be paged/content-addressed without
  truncation; normal actions shall reserve worst-case capacity before router I/O   and bounded resolution-only recovery shall never certify completion.
- **FR-072:** True-chair lifecycle adoption shall capture one immutable
  terminal-sequence certification cut and perform automatic same-subject   rebind-or-stale without waiting on review actions; the exact public rebind   operation shall execute or replay the same deterministic transition and   immutable receipt without accepting caller-authored subject claims.
- **FR-073:** Completion shall use disjoint target/slot blocker domains and
  expose a target-present structural-integrity arm.
- **FR-074:** Live route-recovery projection alone shall supply retirement CAS
  authority; receipt recovery remains audit-only.

Acceptance additionally requires:

- **AC-043:** bundle fixtures prove complete changed-file and required-evidence
  derivation through the exact review-diff.v1 status/mode/binary/rename/path/   ordering/digest rules, immutable full-ID conformance manifest, exact base/   head/clean-state binding, all object/chunk/coverage digests, size/count   limits, portal isolation and source/delivery/chair/profile supersession. The   final run-start-to-sealed-HEAD oracle recomputes its own counts/bytes and a   64-MiB+1 closure fails. Omissions, truncation, mutable Git diff configuration,   bundle chaining and stale summaries cannot certify. Golden preimage,   ordering/permutation and copied-digest fixtures cover requirement, evidence,   source, coverage and mandatory-set domains plus paged finding roots/pages.
- **AC-044:** profile fixtures prove exact Codex/Claude primary mapping,
  cursor-agent/xAI and agy/Google routes, native same-family exemption,   publisher eligibility, exact reviewer-family relation, tagged applied versus   inapplicable effort, full availability identity, same-agent binding   continuity and target reprepare after every unrebindable change.
- **AC-045:** router fixtures prove structural codec purity, post-router
  transactional effort/currency/adapter/model checks, five-second process-tree   cancellation, global pair-keyed exact single-flight, requested/resolved   adapter equality, cross-run pair conflict before a second router, legal same   action ID on different adapters and durable replay without router execution.
- **AC-046:** adapter fixtures bind certifying-review-packet-only.v1 to each
  contract digest and prove no mutable cwd, inherited HOME/environment, workspace/source/shell/browser/network tool or cross-bundle portal read for Claude, Codex, Cursor and Agy. Direct Claude/Codex routes prove equal schema, ledger and denial canaries. Cursor/Agy remain capability=false until their pinned supervisor/helper/broker, peer identity, exact two-tool allowlist, outer sandbox, exact portal discovery, pre-artifact intent, closed `portal-stdio-v1` argv and exactly-three-locator environment rejection, inherited non-stdio descriptor refusal, bounded-LF-frame oversize/unterminated negatives, supervisor-FD-3 `CLOEXEC` isolation, wrong/relayed accepted-FD rejection, daemon/supervisor crash recovery, singleton-link canonical-to-trusted-claim rename/retry/substitution races and TERM/250-ms/KILL/setsid/setpgid-group-split/double-fork/reparent canaries pass on the activated build. Launch custody is deliberately not gated on a pre-exec registration handshake: [ADR 0018](../../adr/0018-accept-portal-stdio-v1-launch-custody.md) accepts the residual executable-substitution window instead of building one.
- **AC-047:** terminal fixtures distinguish safe CLEAN/FINDINGS, UNUSABLE,
  proved max-turn/provider/no-answer failure and effect ambiguity; settle exact   authenticated usage or conservatively charge every proved-effect terminal,   release proved no-effect, retain true ambiguity, expose only closed   digests/blockers and never redispatch. Insufficient CLEAN becomes UNUSABLE;   insufficient FINDINGS remains visible/noncertifying with zero accepted   resolutions. A stale in-flight answer still settles and advances its reserved   head with zero accepted resolutions and all safe new findings carried. Six   terminal-result golden vectors bind action pair, stable terminal sequence and   exact arm fields. Provider failure exports an unchanged-head receipt record   with no evidence/new-head fields.
- **AC-048:** head-CAS fixtures cover first and second FINDINGS, UNUSABLE,
  concurrent forks, paged carry-forward repair findings, source/evidence/mixed   repair currency, repaired-target CLEAN and   exact replay, including identical versus conflicting terminal-input digests.   Reducer fixtures prove the disjoint blocker truth table, capacity exhaustion,   zero-I/O admission refusal and noncertifying resolution-only recovery.
- **AC-049:** recovery fixtures cover every unresolved action state, optional
  pair lookup, proved zero-effect release, exact-or-conservative terminal   settlement and direct-human full-reservation retirement with no permanent   freeze, route reconstruction or provider dispatch. They prove all certifying   actions are excluded from generic recovery. Wrong authority/gate/generation   and unconfirmed retirement change nothing; awaiting-human-retire blocks   target Phase B and run acceptance until that exact gate closes. Operator   retirement fixtures obtain pair/generation/state/reservation only from the   live recovery projection and reject receipt snapshots.
- **AC-050:** agent action/read/evidence/completion and operator Evidence/System
  projections enforce exact scope, immutable-receipt versus live-currency   separation, exact evidence/list/annotation/completion/profile/portal wires,   append-only provider events and the standalone receipt-v2 local definitions, sort/equality/   history/count/JCS-hash invariants. The current baseline contains no   model_routing_receipts, cross_family_reviews, modelRoutingReceipts,   crossFamilyReviews, recordModelRoutingEvidence or   recordCrossFamilyReviewEvidence or fabric.v1.review-evidence.record table,   field or API.
- **AC-052:** preparation fixtures prove DB-only acceptance within the public
  deadline, exact replay/join versus changed-input conflict, one active row,   never-reused high-water generations, every state edge, exact conflict/failure   terminal union, phase-only or monotonic verified-item progress, build/   fsync/Phase-B crash recovery, same-generation reclaim, CAS-byte retention and   either one complete target or one terminal conflict with no duplicate.   Rotation racing build commits only against the adopted current same-agent   binding or conflicts.
- **AC-053:** binding fixtures preserve already-current evidence across an
  adopted same-agent rotation, assign stable terminal sequences, capture the   exact predecessor certification cut and terminalise old prepared work no-   effect without blocking lifecycle. Effectful/ambiguous work recovers normally;   post-cut output remains adverse/noncertifying and every crossed agent/profile/   source chain yields stale target rather than failed adoption. Public rebind   fixtures cover exact execution/already-applied replay, changed replay,   pointer/head races, non-adopted custody, crossed identity/subject fields,   multiple contiguous rotations and zero router/provider/portal I/O. Completion   fixtures expose exact unavailable slots before target creation. Annotation   fixtures enforce the four values, append-only current projection and absence   from completion/receipt. Standalone receipt validation uses no resolver and   rejects every future objective/provider/operation code. Cut/basis fixtures   equality-bind the exact agent/custody/revision ref and reject crossed custody.
