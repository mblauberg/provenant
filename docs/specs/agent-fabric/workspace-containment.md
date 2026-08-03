# Agent Fabric workspace trust and containment

## Typed Git grants, effect custody and recovery

the typed Git effect contract owns `GitActionAuthorisation` and the observable Git semantics. This section owns their additive
persistence, the one production Git effect owner and crash recovery. It does not authorise a Git mutation, push,
pull-request merge, release or deployment.

The next unused additive migration after the operator-lifecycle schema shall add immutable revisioned Git grants and a
one-to-one Git binding for the existing `operator_effect_custody` owner. Its ordinal is assigned at serial integration
so it cannot collide with another already approved additive migration. The migration shall not create a second operator
command journal or a parallel effect state machine.

The migration adds `runs.authority_revision` and `runs.git_allowlist_epoch`, each initialised to one, plus nullable
`runs.git_allowlist_digest` and immutable `run_authority_revisions(project_session_id, coordination_run_id,
authority_revision, authority_ref, git_allowlist_epoch, git_allowlist_digest, activated_at_run_revision, created_at)`.
`runs` is the current revision owner. Authority rotation compare-and-sets the current tuple, appends the next contiguous
history row, increments both authority and run revisions and revokes every active Git grant under the old tuple in one
transaction. `runs.dependency_revision` remains the canonical dependency owner; no grant-local counter may substitute
for either revision. The history primary key is `(project_session_id, coordination_run_id, authority_revision)`. The
exact four-column tuple `(project_session_id, coordination_run_id, authority_revision, authority_ref)` is `UNIQUE`, so
the existing `operator_git_effect_bindings` composite foreign key has a valid parent key; the exact six-column tuple
`(project_session_id, coordination_run_id, authority_revision, authority_ref, git_allowlist_epoch,
git_allowlist_digest)` is likewise declared `UNIQUE`, so the allow-list-bound `operator_git_grants` composite foreign
key also has a valid parent key. The history has a composite foreign key to `runs`. Insert/update triggers require the
current `runs` authority tuple to have exactly one matching immutable history row. Adding, replacing or removing
`git_allowlist_v1` appends authority history and advances the run's authority revision/ref and allow-list epoch/digest
in the same transaction. No other run/session/dependency revision change advances the allow-list epoch.

The daemon also persists two trusted registries before accepting a grant:

- `git_execution_profiles` binds profile ID/revision, Git binary path/version/ digest, object format, deterministic
  backend IDs, sanitised config/environment digest, helper/sandbox registry digest and hard result bounds. Trusted
  machine configuration alone may add a profile; project configuration cannot select an execution profile.
- `git_remote_registrations` binds registration ID/revision/generation, project, display name, normalised secret-free
  target identity/digest, transport kind, remote-port adapter/contract and credential-selector digest. Retargeting
  appends a revision and advances generation. One display name may have only one active target per project, but that
  name is never a foreign key or authority identity.

Registry lifecycle changes increment daemon global state. Profiles and remote registrations contain no credential value
or project-selected executable. A grant/action/recovery lookup references their exact composite identity and digest; it
never resolves authority from `.git/config`.

The closed logical grant shape is:

```sql
CREATE TABLE operator_git_grants (
  grant_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  session_generation INTEGER NOT NULL CHECK (session_generation >= 1),
  issuing_session_revision INTEGER NOT NULL CHECK (issuing_session_revision >= 1),
  coordination_run_id TEXT NOT NULL,
  issuing_run_revision INTEGER NOT NULL CHECK (issuing_run_revision >= 1),
  issuing_dependency_revision INTEGER NOT NULL CHECK (issuing_dependency_revision >= 1),
  authority_ref TEXT NOT NULL,
  authority_revision INTEGER NOT NULL CHECK (authority_revision >= 1),
  git_allowlist_epoch INTEGER NOT NULL CHECK (git_allowlist_epoch >= 1),
  git_allowlist_digest TEXT NOT NULL,
  repository_root TEXT NOT NULL,
  worktree_path TEXT NOT NULL,
  execution_profile_id TEXT NOT NULL,
  execution_profile_revision INTEGER NOT NULL CHECK (execution_profile_revision >= 1),
  execution_profile_digest TEXT NOT NULL,
  source_kind TEXT NOT NULL CHECK (source_kind IN ('launch-envelope','operator-command')),
  source_digest TEXT NOT NULL,
  constraints_json TEXT NOT NULL,
  grant_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('active','revoked')),
  expires_at INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  revoked_at INTEGER,
  PRIMARY KEY (grant_id, revision),
  FOREIGN KEY (project_session_id, coordination_run_id)
    REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY (project_session_id, coordination_run_id,
               authority_revision, authority_ref,
               git_allowlist_epoch, git_allowlist_digest)
    REFERENCES run_authority_revisions(
      project_session_id, coordination_run_id, authority_revision,
      authority_ref, git_allowlist_epoch, git_allowlist_digest),
  FOREIGN KEY (execution_profile_id, execution_profile_revision)
    REFERENCES git_execution_profiles(profile_id, revision),
  CHECK (length(authority_ref)=71 AND substr(authority_ref,1,7)='sha256:'),
  CHECK (length(grant_digest)=71 AND substr(grant_digest,1,7)='sha256:'),
  CHECK ((state='active' AND revoked_at IS NULL) OR
         (state<>'active' AND revoked_at IS NOT NULL))
);
```

`constraints_json` is a canonical hash mirror, not the enforcement owner. The migration normalises closed child rows for
concrete operation variants, registered remote identities/revisions/generations/target digests, fully qualified refs and
canonical path prefixes. Operation variants use a closed enumeration and reject every gate-only value. Composite foreign
keys bind each remote child to `git_remote_registrations`. Empty child sets mean unavailable for that category; no query
treats absence as wildcard.

`grant_digest` hashes the immutable grant identity, captured issuing session/ run/dependency provenance, authority and
allow-list tuple, repository/worktree, execution profile, normalised child constraints, source authority and expiry;
later revocation fields are excluded. Launch custody may insert a grant only from the exact approved launch-envelope
digest. Later creation/revision/revocation uses a separate previewed `git-authorise` operator command, verifies its
capability and independently attested human provenance, and proves every child row is a positive subset of the current
run `git_allowlist_v1`. `git` cannot call this path. Missing parent allow-list rows, negative-only authority or any
widened dimension fails before insert.

A revision's binding and constraints are immutable; replacement appends the next contiguous revision and revokes the
prior active revision atomically. Only one compare-and-set `active -> revoked` lifecycle change is allowed. At most one
revision of a grant is active. Point-of-use indexes cover expiry, revocation/state, session generation, authority
revision/ref, allow-list epoch/digest, execution profile and remote registration generation. Separate audit indexes
expose issuing session/run/dependency provenance; current-value queries never compare those issuance fields with the
live orchestration revisions. A grant record never contains a bearer capability, remote credential, URL with
credentials, Git argument vector or executable.

Gate-only identity is owned before gate creation by one bounded table:

```sql
CREATE TABLE git_operation_drafts (
  draft_id TEXT PRIMARY KEY,
  revision INTEGER NOT NULL CHECK (revision >= 1),
  draft_request_id TEXT NOT NULL,
  request_digest TEXT NOT NULL,
  operator_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  observed_session_revision INTEGER NOT NULL,
  session_generation INTEGER NOT NULL,
  coordination_run_id TEXT NOT NULL,
  observed_run_revision INTEGER NOT NULL,
  observed_dependency_revision INTEGER NOT NULL,
  authority_ref TEXT NOT NULL,
  authority_revision INTEGER NOT NULL,
  git_allowlist_epoch INTEGER NOT NULL,
  git_allowlist_digest TEXT,
  draft_kind TEXT NOT NULL
    CHECK (draft_kind IN ('mutation','custody-resolution')),
  operation_id TEXT NOT NULL UNIQUE,
  operation_kind TEXT NOT NULL,
  payload_digest TEXT NOT NULL,
  binding_json TEXT NOT NULL,
  draft_digest TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN
    ('open','gate-bound','consumed','stale','expired','cancelled')),
  expires_at INTEGER NOT NULL,
  consumed_command_id TEXT,
  terminal_reason TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE (operator_id, project_id, project_session_id, draft_request_id),
  UNIQUE (draft_id, operation_id),
  FOREIGN KEY (project_session_id, coordination_run_id)
    REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY (operation_id) REFERENCES operation_admissions(operation_id),
  CHECK ((state='consumed')=(consumed_command_id IS NOT NULL)),
  CHECK ((state IN ('stale','expired','cancelled'))=
         (terminal_reason IS NOT NULL))
);
```

The canonical closed `binding_json` is decoded by the same mutation or custody- resolution codec as the Agent Fabric contract, never as
open JSON. Identity/binding/authority/ observation/expiry fields are immutable; only one-step revisioned lifecycle
fields advance. `draft_digest` hashes every immutable field including `operation_id`; lifecycle/consumption fields are
excluded. Draft creation and its `prepared` operation admission commit together. Exact request replay returns the same
row; changed request content under the dedupe key conflicts. Drafts have a bounded expiry and no repository reservation,
generic effect custody, grant use or liveness contribution. The existing operator command journal owns draft
`create`/`cancel` Preview/ Commit; the draft table is not another command or effect journal.

One active gate association is permitted per draft operation. Gate binding compare-and-sets `open -> gate-bound`.
Confirmed final Commit alone may consume `gate-bound -> consumed` and admission `prepared -> authorised` in the same
transaction that creates mutation custody/reservation or records custody resolution. Final Preview performs no lifecycle
write. A binding mismatch reported by Preview changes nothing; draft reconciliation or a confirmed Commit
compare-and-sets the draft to `stale` without creating effect rows. Expiry or explicit cancellation uses its matching
terminal state. Each terminal path cancels the prepared admission and supersedes every associated gate transactionally.
No terminal draft can be reopened, rebound or copied into a new operation ID. Gate rejection/cancellation/supersession
cancels its draft; gate deferral leaves the bounded draft `gate-bound` until another terminal path.

The common-directory reservation is also a persisted logical owner, not an in-memory mutex:

```sql
CREATE TABLE git_mutation_reservations (
  custody_id TEXT NOT NULL,
  generation INTEGER NOT NULL CHECK (generation >= 1),
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  git_common_dir TEXT NOT NULL,
  common_dir_identity_digest TEXT NOT NULL,
  lock_plan_digest TEXT NOT NULL,
  state TEXT NOT NULL
    CHECK (state IN
      ('reserved','dispatching','conflict','ambiguous','quarantined',
       'released','retired')),
  owner_instance_id TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (custody_id, generation),
  FOREIGN KEY (custody_id) REFERENCES operator_effect_custody(custody_id),
  FOREIGN KEY (project_session_id, coordination_run_id)
    REFERENCES runs(project_session_id, run_id)
);
CREATE UNIQUE INDEX one_active_git_mutation_per_common_dir
  ON git_mutation_reservations(git_common_dir)
  WHERE state IN
    ('reserved','dispatching','conflict','ambiguous','quarantined');
```

Canonical/no-follow common-directory identity, digest and lock plan are immutable. State and owner advance only through
custody-coupled compare-and-set transitions. A terminal machine-proved applied/no-effect/rejected outcome releases the
row in the same transaction; conflict, ambiguity and quarantine retain it. A separately authorised typed continue or
abort atomically transfers a predecessor conflict reservation to the successor custody/generation before dispatch.
Reclaim requires proof that the recorded daemon instance is dead and may remove only that instance's private reservation
artifact, never a Git lock or project file. Only the gate-bound human-resolution transaction below may retire an
unprovable reservation without machine outcome proof.

The one-to-one binding for the existing custody owner is logically:

```sql
CREATE TABLE operator_git_effect_bindings (
  custody_id TEXT PRIMARY KEY
    REFERENCES operator_effect_custody(custody_id),
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  prepared_session_revision INTEGER NOT NULL
    CHECK (prepared_session_revision >= 1),
  session_generation INTEGER NOT NULL CHECK (session_generation >= 1),
  coordination_run_id TEXT NOT NULL,
  prepared_run_revision INTEGER NOT NULL CHECK (prepared_run_revision >= 1),
  prepared_dependency_revision INTEGER NOT NULL
    CHECK (prepared_dependency_revision >= 1),
  authority_ref TEXT NOT NULL,
  authority_revision INTEGER NOT NULL CHECK (authority_revision >= 1),
  git_allowlist_epoch INTEGER NOT NULL CHECK (git_allowlist_epoch >= 1),
  git_allowlist_digest TEXT,
  grant_id TEXT,
  grant_revision INTEGER,
  draft_id TEXT,
  draft_revision INTEGER CHECK (draft_revision IS NULL OR draft_revision >= 1),
  gate_id TEXT,
  gate_revision INTEGER,
  repository_root TEXT NOT NULL,
  worktree_path TEXT NOT NULL,
  repository_state_digest TEXT NOT NULL,
  execution_profile_id TEXT NOT NULL,
  execution_profile_revision INTEGER NOT NULL
    CHECK (execution_profile_revision >= 1),
  execution_profile_digest TEXT NOT NULL,
  remote_registration_id TEXT,
  remote_registration_revision INTEGER,
  remote_generation INTEGER,
  remote_target_digest TEXT,
  operation_id TEXT NOT NULL,
  operation_variant TEXT NOT NULL,
  effect_binding_digest TEXT NOT NULL,
  result_recipe_digest TEXT NOT NULL,
  decision_digest TEXT NOT NULL,
  before_git_state_json TEXT NOT NULL,
  expected_terminal_state_json TEXT NOT NULL,
  state TEXT NOT NULL CHECK (state IN
    ('prepared','dispatching','conflict','conflict-transferred','ambiguous',
     'quarantined','applied','no-effect','rejected','failed',
     'human-resolved')),
  state_revision INTEGER NOT NULL CHECK (state_revision >= 1),
  terminal_basis TEXT CHECK (terminal_basis IS NULL OR terminal_basis IN
    ('machine-proof','conflict-transfer','human-adjudication')),
  predecessor_custody_id TEXT,
  predecessor_conflict_generation INTEGER CHECK (
    predecessor_conflict_generation IS NULL OR
    predecessor_conflict_generation >= 1),
  owned_conflict_generation INTEGER CHECK (
    owned_conflict_generation IS NULL OR owned_conflict_generation >= 1),
  mutation_reservation_generation INTEGER NOT NULL
    CHECK (mutation_reservation_generation >= 1),
  lock_plan_digest TEXT NOT NULL,
  lookup_generation INTEGER NOT NULL CHECK (lookup_generation >= 0),
  lookup_evidence_digest TEXT,
  lookup_outcome TEXT CHECK (lookup_outcome IS NULL OR lookup_outcome IN
    ('exact-conflict','exact-applied','exact-no-effect','incomplete',
     'unavailable','inconsistent','inspector-unavailable',
     'remote-proof-permanently-unavailable','mixed-local-remote-evidence',
     'evidence-integrity-failure','conflict-state-unverifiable')),
  lookup_failure_signature_digest TEXT,
  lookup_observed_at INTEGER,
  resolution_eligible INTEGER NOT NULL CHECK (resolution_eligible IN (0,1)),
  resolution_eligible_lookup_generation INTEGER,
  resolution_eligible_evidence_digest TEXT,
  resolution_eligibility_reason TEXT CHECK (
    resolution_eligibility_reason IS NULL OR resolution_eligibility_reason IN
      ('inspector-unavailable','remote-proof-permanently-unavailable',
       'mixed-local-remote-evidence','evidence-integrity-failure',
       'conflict-state-unverifiable')),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY (project_session_id, coordination_run_id)
    REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY (project_session_id, coordination_run_id,
               authority_revision, authority_ref)
    REFERENCES run_authority_revisions(
      project_session_id, coordination_run_id, authority_revision,
      authority_ref),
  FOREIGN KEY (grant_id, grant_revision)
    REFERENCES operator_git_grants(grant_id, revision),
  FOREIGN KEY (draft_id, operation_id)
    REFERENCES git_operation_drafts(draft_id, operation_id),
  FOREIGN KEY (gate_id, operation_id)
    REFERENCES scoped_gate_operations(gate_id, operation_id),
  FOREIGN KEY (operation_id) REFERENCES operation_admissions(operation_id),
  FOREIGN KEY (execution_profile_id, execution_profile_revision)
    REFERENCES git_execution_profiles(profile_id, revision),
  FOREIGN KEY (remote_registration_id, remote_registration_revision)
    REFERENCES git_remote_registrations(registration_id, revision),
  FOREIGN KEY (predecessor_custody_id)
    REFERENCES operator_git_effect_bindings(custody_id),
  FOREIGN KEY (custody_id, mutation_reservation_generation)
    REFERENCES git_mutation_reservations(custody_id, generation),
  CHECK ((grant_id IS NULL)=(grant_revision IS NULL)),
  CHECK ((draft_id IS NULL)=(draft_revision IS NULL)),
  CHECK ((gate_id IS NULL)=(gate_revision IS NULL)),
  CHECK ((draft_id IS NULL)=(gate_id IS NULL)),
  CHECK ((grant_id IS NULL)<>(gate_id IS NULL)),
  CHECK ((remote_registration_id IS NULL)=
         (remote_registration_revision IS NULL)),
  CHECK ((remote_registration_id IS NULL)=(remote_generation IS NULL)),
  CHECK ((remote_registration_id IS NULL)=(remote_target_digest IS NULL)),
  CHECK ((predecessor_custody_id IS NULL)=
         (predecessor_conflict_generation IS NULL)),
  CHECK ((lookup_generation=0)=(lookup_evidence_digest IS NULL)),
  CHECK ((lookup_generation=0)=(lookup_outcome IS NULL)),
  CHECK ((lookup_generation=0)=(lookup_observed_at IS NULL)),
  CHECK (lookup_evidence_digest IS NULL OR
         (length(lookup_evidence_digest)=71 AND
          substr(lookup_evidence_digest,1,7)='sha256:')),
  CHECK (lookup_failure_signature_digest IS NULL OR
         (length(lookup_failure_signature_digest)=71 AND
          substr(lookup_failure_signature_digest,1,7)='sha256:')),
  CHECK (
    (lookup_outcome IN
      ('incomplete','unavailable','inconsistent','inspector-unavailable',
       'remote-proof-permanently-unavailable','mixed-local-remote-evidence',
       'evidence-integrity-failure') AND
     lookup_failure_signature_digest IS NOT NULL) OR
    ((lookup_outcome IS NULL OR lookup_outcome NOT IN
      ('incomplete','unavailable','inconsistent','inspector-unavailable',
       'remote-proof-permanently-unavailable','mixed-local-remote-evidence',
       'evidence-integrity-failure')) AND
     lookup_failure_signature_digest IS NULL)),
  CHECK ((resolution_eligible=0)=
         (resolution_eligible_lookup_generation IS NULL)),
  CHECK ((resolution_eligible=0)=
         (resolution_eligible_evidence_digest IS NULL)),
  CHECK ((resolution_eligible=0)=
         (resolution_eligibility_reason IS NULL)),
  CHECK (resolution_eligible=0 OR
         resolution_eligible_lookup_generation=lookup_generation),
  CHECK (resolution_eligible=0 OR
         resolution_eligible_evidence_digest=lookup_evidence_digest),
  CHECK (resolution_eligible=0 OR
         resolution_eligibility_reason=lookup_outcome),
  CHECK (resolution_eligibility_reason<>'conflict-state-unverifiable' OR
         (state='quarantined' AND
          (owned_conflict_generation IS NOT NULL OR
           predecessor_conflict_generation IS NOT NULL)))
);
```

The binding's `prepared_session_revision`, `prepared_run_revision` and `prepared_dependency_revision` are the final
action's compare-and-set snapshot, not grant-lifetime fields. They may differ from the grant's issuing provenance; the
grant remains valid if its live fences still match.

The migration transactionally widens the canonical `operator_effect_custody.state` check with `conflict` and
`quarantined`, and widens `operation_admissions.state` with `conflict`, `ambiguous` and `quarantined`. These are
refinements of the existing owners, not parallel journals. Generic-owner triggers reject those new states unless the
exact Git binding/admission/reservation join exists. Git lifecycle triggers permit only the following row combinations
and update every named row in one transaction:

| Event | Git binding | Generic custody | Target admission | Reservation | Liveness |
| --- | --- | --- | --- | --- | --- |
| unconsumed gate draft | absent | absent | `prepared` | absent | no |
| final Commit prepared | `prepared` | `prepared` | `authorised` | `reserved` | yes |
| dispatch begins | `dispatching` | `dispatching` | `executing` | `dispatching` | yes |
| exact bounded conflict | `conflict` | `conflict` | `conflict` | `conflict` | yes |
| persisted owned/inherited conflict proved destroyed/altered | `quarantined` | `quarantined` | `quarantined` | `quarantined` | yes |
| outcome unknown | `ambiguous` | `ambiguous` | `ambiguous` | `ambiguous` | yes |
| machine proof permanently unavailable | `quarantined` | `quarantined` | `quarantined` | `quarantined` | yes |
| machine-proved applied | `applied` | `terminal` | `terminal` | `released` | no |
| machine-proved no effect | `no-effect` | `no-effect` | `terminal` | `released` | no |
| post-prepare proved reject/failure | `rejected`/`failed` | same | `cancelled` | `released` | no |
| conflict handed to typed successor | `conflict-transferred` | `terminal` | `terminal` | `released`; successor is `reserved` | successor only |
| human custody adjudication | `human-resolved` | `terminal` | `terminal` | `released` or `retired` | no |

An `applied`/`no-effect` terminal basis is `machine-proof`; a transferred conflict is `conflict-transfer`;
`human-resolved` is `human-adjudication`. Triggers reject a terminal basis inconsistent with the row combination, a
conflict without one positive `owned_conflict_generation`, an eligible-resolution marker outside
`ambiguous`/`quarantined`, or a marker whose generation/evidence does not equal the latest lookup. There is exactly one
current conflict owner and one active reservation for a Git common directory.

An explicit authenticated conflict reconciliation binds the exact custody, lineage generation, binding state revision,
common-directory identity and prior evidence. The Git-only `owned-conflict` form requires outer/binding state
`conflict`, positive owned generation and exact nullable predecessor generation. The Git-only `inherited-successor` form
requires positive predecessor generation, null owned generation and one exact mapping of outer status to binding state:
`pending -> prepared`, `ambiguous -> ambiguous` or `quarantined -> quarantined`. Both forms name the original target
command and compare-and-set reservation generation, lookup generation and nullable prior evidence digest, and require
the current resolution-eligibility discriminator to be `none`. The original target's project/session are
reauthenticated, but the discriminator requires the distinct `git-custody-resolve` capability; the original `git`
capability alone is insufficient. Missing/stale/crossed lineage fields, an existing eligibility marker or use by another
action family changes nothing. The inspector uses only the sealed no-process typed local reader. Complete proof that
native operation state, index stages or the bounded conflict-path manifest no longer equals the persisted conflict
atomically increments lookup and binding-state revisions, records the complete evidence digest/outcome/time, moves the
Git binding, generic custody, admission and reservation from `conflict` to `quarantined`, and sets the matching
generation-bound `resolution_eligible` marker with reason `conflict-state-unverifiable`. The reservation and liveness
blocker remain. Exact intact proof retains `conflict`; incomplete, unavailable or internally inconsistent observation
also retains it and creates no eligibility marker; each accepted inspection still appends the next bounded lookup
evidence/outcome/time so a later request can compare-and-set it. This transition invokes no Git process, remote call or
mutation and cannot continue, abort or restore the native operation.

For an inherited successor, exact intact proof atomically changes `prepared|ambiguous|quarantined` to `conflict`,
assigns the next positive owned generation and retains the reservation. Complete proof that the inherited native
state/index/path manifest no longer holds atomically changes or retains all four owners as `quarantined`, keeps owned
generation null, retains the positive predecessor generation and reservation, and sets the matching
`conflict-state-unverifiable` eligibility tuple. Incomplete, unavailable or inconsistent proof moves `prepared` to
`ambiguous` or retains the existing ambiguity/quarantine without eligibility. No branch releases the transferred
reservation or rewrites predecessor machine evidence.

The same owned/inherited all-four-owner quarantine mapping is used for a closed permanent `inspector-unavailable` or
`evidence-integrity-failure` outcome, with that exact outcome as the eligibility reason. Immediate permanent
classification is permitted only when the digest-pinned reader or trusted execution-profile contract is absent/revoked
for the target generation, or the sealed reader can read the bounded canonical files but proves their format/hash
relationship cannot yield any complete observation. Otherwise `unavailable` and `inconsistent` are transient. They
become permanent only on the third consecutive accepted lookup for the same custody/lineage and identical normalised
failure-signature digest, under distinct reconciliation commands spanning at least 60 seconds with no intervening
different outcome/signature. The immutable reconciliation command results are the streak owner; the current binding
mirrors only the latest signature digest. Project files, operator text and the caller cannot select a permanent code.
Attempts one and two retain the existing conflict or ambiguity/quarantine blocker without eligibility. The third
final-CAS transaction persists the permanent code/evidence/time, moves or retains every owner in `quarantined`, retains
the reservation and sets the exact latest-generation eligibility tuple. Any different outcome/signature resets the
derived streak.

The public reconciliation codec is the exhaustive three-variant union in the Agent Fabric contract: the generic `pending|ambiguous` form
rejects `git_conflict`; the `owned-conflict` form requires `conflict -> conflict`; and the `inherited-successor` form
requires one of the three exact outer/binding-state mappings above. Each Git form requires every custody, lineage,
binding-state, reservation, common-directory and lookup compare-and-set field and rejects extras, including any target
whose resolution eligibility is not `none`. Their required operator action is `git-custody-resolve`. The daemon first
journals the reconciliation command as observe-only/in-flight, projected as `pending/observing` at exactly the next
attempt generation, then performs the bounded read. Its final transaction rechecks every requested field, increments
lookup and binding-state revisions, persists the closed outcome/evidence/time, fixes the target at that same next
attempt generation, applies either the all-four-owner retained-conflict or all-four-owner quarantine mapping, and stores
the closed resulting `OperatorActionStatus` snapshot in the same command row. A stale final recheck atomically changes
no custody/admission/reservation row and instead stores a terminal `rejected` result with `state-changed` for a custody
tuple mismatch, `generation-stale` for principal/session generation mismatch or `authority-insufficient` for
expired/revoked/insufficient capability, plus the target intent digest and original bounded evidence references in that
reconciliation command row. Crash before either final branch leaves no custody transition; exact retry may repeat only
the read-only inspection. Once a status snapshot or stale rejection commits, exact replay performs no inspection, status
returns it immutably and changed replay under that command ID conflicts. A later inspection uses a new command over the
latest target tuple.

`fabric.v1.operator-action.status` projects Git custody in `pending/prepared`, `ambiguous`, `conflict` and `quarantined`
only through the closed `git_custody` status union in the Agent Fabric contract. A target-command query reads the current
binding/generic-custody/admission/reservation join and rejects an impossible combination. Only an inherited successor
with binding `prepared`, positive predecessor lineage, null owned generation and no eligibility may use the `pending`
status with `phase=prepared`; generic pending effects retain the generic status without `git_custody`. A
reconciliation-command query returns `pending/observing` while its bounded read has no terminal command result, then
returns the immutable stored status snapshot or closed stale rejection; it never parses either as an operator-action
receipt. Status requires `read`, has no inspection side effect and exposes no repository path, Git output, credential or
command vector.

Confirmed Commit of `merge-continue`, `merge-abort`, `rebase-continue` or `rebase-abort` revalidates the current
conflict owner and generation. In one transaction it terminalises that predecessor as `conflict-transferred`, releases
its reservation and creates the successor as `prepared`/`authorised`/`reserved` over the exact conflict before-state. If
the successor never dispatches, or dispatch proves the identical conflict remains with no other effect, recovery uses
the sealed no-process typed local reader to move the successor to `conflict` and make it the new owner instead of
releasing the repository. If exact unchanged-conflict proof is unavailable it becomes ambiguous/quarantined without
eligibility and defers any destroyed/altered classification to the explicit `inherited-successor` observe path above.
Exact resolution releases the reservation; another exact conflict records a higher owned generation;
ambiguity/quarantine follows the table. An old owner/generation, concurrent successor or partial transfer fails
atomically.

Draft rows never contribute liveness. Prepared through quarantined Git custody does; conflict blocks project-session
acceptance without forcing an unrelated session state, ambiguity contributes `recovery_required`, and quarantine uses
the existing `quarantined` session/run state. Terminal rows do not contribute. Restart joins all four owners and fails
closed on any impossible combination; it never repairs one row by guessing from another. Human resolution removes the
custody blocker only through the transaction defined below and does not silently advance the session lifecycle.

Human adjudication has one immutable owner:

```sql
CREATE TABLE git_custody_resolutions (
  resolution_id TEXT PRIMARY KEY,
  draft_id TEXT NOT NULL UNIQUE REFERENCES git_operation_drafts(draft_id),
  resolution_operation_id TEXT NOT NULL UNIQUE
    REFERENCES operation_admissions(operation_id),
  target_custody_id TEXT NOT NULL UNIQUE
    REFERENCES operator_git_effect_bindings(custody_id),
  target_operation_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  coordination_run_id TEXT NOT NULL,
  expected_lookup_generation INTEGER NOT NULL,
  lookup_evidence_digest TEXT NOT NULL,
  eligibility_reason TEXT NOT NULL CHECK (eligibility_reason IN
    ('inspector-unavailable','remote-proof-permanently-unavailable',
     'mixed-local-remote-evidence','evidence-integrity-failure',
     'conflict-state-unverifiable')),
  adjudication TEXT NOT NULL CHECK (adjudication IN
    ('applied','no-effect','quarantine-accepted')),
  reason TEXT NOT NULL,
  gate_id TEXT NOT NULL,
  gate_revision INTEGER NOT NULL,
  resolved_by_operator_id TEXT NOT NULL,
  operator_input_record_digest TEXT NOT NULL,
  reservation_disposition TEXT NOT NULL CHECK (reservation_disposition IN
    ('released','retired')),
  resolution_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  FOREIGN KEY (project_session_id, coordination_run_id)
    REFERENCES runs(project_session_id, run_id),
  FOREIGN KEY (target_operation_id)
    REFERENCES operation_admissions(operation_id),
  FOREIGN KEY (gate_id, resolution_operation_id)
    REFERENCES scoped_gate_operations(gate_id, operation_id),
  CHECK ((adjudication='quarantine-accepted')=
         (reservation_disposition='retired'))
);
```

The insert trigger joins the target binding and its generic custody, target admission and active reservation; requires
one identical project/session/run, target operation ID, `ambiguous`/`quarantined` state and exact current eligible
lookup generation/evidence; and validates the consumed `custody-resolution` draft, exact approved human-required gate
and independently attested operator-input record. The reason is bounded and non-empty. The resolution digest covers all
immutable fields and the original machine evidence, excluding only itself.

Confirmed Commit inserts the resolution, advances the target binding to `human-resolved` with `human-adjudication`
basis, target generic custody and target admission to `terminal`, and target reservation to `released` for
`applied`/`no-effect` or `retired` for `quarantine-accepted`. It also consumes the draft, advances the resolution
admission through `authorised` to `terminal`, copies the eligibility reason into the immutable resolution and clears the
target's eligibility marker/three eligibility fields. It then commits the operator command/receipt. All statements are
one transaction; a crash exposes all or none. No update changes the target's lookup evidence, machine outcome or
expected terminal state. The receipt and projections always prefix the selected result with `human-adjudicated-`.

Resolution performs zero Git/remote/process/filesystem calls. Exact replay is read-only and returns the immutable row. A
second resolution, stale lookup, changed evidence, ineligible state, reused/mismatched gate, policy resolver, ordinary
`git`/`decide` capability or direct SQL partial write fails with no reservation or liveness change.
`quarantine-accepted` satisfies only the existing explicit abandonment with reason closure predicate; it never proves
the effect outcome or closes/promotes the session by itself.

The implementation shall install the exhaustive the Agent Fabric contract operation-variant enumeration and stricter
same-project/session/run/profile/remote triggers; they are not optional implementation detail. Binding, before-state,
result-recipe and expected-terminal columns are immutable after insert. Only the closed Git/
generic/admission/reservation lifecycle fields, owned conflict generation, bounded outcome, lookup/eligibility fields
and timestamps may advance through the mapped compare-and-set transitions. The custody ID is daemon-derived from
operator, project, session and command identity. Exact replay returns the same record; changed intent, decision or
binding conflicts.

The preauthorised final Commit inserts one exact `authorised` `operation_admissions` row with generic custody and the
Git binding. Its operation ID derives from operator, project session, stable Preview ID and effect-binding digest. A
later Preview of identical Git state receives a different ID. A gate-only mutation or custody resolution instead uses
the immutable operation ID/payload digest already inserted as a `prepared` admission with `git_operation_drafts`; final
Commit compare-and-sets draft `gate-bound -> consumed` and admission `prepared -> authorised` while creating the
mutation custody rows or human-resolution row. `operation_kind` is the closed mutation variant or `git-custody-resolve`;
`payload_digest` equals the immutable draft binding digest.

The gate path requires the persisted exact `scoped_gate_operations(gate_id, operation_id)` row and operation-draft
identity. A preparation trigger rechecks draft state/revision/digest/expiry, its observed session/run/dependency
revisions and session generation, the gate's exact session/run/revision, `approved` status, human-required flag and
resolver, dependency revision, operation enforcement point and blocked- operation membership. The preauthorised variant
has no draft or gate association but still owns one admission. No draft association is inferred from operation kind or
payload similarity.

The current baseline installs an `operation_gate_block` trigger that joins on `NEW.operation_id`, never
`NEW.operation_kind`, and rejects any admission/gate/binding mismatch before custody becomes prepared. Direct SQL and
public-protocol paths use the same triggers. Missing grant/draft/gate, admission, binding or required resolution row
rolls back the operator command and every owned row.

Preparation first performs all capability, the applicable positive allow-list/ grant or draft/gate checks,
issuance-provenance integrity, current action CAS, repository, worktree, writer-admission, execution-profile,
remote-target and typed Git-state checks inside the custody transaction. It persists canonical before state, the exact
bounded result recipe and the states that would prove applied, conflict or no effect. It also creates one
generation-bearing `git_mutation_reservations` row for the canonical Git common directory. At most one active Fabric Git
mutation reservation may exist per common directory; it coordinates all project worktrees but is not claimed as a
physical fence against an external process.

After the prepared transaction commits, the single Git owner follows this lock-and-dispatch protocol:

1. acquire the private daemon reservation lock and the operation-specific Git/filesystem locks named by immutable
   `lock_plan_digest`, without changing an index, ref, config, worktree or remote;
2. open and bind every affected path with no-follow handles where content is an input, and re-observe repository common
   directory, worktree identity, HEAD, index, worktree bytes, refs, worktree registry, config, execution profile and
   registered remote target while those locks are held;
3. in one SQLite transaction recheck global/session/run/dependency/authority, grant/gate/admission, profile/remote and
   reservation generations, require the observation to equal custody, then CAS generic custody `prepared -> dispatching`
   and operation admission `authorised -> executing`;
4. begin the first mutation immediately while retaining the native locks or an exact native compare-and-set over every
   affected state; and
5. release native locks only after terminal/conflict/ambiguous custody and the operation admission have committed, or
   after proved pre-dispatch cleanup; conflict/ambiguity retains its persisted common-directory reservation.

The operation-specific minimum is:

- index effects use the native index-lock convention, build from pinned exact bytes in a private index, verify the
  original index digest and atomically install it; a changed path cannot be re-read under the old Preview;
- commit, branch, merge and rebase ref changes use Git reference transactions or `update-ref` old-object
  compare-and-set; commit-producing objects follow the authorised result recipe before any ref becomes visible;
- merge/rebase additionally require substrate-enforced exclusive access to the admitted worktree, the index lock and
  exact path manifest; if the substrate cannot prevent an overlapping admitted writer, the variant is unavailable;
- worktree create/move/remove use exclusive destination creation or no-replace rename, exact registry/inode/digest
  checks and the common-directory reservation; force removal never bypasses a locked worktree;
- upstream set/unset uses the local-config lock, exact prior config digest and atomic replace of only the two typed
  branch keys; and
- remote effects use the exact registered remote-port target and its native fast-forward/lease CAS where applicable. A
  remote without the required compare-and-set or inspection contract makes that variant unavailable.

An injected change after initial observation, after lock acquisition, during the final SQLite recheck or immediately
before the first CAS must fail before an authority-visible mutation. Active writer admission or later quarantine alone
is insufficient. A crash while still `prepared` may remove only a proved-owned stale lock/reservation and records no
effect; it cannot clean an unowned Git lock.

The bound execution profile constructs a minimal environment: all caller `GIT_*`, editor, pager, signing, askpass, SSH
and helper variables are removed; system/global configuration and includes are disabled; the repository config is never
consulted for executable selection; hooks resolve to a sealed empty directory; submodule recursion is off; and project
attributes selecting clean/process filters, custom merge/diff drivers or working-tree transforms are rejected for
affected paths. The port uses raw/plumbing operations and the pinned built-in backend. Remote access goes only through
the registered remote port; any allowed absolute helper is digest-checked, receives fixed arguments and runs in its
declared sandbox. No shell, alias, arbitrary command/option, remote URL or interactive prompt is accepted. Bounded
stdin, stdout, stderr, deadline and child cleanup apply to every operation. Credentials are neither persisted nor
returned.

Each operation produces a closed result tied to the persisted `git_result_recipe_v1` digest and execution-profile
digest: command exit class, bounded output digests, exact recipe branch and one fresh typed repository, configuration
and, when applicable, registered-target observation. Terminal success is committed only when the observation matches the
recipe's exact object IDs, ref/config transitions, commit-parent/tree/identity/timestamp/ message fields,
source-to-new-commit mapping, index/worktree manifest and remote result. The persisted recipe bounds of 64 ref/config
updates, 128 commit mappings and 4096 conflict paths are hard admission limits, not truncation limits.

An exit failure is `no-effect` only when the inspector proves every affected local and remote ref, object, index,
worktree, registry entry and config key remains at the persisted before state. Otherwise it is `ambiguous` or
`quarantined`. A merge, pull-merge, rebase or pull-rebase start may record only the recipe's exact bounded `conflict`
state, including native operation state, index stages, conflict paths and generation. It never continues or aborts in
that custody. `merge-continue`, `merge-abort`, `rebase-continue` and `rebase-abort` are new previewed, gate-authorised
operations that bind the predecessor custody and conflict generation, re-observe the complete conflict state and use a
new recipe, admission and reservation generation. A start gate or admission cannot authorise any successor. Partial
pull, unknown hook-like external behaviour, remote observation failure or any mismatch between process result and
repository state is never silently normalised to success.

Startup and live reconciliation use this closed policy:

- `prepared`: perform zero Git process or remote calls; after proving dispatch never began, remove only a proved-owned
  daemon-private reservation artifact, release its persisted reservation and record `no-effect` once; never remove a Git
  lock or project file. A prepared typed conflict successor instead uses the sealed no-process typed local reader to
  prove the inherited conflict unchanged and atomically becomes the current `conflict` owner/reservation without
  executing its requested action; unavailable/mismatched proof retains an ambiguity/quarantine blocker without
  eligibility rather than releasing it, and only an explicit `git-custody-resolve`-capable `inherited-successor`
  observation may classify complete destroyed/altered proof;
- `dispatching` or `ambiguous`: call only the fixed read-only Git effect inspector for the exact custody ID and
  increment `lookup_generation`; never execute, abort, continue, retry or reconstruct the mutation;
- `quarantined`: perform no automatic repeated lookup. An explicit bounded reconciliation request may call only that
  same inspector and either append a higher lookup generation with machine proof or retain quarantine; it never mutates
  Git;
- `conflict`: startup and passive supervision perform no lookup. An explicit authenticated request bound to the exact
  custody, owned conflict generation, state revision, reservation generation, common-directory identity, lookup
  generation and nullable prior evidence may call only the sealed no-process typed local reader. Every accepted
  observation appends exactly one lookup generation with bounded evidence, outcome and time under a final
  compare-and-set. Exact bounded conflict proof retains the predecessor and reservation for a separately previewed typed
  continue/abort decision. Complete proof that the persisted native state, index stages or conflict-path manifest was
  destroyed or altered out of band atomically moves all four owners to `quarantined`, retains the reservation, and sets
  only the matching `conflict-state-unverifiable` eligibility marker. Incomplete, unavailable or inconsistent proof
  retains `conflict` without eligibility while transient; a closed permanent inspector/integrity outcome moves all four
  owners to `quarantined` with matching eligibility. The inspection never invokes a Git process, remote call, continue,
  abort, restore or other mutation;
- inherited typed successor in `prepared|ambiguous|quarantined`: an explicit `git-custody-resolve`-capable request bound
  to the exact predecessor generation and null owned generation uses the same one-lookup/final-CAS protocol. Exact
  intact proof assigns the next owned conflict generation and moves every owner to `conflict`; complete
  destroyed/altered proof retains the transferred reservation and moves/retains every owner in `quarantined` with
  matching `conflict-state-unverifiable` eligibility; incomplete, unavailable or inconsistent proof retains an
  ambiguity/quarantine blocker without eligibility while transient, while a closed permanent inspector/integrity outcome
  uses the same retained-reservation quarantine/eligibility mapping. It performs no Git process, remote call or
  mutation;
- exact applied proof: record terminal success and the after-state digest once;
- exact no-effect proof: record terminal no-effect once;
- incomplete, unavailable, conflicting or mixed local/remote evidence: retain `ambiguous` or enter `quarantined` with
  its evidence digest. Only a closed permanent-unprovability reason may set the exact generation-bound
  `resolution_eligible` marker; and
- terminal, no-effect, rejected or failed-with-proof: return the stored outcome on replay with zero Git I/O.
  `human-resolved` returns its separately labelled immutable adjudication and never re-runs lookup.

Lookup of fetch, pull or push opens only the custody's exact remote registration ID/revision/generation/target digest
and exact refs. A same-name registration with a changed target is a mismatch, not a lookup route. Remote absence,
timeout or changed/unreadable advertised state cannot prove no effect. Lookup of commit, branch and worktree actions
resolves exact objects and all registered worktrees. Upstream lookup reads only the two typed local branch keys under
the persisted config digest and target binding. Merge and rebase lookup records their native operation state but never
invokes automatic `--abort` or `--continue`. An unresolved binding is a project-session membership/closure blocker; the
affected session remains in an existing liveness-contributing ambiguity or quarantine state. Pending Git custody is not
discarded merely to permit daemon idle stop.

Migration preflight shall reject malformed/non-canonical paths, unknown effect, variant, recipe or state values, invalid
digests, missing run authority history, missing execution-profile or target-bound remote records, non-contiguous grant
revisions, two active grant revisions, missing normalised constraint children, widened/gate-only constraints, malformed
or impossible issuance provenance, cross-project/session/run references and an existing generic Git custody row without
a complete typed binding. A valid historical issuing revision lower than the current orchestration revision is not
stale. Preflight also rejects an impossible draft/admission/gate state, a non-exact gate-operation association, any
mismatch in the four-owner Git state table, duplicate active common- directory reservations, a partial/duplicate human
resolution and unowned/ native Git locks that prevent a safe initial observation. It shall not infer a grant, draft,
target, profile, admission, resolution or human gate for historical data.

Authority/allow-list history, profile, remote, normalised grant-child, draft, reservation, binding, custody-resolution,
operation-admission and exact `(gate_id, operation_id)` foreign keys, indexes and triggers install in one transaction in
the fresh baseline. The operation-ID join is the only `operation_gate_block` trigger. Binding/reservation immutability,
mapped state-transition, digest, positive-containment, live- authority, same-run and global-revision triggers become
live before the schema version advances. Recovery is forward repair or verified restore under the recovery contract.

Acceptance additionally requires deterministic oracles for:

- migration preflight/rollback, every composite foreign key, immutable draft/binding/resolution content, contiguous
  grant revisions, one-active- revision, exclusive grant/gate choice, every four-owner state combination, one active
  reservation per common directory and indexed current-grant/ draft/recovery queries;
- grant issue/revise/revoke requiring `git-authorise`, a Preview-bound direct-human decision and positive subset of
  every `git_allowlist_v1` dimension; missing/stale parents, wildcard/negative-only rows, widened
  variants/refs/paths/remotes/profile, gate-only variants, reused human provenance and an ordinary `git` caller all fail
  before insert/update;
- absent, expired, revoked/non-active, tampered/nonexistent issuance provenance, wrong project/session/generation,
  authority/allow-list/profile/remote fence, repository/worktree or constraint-mismatched grants causing zero Git
  process I/O, including a sibling operation variant or same remote name with another target;
- ordinary session/run/dependency and repository HEAD/ref/index/content revision advancement after issuance leaving the
  grant valid, while a stale action Preview fails its own CAS and a fresh Preview reuses that grant.
  Authority/allow-list rotation, session-generation, canonical repository/ worktree identity, profile or remote-target
  change at Preview, prepare, lock, final recheck or first mutation invalidates it without rewriting issuance
  provenance;
- exact draft replay returning one prepared admission/operation ID and no custody/reservation/liveness; changed payload,
  duplicate ID, early effect row, expiry/cancel/stale reopen and gate association by operation kind all fail;
- final gate Preview making no write, while confirmed Commit alone atomically consumes the exact draft, authorises its
  admission and creates custody/ reservation. Direct-SQL negatives vary draft/gate operation ID, revision, digest,
  resolver, run, dependency revision, enforcement point and blocked effect; the removed operation-kind trigger accepts
  no same-kind substitute;
- one real temporary-repository operation for every the Agent Fabric contract operation variant, including upstream set/unset and typed
  merge/rebase continue/abort, with fixed profile/backend, bounded I/O and a receipt matching a fresh typed read;
- hostile local/system/global config, includes, hooks, attributes, filters, merge/diff drivers, aliases,
  credential/remote helpers, SSH/editor/pager/ signing/askpass variables, prompts and submodule recursion canaries
  proving rejection or the sealed trusted path before any project-selected executable;
- byte-identical merge, pull-merge, rebase and pull-rebase recipe output for one pinned profile across wall-clock,
  locale and caller-config changes, plus exact parent/tree/identity/timestamp/message, source-to-new mapping, conflict
  manifest and hard-bound checks; a Git binary/version/digest change invalidates the old Preview, and an unpinned
  backend is unavailable;
- exact start-conflict proof followed by separately drafted/gated typed continue and abort, with predecessor
  custody/generation, atomic terminal/ successor transfer and one-reservation checks. Crash before successor dispatch
  makes the successor the conflict owner without Git I/O; old-owner, concurrent-successor, automatic recovery and start
  gate/admission reuse fail;
- an out-of-band abort, manual resolution or conflict-state edit makes the persisted predecessor conflict, or the
  inherited conflict after successor Commit but before successor dispatch, fail a complete sealed-reader comparison.
  Explicit reconciliation atomically quarantines every owner, retains the reservation and records
  `conflict-state-unverifiable`; incomplete evidence and the first two identical transient unavailable/inconsistent
  signatures advance only the bounded lookup audit and retain the blocker. A missing/ revoked pinned inspector, proved
  canonical-evidence integrity failure or the third identical failure signature under the bounded time/command rule
  quarantines every owner with matching `inspector-unavailable` or `evidence-integrity-failure` eligibility. A different
  signature resets the streak. Closed codec negatives reject missing/extra/cross-variant fields, nullable-evidence
  mismatch, stale binding/conflict/reservation/lookup generations and `git` without `git-custody-resolve`. Target and
  reconciliation command status queries distinguish target `pending/prepared`, `ambiguous`, `conflict` and
  `quarantined`, plus reconciliation-command `pending/observing`, for both owned and inherited-successor lineage; exact
  replay returns the immutable snapshot with zero inspection, changed replay conflicts, and a crash before final CAS may
  repeat only the read-only inspection. A race that transfers custody or completes another lookup between inspection and
  final CAS stores one terminal closed rejection command with zero owner update; exact replay performs no second
  inspection and a new command must bind the latest tuple. A separately drafted/gated custody adjudication then releases
  or retires exactly that reservation with zero Git/remote/ process/filesystem mutation, while an unchanged conflict
  still permits only typed continue/abort;
- fault injection before binding/reservation insert, after generic custody, after prepare commit, after private lock,
  after each native lock, before the SQLite CAS and immediately before the first mutation/CAS for index, ref,
  merge/rebase worktree, worktree registry, config/upstream and remote families; every injected competing change fails
  with zero authority-visible mutation;
- restart of `prepared` making no Git/remote call and cleaning only its owned private artifact (or retaining an
  inherited conflict), restart of `dispatching`/`ambiguous` making exactly one bounded lookup and no mutation,
  quarantine making no automatic lookup, retained conflict making no automatic action, and exact machine/human terminal
  replay making no call;
- commit/branch/worktree/upstream local proof, target-bound fetch/pull/push remote proof, same-name target retarget
  invalidation, merge/rebase conflict, partial pull and unavailable remote observation remaining honest;
- unresolved Git custody blocking project-session closure and surviving daemon and Console restart without a duplicate
  effect, followed by `git-custody-resolve` negatives for ineligible or intact-conflict custody, stale lookup/evidence,
  wrong gate/capability/human provenance and changed replay. Faults before/after every resolution statement leave all
  rows unchanged or one immutable human-labelled result, make zero Git/remote/process call, preserve machine evidence,
  release/retire one reservation and remove only its closure blocker; and
- capability, remote-credential, command, output and receipt canaries proving that no secret, arbitrary argument or
  unbounded process output reaches persistence, projection, logs or Console rendering.

## Scoped enforcement, authority, and topology

## Scoped-operation enforcement, operator-effect custody and Herdr seam

The additive persistence change for operation enforcement shall bind each gate-operation predicate to the gate's exact
project session and coordination run. The public check supplies the operation-target contract's `operationTarget` and current
dependency revision. For `{kind: task}`, the transaction proves the task belongs to that run and joins the operation
kind to the gate's current `scoped_gate_tasks` row at the same bound dependency revision. For `{kind: run}`,
task/subtree gates never match. Run/release gates remain bounded to their exact run. Preparation triggers and service
checks use the same predicate, so a target-less call, same-kind sibling substitution, stale graph or cross-run task
cannot authorise or block an effect accidentally.

Project-session closure and global idle-stop use one exhaustive classification of `operator_effect_custody`. `prepared`,
`dispatching`, `ambiguous` and `failed` are unresolved; `terminal`, `no-effect` and `rejected` are terminal. Every
unresolved row is a closure blocker for its exact project session and a daemon-liveness contributor. Each row maps to
exactly one daemon recovery owner. Specialised launch, Git, chair/child bridge, registered external-effect and Herdr
owners exclude their rows from generic mutation while retaining the blocker. An unowned prepared row may be terminalised
as no-effect only with durable proof that dispatch never began. Every post-dispatch or failed row uses bounded
evidence-only lookup; an absent port, unknown owner, malformed outcome or incomplete evidence remains
ambiguous/quarantined and moves the owning session/run to the corresponding existing recovery state. Recovery never
replays an effect merely to permit acceptance or daemon shutdown.

The optional Herdr package composes through one daemon-owned `herdr-control-v1` action owner. The daemon prepares one
stable action for only the closed the Agent Fabric contract operation family, persists dispatch before Herdr I/O and completes it only
from a closed receipt matching the prepared intent. Restart leaves prepared actions undispatched and performs lookup
only for dispatched or ambiguous actions. Herdr presence is a separate bounded observation with `available`,
`unavailable` or `stale` freshness; loss records `visibility_degraded` without inferring provider death, task state,
delivery or completion. When the optional package is disabled or unavailable, action calls return typed unavailability
and the daemon, Console and all non-Herdr protocol paths remain fully operable.

Deterministic verification additionally covers:

- same-operation sibling tasks under task/subtree gates, exact run targets, cross-run task rejection and atomic
  dependency-revision rebinding through both public checks and preparation triggers;
- every operator-effect state in idle and closure queries, single-owner classification, typed-owner exclusion,
  unknown-owner fail-closed behaviour and crash points before prepare, before/after dispatch and before terminal
  evidence commit; and
- every closed Herdr action, disabled/unavailable portability, separate presence degradation, stable replay,
  prepared-with-zero-I/O restart, lookup-only dispatched/ambiguous recovery and negative pane-inference canaries.

## Integration-principal and direct-human attestation enforcement

The current squashed baseline creates one hash-only integration credential table. Its unique identity binds capability
ID and token hash to integration ID, project, project session, coordination run, principal generation, provider ID,
provider-session reference, closed granted-operation JSON, issue/expiry/revocation timestamps and revision.
Insert/update triggers reject unknown operations, empty grants, mutable identity fields, generation rollback, expiry
before issue and any grant outside operations whose registry principal set includes `integration`. Revocation is
monotonic. No raw credential column, compatibility backfill or project-wide wildcard exists.

The registry's exact integration set is provider-state report, provider-action reconcile, operator-intervention record,
visibility-failure record, budget usage record/reconcile, integration input-attest, resource reconcile and result-
delivery claim/provider-accept/consume. Baseline constraints require the bound project session to belong to the project
and the run to belong to that session; the principal generation is current for that exact integration/run binding. No
integration grant contains provider-action dispatch, lifecycle, topology, lease, capability, gate-resolution or
operator-control operations.

Trusted daemon composition owns provisioning; the public socket exposes no credential-issuance operation. Provisioning
is idempotent only for an identical binding and token hash, uses the `afi_` family and returns the raw value once to the
in-process/provider bridge. Authentication uses constant-shape hash lookup, expiry/revocation checks and the operation
registry to produce the existing closed integration principal/grant. Dispatch has an exhaustive integration branch for
the eleven operations above; it never falls through the agent or operator dispatcher. Every arm reloads the credential
by connection hash and rechecks expiry, revocation, grant, project/session/run/generation and the operation-specific
action/resource/budget/delivery owner before mutation. For input attestation, the branch supplies the bound provider
identity/session to `OperatorStore`, which compares them to the provider-native event before insert.

One production `DirectHumanInputEventSource` boundary accepts only a closed native event union. The successful arm has
provider-native attribution, exact provider/session/message/event identity, immutable event digest, `user` role and
exact utterance. Assistant, tool, system, echo, wrapper, terminal, pane, CLI, injected, ambiguous and unavailable arms
have no conversion to `direct-human`. Conformance invokes the actual adapter classifier with a fake native transport and
proves no direct store or self-assertion shortcut. When no eligible provider event source is configured, conversational
attestation is honestly unavailable; typed Console decision remains independent.

The operator store and gate store share one canonical digest-binding function. It parses stored references through the
public closed codecs, de-duplicates by first occurrence and appends release receipt/artifact digests. It compares
length, order and every value both when recording and resolving. Resolution also requires the operator command's
attested-provider provenance to match the same attestation ID, integration ID and generation. Any parse failure or state
change fails closed without changing gate or membership state.

Deterministic verification additionally covers:

- migration rollback/checksum/restart, immutable identity, monotonic revocation and all operation-registry/grant
  negatives;
- real Unix-socket integration negotiation and each of the eleven dispatch arms, one successful native callback flow,
  connection-hash rebinding checks and zero agent/operator fallthrough;
- wrong, missing, extra, duplicate and reordered gate/release digest vectors at record and resolve time, plus sentinel
  and explicit-operator matching;
- every ineligible native-event arm, message/event replay, wrong provider/
  provider-session/project/project-session/run/generation, expired/revoked or insufficient grant, every non-integration
  operation and changed gate/ provenance with zero mutation; and
- public-tree, SQLite, logs, errors, receipts, projection and rendering scans proving no `afi_` bearer fragment
  survives.

## Final acceptance and chair-membership reconciliation

Accepted project-session close recomputes the project-session acceptance contract's canonical final-acceptance reference from
`scoped_gates`; caller-supplied digest syntax is never authority. The canonical sorted set contains exactly one matching
row for each run currently awaiting acceptance and no historical terminal run. Each row must belong to the exact
session/run, be human-required, approved, run-scoped, operation-enforced for `fabric.v1.project-session.close`, resolve
to the current operator under the expected-approver rule and carry one persisted explicit-confirmation arm. Lookup and
validation occur inside the close command transaction before any run, lease, capability, agent or session mutation.
Zero, missing, extra, duplicate or non-identical matches fail closed. The gate resolver applies the same non-final
obligation predicate before it may write approved final-close status, and requires both session and owning run to be
quiescing. It excludes only current run/chair membership, other exact final- close gates awaiting their resolutions and
the owning in-progress project- drain custody. Approval while active or after any post-drain new-work canary fails with
zero gate mutation.

Acceptance preparation classifies every session run from durable state. A `quiescing` run must have its exact current
chair lease and active run/lease memberships, then moves atomically to `awaiting_acceptance`, freezes that lease and
reconciles those memberships. An already `awaiting_acceptance` run is validated without replaying the transition. A
historical `closed` run must already have reconciled required membership; a terminal `cancelled` or `launch_failed` run
must have explicit abandoned membership and no active current chair lease. Other states fail closed. Reopen and accepted
close update only `awaiting_acceptance` runs, preserving historical terminal states.

The quiesce-exit/reopen transaction supersedes every gate in the session whose exact operation binding names
`fabric.v1.project-session.close`, advances those gate revisions and reconciles any still-active gate memberships before
it restores run/lease memberships. New work may then proceed, but the next acceptance cycle cannot reach
`awaiting_acceptance` or `closed` until a fresh gate per affected run is created and explicitly resolved. Crash rollback
exposes either the entire prior cycle or the entire superseded/new-active lifecycle, never a reopened session with a
reusable approved gate. For `quiescing -> active` it also restores every affected run to active; for a
reconciliation/recovery/quarantine exit it moves the affected runs to the same exceptional state and freezes their
current chair leases. Only the exact receipt-bound `quiescing -> awaiting_acceptance` path preserves approved close
gates. Every non-close exit from `awaiting_acceptance` uses the same invalidation transaction and restores or abandons
the exact affected run/current-chair memberships according to their new durable source state. Public transition cannot
enter `quiescing`; only typed project-drain custody may do so. Transitions among active, visibility-degraded and
exceptional session states also CAS the affected run lifecycle and current chair-lease status, and crash rollback
exposes neither half. A work-admitting target requires exact active required run and current-chair membership plus a
live current-chair capability. A lost launched-chair bridge blocks every generic departure until its typed recovery
custody commits or abandons the loss. Legacy imports bind both memberships, and a forward-only migration repairs earlier
task, message and chair-lease membership dispositions plus session revisions idempotently. Protocol parsing and
projection distinguish a human decision from the closed system-supersession disposition. Reopen may write the latter
only while moving a pending/deferred close gate to `superseded`; it never satisfies a gate, acceptance receipt or
consequential-operation authority check. The disposition's cause is a closed `{kind, ref}` union, so an internal chair-
loss event is never mislabeled as an operator command and every reference names an existing durable owner record. Daemon
dispatch checks the negotiated `gate-system-supersession.v1` result feature before returning a gate carrying that arm.
Old-client/new-daemon fixtures prove read and dedupe replay fail with typed feature unavailability and zero mutation,
rather than failing later during client decode. Gate create, human terminal resolution and reopen supersession update
the gate row, membership row and owning session membership/session revisions in one transaction. Exact command replay
returns the committed revisions without a second increment; crash rollback exposes none of them.

Every takeover and chair-bridge recovery transaction that increments chair generation also revokes the predecessor chair
lease, abandons its membership with `chair-takeover` or `chair-bridge-recovery`, inserts the successor as the sole
active required lease member and advances the session membership revision. No superseded chair lease remains frozen
after the atomic successor commit. Generic membership target/disposition validation recognises write, chair and
task-owner lease tables with exact session/run binding.

Deterministic verification additionally covers arbitrary/stale/cross-session acceptance references, non-human and
wrong-operation gates, typed and native confirmation arms, multi-run terminal history, close/reopen preservation,
post-reopen work with old-reference rejection and fresh-gate acceptance, takeover and bridge-recovery crash rollback,
and released/revoked membership validation for all three lease owners.

## Terminal bridges, singleton topology and multi-session operation

Migration 0013 is forward-only. Its preflight rejects more than one non-terminal run per project session, a
missing/ambiguous current chair, or a terminal lost/pending bridge; it never edits migrations 0001–0012. It installs an
all-mode partial unique run index and a partial unique active-chair-lease index. It re-derives
run/current-and-predecessor chair lease, task, required-message, write/task-owner lease, workstream and provider-action
membership from source truth, updates each changed membership, and advances each affected session membership/session
revision exactly once. Upgrade and restart fixtures cover zero-delivery messages, expired/abandoned delivery,
cancelled/degraded tasks, missing current chair membership and superseded predecessor leases.

Clean accepted/cancelled/failed close, typed project stop and chair-recovery abandon persist immutable bridge-retirement
evidence in their transaction. The retirement binding names the session/run, terminal kind/reference, exact owner
command or recovery and timestamp. It is admitted only after terminal run/session state, revoked current chair
lease/capability and archived agent are rechecked. Child bridge rows move from `active` to `none` with provider and
capability fields cleared. Existing terminal rows are backfilled only under the same proof; otherwise migration fails
for explicit recovery. Startup excludes retired launched bridges and `none` child bridges. After commit, supervision
best-effort closes and removes volatile transport/action/generation mappings; process crash already closes those
transports and cannot undo durable fences.

Cancelled or failed close owns only `draft`, `awaiting_launch`, `launch_failed` and `awaiting_acceptance`. The last
source supersedes all final- close gates before the closure predicate; pending/deferred memberships become abandoned and
human-resolved history stays reconciled. Active/quiescing stop, launch ambiguity, lost-chair recovery and quarantine
remain with their typed owners. Recovery-abandon rejects any unrelated active membership or durable source obligation,
then abandons exactly the current run/lease memberships, revokes all run capabilities, archives agents, retires bridges
and increments membership revision once with crash rollback.

Launched-chair graceful replacement has a distinct live-handoff custody. Its prepare/dispatch/observe/commit state is
generation-bound and promotes only an already retained successor child bridge under the same provider contract. Generic
chair takeover rejects both active and lost launched-chair rows; lost rows use recovery custody. No path can leave the
durable launched bridge naming the predecessor while the run names the successor.

The recovery supervisor enumerates retained bridge keys globally but fences each exact project-session/run/revision in
its own SQLite transaction. One corrupt or unavailable session reports typed recovery evidence without rolling back a
sibling session already fenced. Retries are idempotent per stable loss ID.

`workstreams.v1` owns the chair-authenticated coordinated-workstream create and terminal-state operations described by
the Agent Fabric contract. The daemon transaction binds the root task/team, narrowed authority/budget, resource scope, workstream and
membership and proves that no second chair/run was created. Operator projection includes `projectSessionId` in every run
reference, summary and detail. The Console retains its project-scoped client, opens a secondary exact selected-session
client, auto-selects only one attachable session and otherwise requires an explicit stable session choice; it never
discards project-level authority needed to start another independent session.

`run-session-projection.v1` is a closed result-shape feature for operator snapshot, projection-page, view-page and
detail-read results. When negotiated, every returned run projection and every run row summary/reference/detail contains
the same exact `projectSessionId`; missing or mixed presence rejects the whole result before the client consumes it.
When unnegotiated those fields are omitted from the generic protocol shape. The pre-release Console requires the feature
during initialise and performs no retry or identity inference. A peer that cannot negotiate it is explicitly
incompatible.
