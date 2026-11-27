# Policy table: the section 3.2 audit

Owner: `tools/front-door` records and run crates. Anchor: the compiled table at
`crates/run/assets/policy.toml` and the shared contracts in `docs/specs/agent-native.md`.

This document records the audit of all 83 rows that refused on the slice 0 base.
The table is the single executable source of rule codes, classes, strengths and
groups; this document is its justification record. No new hard boundary is added.

## The classification rule

A row is placed by what refusing it protects, not by the name of the check that
raises it:

| Predicate | Class | Effect |
|---|---|---|
| Guards credentials; destroys another live lane's refs, commits or uncommitted files; or a user gate, merge, release or ruleset | `credentials`, `other_work`, `authority` | refuses; immutable, no knob |
| Proceeding cannot produce a valid result (unrecordable prompt, unsupported confinement claim, missing object) | `operation` | typed operation error naming the cause; immutable, never cites ADR 0035 |
| Everything else, including ownership, busy and lineage checks that destroy nothing | `none` + `group` | warns and proceeds; `refusable`, `knob = policy.<CODE>` |

A `none` row resolves from `policy.<CODE>`, then `policy.<group>`, then
`policy.default`, then its compiled strength (layer first, then specificity). Only
rows with `refusable = true` can be raised to `refuse`; `--strict` sets
`policy.default = refuse` and `--checks off` sets `policy.default = off`.
Explicit `policy.<CODE>` on a boundary or operation row warns
`CHECK_NOT_OVERRIDABLE`; `default` and group keys never do.

## Groups

| Group | Covers | Configured stop |
|---|---|---|
| `confinement` | grant, root, view and cwd overlap; hard links on non-credential grants | `policy.confinement = refuse` |
| `git` | alternates, worktree registration and Git control links | `policy.git = refuse` |
| `custody` | lease, lineage, handover and moved-head checks | `policy.custody = refuse` |

## The 83-row audit

`Before` is the class on the slice 0 base; `After` is the compiled class now.
Rows marked `retained` already matched the rule and keep their code, class and
immutable refusal. Every `after = none` row carries its group, knob and
`refusable = true`; every `after = operation` row names the cause it reports.

| Code | Before | After | Group | Decision |
|---|---|---|---|---|
| `VALIDATION_VERIFY_WORKTREE_AUTHORITY_GIT_ISOLATION` | other_work | none | confinement | An exact grant with other hard links warns: settlement moves hard-linked handoffs aside, so the link destroys nothing. A replaced or missing checkout is the typed operation error SOURCE_IDENTITY_MISMATCH. |
| `VALIDATION_VERIFY_GIT_CONTROL_LINKS_GIT_ISOLATION` | other_work | none | git | A hard link on a Git control file warns: settlement moves hard-linked handoffs aside before a writer starts, so the link is not destruction of another lane's work. |
| `VALIDATION_VERIFY_GIT_CONTROL_LINKS_DATA_INTEGRITY` | other_work | operation | - | An incomplete control-directory scan cannot establish that protected Git files have no extra links; proceeding cannot produce a valid verification, so this is a typed operation error. |
| `VALIDATION_REFUSES_LANE_ROOT_CREDENTIALS` | credentials | credentials | - | retained — A lane root must not expose credential stores. |
| `CONTRACT_CONTINUATION_VALIDATE_AUTHORITY` | operation | operation | - | retained — A continuation must match its recorded route and grant identity. |
| `CONTINUATION_CONTINUATION_REQUEST_AUTHORITY` | operation | operation | - | retained — A continuation must resolve the provider and model actually applied to its prior attempt. |
| `CONTINUATION_CONTINUATION_FOR_AUTHORITY` | operation | operation | - | retained — A continuation requires its recorded session, applied route and account identity and must match the selected route. |
| `CONTINUATION_COPY_STATE_CLASSIFIED_DATA_INTEGRITY` | other_work | other_work | - | retained — Hard-linked retained state cannot be copied as an independent history object. |
| `CONTINUATION_COPY_STATE_CLASSIFIED_AUTHORITY` | operation | operation | - | retained — Retained state copying must remain on the anchored filesystem. |
| `LAUNCH_APPLIED_WRITE_GRANTS_AUTHORITY` | operation | operation | - | retained — Applied write grant paths must be representable so continuation and handoff use the selected attempt's actual authority. |
| `CONFINEMENT_HOST_INDEXES_PROTECTED_DATA_INTEGRITY` | other_work | none | confinement | A write grant that overlaps a host index or durable run state warns and is re-rooted into a staged copy; overlapping grants are advisory and no other lane's data is overwritten. |
| `PREPARATION_PREPARE_CANDIDATES_AUTHORITY` | authority | operation | - | The selected adapter lacks a declared capability or write access. Proceeding cannot produce a valid launch, so this is a typed operation error naming the missing capability, not a user-gate boundary. |
| `LANE_LAUNCHSPEC_CONFIGURE_PUBLISHING_AUTHORITY` | operation | operation | - | retained — Publishing requires valid absolute paths before constructing its selected credential and Git wrappers. |
| `PREPARATION_PREPARE_WITH_CREDENTIALS` | credentials | credentials | - | retained — Publishing policy must be read and its selected GitHub credential restrictions applied before publishing is enabled. |
| `DISPATCH_PREPAREDPROVIDER_CHECK_POLICY_AUTHORITY` | operation | operation | - | retained — A prepared continuation must retain its validated task identity. |
| `LAUNCH_RUNSTORE_START_INNER_AUTHORITY` | operation | operation | - | retained — The stored prompt must match its recorded length and digest before a host launches the task. |
| `ARTIFACTS_RUNSTORE_ARTIFACT_BYTES_DATA_INTEGRITY` | other_work | operation | - | A prompt artifact above its recorded length or failing its digest cannot be reopened as the authorised task; this is a typed operation error naming the artifact and bound. |
| `ARTIFACTS_RUNSTORE_ARTIFACT_BYTES_AUTHORITY` | operation | operation | - | retained — If the stored prompt cannot be reopened, its bytes cannot be verified against the authorised task and no substitute prompt is launched. |
| `PREPARATION_CHECK_GRAPH_INPUTS_ROOT_AUTHORITY` | operation | operation | - | retained — A graph-bound task may receive only its own node's input directory, without a redirected path. |
| `CONFINEMENT_SOURCE_CWD_AUTHORITY` | operation | none | confinement | A valid registered checkout outside the read roots is added to the roots and warns; a missing or replaced checkout is the typed operation error SOURCE_IDENTITY_MISMATCH. Path ownership is advisory and destroys nothing. |
| `CONFINEMENT_REQUIRE_SOURCE_HEAD_AUTHORITY` | operation | operation | - | retained — A pinned graph source must have a valid recorded head and must not move before preparation. |
| `CONFINEMENT_READ_ONLY_PLAN_AUTHORITY` | operation | operation | - | retained — Read-only plans require the selected project binding and a Git repository when the source is pinned. |
| `SOURCE_TREE_ENSURE_SOURCE_ROOTS_AUTHORITY` | operation | none | confinement | Under a pinned view, an overlapping access or asset grant is re-rooted to a copy staged inside the view and warns; it is never bound to the live tree. If re-rooting is impossible the grant reads the live tree with LIVE_TREE_EXPOSED. |
| `SOURCE_TREE_ENSURE_PROFILE_ISOLATED_AUTHORITY` | operation | none | confinement | Under a pinned view, a provider grant that would reopen the live checkout is re-rooted to a staged copy and warns; overlapping grants are advisory claims and destroy no other lane's work. |
| `SOURCE_TREE_ENSURE_VIEW_READ_ONLY_AUTHORITY` | operation | operation | - | retained — Provider writes cannot modify the immutable source view supplied to the task. |
| `CONFINEMENT_WRITER_SOURCE_CONTEXT_AUTHORITY` | operation | none | custody | A writer continuation whose predecessor head moved re-pins to the current head and warns WRITER_HEAD_MOVED; retained uncommitted files are listed, never reset. An unreadable predecessor is an operation error. |
| `HAND_OFF_RUNSTORE_HAND_OFF_GIT_ISOLATION` | other_work | other_work | - | retained — A retained writer import keeps its worktree fenced until the protected Git state is reconciled. |
| `WRITER_WRITERLAUNCH_SNAPSHOT_GIT_ISOLATION` | other_work | operation | - | The HEAD is not on the selected branch, so a writer snapshot would capture the wrong tree; this typed operation error names the branch and HEAD and cannot be overridden. |
| `WRITER_WRITERLAUNCH_REQUIRE_CONTINUATION_SOURCE_GIT_ISOLATION` | other_work | none | custody | A head that moved on from the pinned commit is re-pinned and warns WRITER_HEAD_MOVED; a moved head is not destruction. An unreadable predecessor record is a typed operation error. |
| `HAND_OFF_RUNSTORE_HAND_OFF_AUTHORITY` | operation | operation | - | retained — A handoff must use the selected worktree and held custody binding; fenced, unknown, invalid or unconfirmed custody cannot be granted to another attempt. |
| `CONTROLS_RUNSTORE_CHECK_WRITER_LINEAGE_AUTHORITY` | operation | none | custody | Custody is a lease derived from the holder's attempt state. Taking a lapsed lease warns WRITER_HANDOVER and keeps the prior lane's uncommitted files; only destroying a live lane's work refuses. |
| `HAND_OFF_RUNSTORE_HAND_OFF_AUTHORITY_2` | operation | operation | - | retained — An attempt changed after custody acquisition cannot be handed off. |
| `PLAN_CONFINEDPLAN_NEW_AUTHORITY` | authority | none | confinement | A read-only lane that enables extensions or inherited hooks warns and proceeds with the plain profile; it destroys no other lane's work. Claiming a nested confinement the backend cannot enforce is the typed operation error PLAN_NESTED_CONFINEMENT_UNSUPPORTED. |
| `PLAN_CONFINEDPLAN_NEW_CREDENTIALS` | credentials | credentials | - | retained — Secret roots and provider-private state must not overlap grants that expose credential material. |
| `PLAN_CONFINEDPLAN_NEW_DATA_INTEGRITY` | other_work | none | confinement | Overlapping grant roots warn and are re-rooted into the pinned view; claim overlap alone never destroys another lane's work and is not a boundary. |
| `PLAN_CONFINEDPLAN_NEW_CREDENTIALS_2` | credentials | credentials | - | retained — Agy's selected home source and private destinations must not overlap secret or protected roots. |
| `PLAN_CONFINEDPLAN_NEW_AUTHORITY_2` | operation | operation | - | retained — Required roots must be representable and disjoint so the selected grant and private state can be enforced. |
| `RENDER_COMPILE_SBPL_GIT_ISOLATION` | other_work | other_work | - | retained — Writer grants must match held worktree authority and keep real Git metadata outside writable scratch. |
| `RENDER_COMPILE_SBPL_CREDENTIALS` | credentials | credentials | - | retained — Credential, secret and private-state exclusions must hold after anchors and grants are resolved. |
| `RENDER_COMPILE_SBPL_AUTHORITY` | operation | operation | - | retained — The backend cannot claim a requested nested confinement policy that it cannot enforce. |
| `RENDER_COMPILE_SBPL_AUTHORITY_2` | operation | operation | - | retained — Held grant anchors are verified before and after rendering so replaced paths cannot change the authority that was approved. |
| `RENDER_COMPILE_SBPL_AUTHORITY_3` | operation | operation | - | retained — Required anchors must be present and resolvable to enforce the selected grants. |
| `RENDER_SBPL_PATH_AUTHORITY` | operation | operation | - | retained — Required paths must be encoded losslessly to enforce the requested permissions; optional discoveries warn and are omitted. |
| `PROBE_SYSTEM_BACKEND_CREDENTIALS` | credentials | credentials | - | retained — Without an enforcing sandbox the lane could read credential stores outside its selected authentication grants. |
| `PROBE_SYSTEM_BACKEND_GIT_ISOLATION` | other_work | other_work | - | retained — Without an enforcing sandbox a writer could modify the real Git administrative and common directories. |
| `REPO_VERIFY_WORKTREE_GIT_ISOLATION` | other_work | other_work | - | retained — Worktree registration, common Git identity, backlinks and repository revalidation bind a writer to its selected project and Git directories. |
| `REPO_REJECT_ALTERNATES_DATA_INTEGRITY` | other_work | none | git | Borrowed objects from an alternate store are copied into the lane store and then used, with a warning; only objects that cannot be read are a typed operation error. |
| `REPO_VERIFIEDREPO_OPEN_STRUCTURE_GIT_ISOLATION` | other_work | other_work | - | retained — The selected repository and Git marker must be owner-controlled and their descriptors must identify the same Git structure. |
| `REPO_VERIFIEDREPO_REVALIDATE_GIT_ISOLATION` | other_work | other_work | - | retained — A probed repository's Git structure and configuration must retain their recorded identities at use time. |
| `REPO_READ_BOUNDED_GIT_ISOLATION` | other_work | operation | - | Git markers and configuration must be read completely and consistently; a short or racy read cannot produce a valid verification of the selected worktree and is a typed operation error. |
| `REPO_REJECT_ALTERNATES_DATA_INTEGRITY_2` | other_work | none | git | The alternates declaration is copied into the lane store and warned about before history is accepted; the snapshot dissociates the borrowed objects rather than refusing them. |
| `OPENCODE_PREPARE_CREDENTIALS` | credentials | credentials | - | retained — The selected authentication policy and private provider state must match OpenCode's credential isolation. |
| `OPENCODE_PREPARE_AUTHORITY` | operation | operation | - | retained — A resumed invocation requires its validated continuation identity. |
| `OPENCODE_PREPARE_AUTHORITY_2` | operation | operation | - | retained — When a Git-context prompt refers to scratch, OpenCode must receive an exact matching path for that authorised context. |
| `COMMAND_PREPARE_CREDENTIALS` | credentials | credentials | - | retained — A configured command does not accept a provider authentication grant. |
| `COMMAND_PREPARE_AUTHORITY` | operation | operation | - | retained — A configured command cannot silently ignore a requested continuation, model, effort or account selection. |
| `COMMAND_PREPARE_AUTHORITY_2` | operation | operation | - | retained — A configured command must have non-empty argv and an absolute host-resolved executable to launch the selected invocation. |
| `COMMAND_PREPARE_AUTHORITY_3` | operation | operation | - | retained — The command adapter only implements read-only access and cannot claim writer authority. |
| `CURSOR_PREPARE_CREDENTIALS` | credentials | credentials | - | retained — The selected login and attempt-private state must be present before Cursor receives its credential access. |
| `CURSOR_PREPARE_AUTHORITY` | operation | operation | - | retained — A resumed invocation requires its validated continuation identity. |
| `AGY_PREPARE_CREDENTIALS` | credentials | credentials | - | retained — Agy must select its keychain authentication; its home remains in attempt-private state or scratch. |
| `AGY_PREPARE_AUTHORITY` | operation | operation | - | retained — A resumed invocation requires its validated continuation identity. |
| `CODEX_PREPARE_CREDENTIALS` | credentials | credentials | - | retained — The selected login and attempt-private state must be present before Codex receives its credential access. |
| `CODEX_PREPARE_AUTHORITY` | operation | operation | - | retained — A resumed invocation requires its validated continuation identity, and Codex must use its clean configuration policy. |
| `CLAUDE_PREPARE_CREDENTIALS` | credentials | credentials | - | retained — Claude receives only its selected account token through the host pipe, with no separate credential root grant. |
| `CLAUDE_PREPARE_AUTHORITY` | operation | operation | - | retained — The token must match the selected account and a resumed invocation must retain its validated continuation identity. |
| `LANE_LANERUNTIME_ENVIRONMENT_AUTHORITY` | operation | operation | - | retained — Required wrapper executable and state paths must remain anchored when providers scrub inherited environment and HOME. |
| `LAUNCH_RUNSTORE_START_INNER_AUTHORITY_2` | operation | operation | - | retained — Request digest and host-owned retry checks bind launch to the validated request and caller. |
| `VALIDATION_VALIDATE_PROFILE_SPEC_IMPL_CREDENTIALS` | credentials | credentials | - | retained — Authentication roots and writes must be confined to the selected provider and ordinary reads and writes cannot encompass credential roots. |
| `VALIDATION_VALIDATE_PROFILE_SPEC_IMPL_AUTHORITY` | operation | operation | - | retained — A process policy must identify exact executables and the Fabric socket must name an exact endpoint before those permissions can be enforced. |
| `VALIDATION_VALIDATE_PROFILE_SPEC_IMPL_CREDENTIALS_2` | credentials | credentials | - | retained — Discovery grant entries are rechecked against held parent anchors before and after rendering so replacement or redirects cannot change credential-backed reads. |
| `VALIDATION_VALIDATE_PROFILE_SPEC_IMPL_AUTHORITY_2` | operation | operation | - | retained — Required profile paths must be representable and valid to enforce the selected permission policy. |
| `LAUNCH_ADMIT_AUTHORITY` | operation | operation | - | retained — The adapter, registered account and continuation must match the selected route, and a read-only working directory must lie within a granted root. |
| `LAUNCH_ADMIT_CREDENTIALS` | credentials | credentials | - | retained — The host home must be absolute so credential exclusions are evaluated against the selected host identity. |
| `LAUNCH_ADMIT_AUTHORITY_2` | operation | operation | - | retained — Executable and working-directory paths must be absolute to bind the launch to the selected executable and granted working directory. |
| `LAUNCH_PRIVATE_GIT_OBJECTS_GIT_ISOLATION` | other_work | other_work | - | retained — A writer's Git wrapper must bind commands to the private Git and object directories. |
| `LANE_LAUNCHSPEC_CONFIGURE_PUBLISHING_CREDENTIALS` | credentials | credentials | - | retained — The publishing wrapper must preserve the selected GitHub credential policy and avoid exposing it to other lanes. |
| `LANE_INSTALL_GIT_GIT_ISOLATION` | other_work | other_work | - | retained — A bound writer Git launcher must be placed first in PATH so Git uses its private view. |
| `LANE_GIT_WRAPPER_AUTHORITY` | operation | operation | - | retained — A Git executable containing a NUL cannot identify an executable to launch; native non-UTF-8 and shell-special bytes are preserved. |
| `IDENTITY_REGISTEREDACCOUNT_VALIDATE_ROUTE_AUTHORITY` | operation | operation | - | retained — The selected account must match the route it authorises. |
| `ADMISSION_RUNSTORE_ADMIT_AUTHORITY` | operation | operation | - | retained — A guardian and store identity are required to bind a lease to its owner and preserve custody when the host later recovers it. |
| `CONTINUATION_COPY_STATE_CLASSIFIED_AUTHORITY_2` | operation | operation | - | retained — The retained state root must remain an anchored directory; symlinks, non-directories and changed roots remain refused at every copy budget. |
| `PREPARATION_PREPARE_CANDIDATES_DATA_INTEGRITY` | other_work | operation | - | The raw prompt must fit RECORD_LIMIT before host preambles are added; an over-long prompt cannot be recorded, so this is a typed operation error naming the bound, never a refusal of another lane's work. |

## Splits and new codes

Where one base row covered both a warn-and-proceed case and an immutable
case, the audit keeps one stable code per half. These codes are added to the
compiled table so later slices bind to a name rather than inventing one:

| Code | Class | Group | Why it exists |
|---|---|---|---|
| `PLAN_NESTED_CONFINEMENT_UNSUPPORTED` | operation | - | The backend cannot provide the nested confinement requested by the plan; proceeding cannot enforce the claim, so this typed operation error names the requested mode and the enforcing backend. |
| `WRITER_HEAD_MOVED` | none | custody | A continuation whose predecessor head moved re-pins to the current head and warns; it names the prior lane, its terminal state and last head, and keeps uncommitted files. |
| `GITDIR_REGISTRATION_STALE_NONE` | none | git | A stale worktree registration whose backlink names this path and which no live lease names another holder is repaired with git worktree repair, re-verified, and warned; only a registration a live lane's lease names refuses. |
| `SOURCE_IDENTITY_MISMATCH` | operation | - | A checkout that is missing or replaced cannot be pinned to its recorded identity, so the source cannot be prepared; this typed operation error names the expected and observed identity. |
| `LIVE_TREE_EXPOSED` | none | confinement | An overlapping grant that cannot be re-rooted into the pinned view reads the live tree and warns; it is an advisory claim and destroys no other lane's work. `policy.confinement = refuse` stops instead. |
| `GITDIR_LAYOUT_INVALID` | operation | - | A fabricated Git layout or a backlink that does not name this worktree cannot be repaired into a valid registration; this typed operation error names the layout. |
| `UNREADABLE_PREDECESSOR` | operation | - | A writer continuation whose predecessor record cannot be read has no pinned commit or owner to inherit, so it is a typed operation error naming the predecessor. |

The two custody rows named in section 3.2
(`WRITER_WRITERLAUNCH_REQUIRE_CONTINUATION_SOURCE_GIT_ISOLATION` and
`CONFINEMENT_WRITER_SOURCE_CONTEXT_AUTHORITY`) now warn and re-pin on a moved
head; their `WRITER_HEAD_MOVED` half can be raised or silenced by
`policy.custody`. An unreadable predecessor is `UNREADABLE_PREDECESSOR`.
`REPO_VERIFY_WORKTREE_GIT_ISOLATION` stays `other_work` for the
`GITDIR_HELD_BY_OTHER_LANE` case, where repair would rewrite a registration a
live lease names; the stale-backlink half is `GITDIR_REGISTRATION_STALE_NONE`,
and a fabricated layout or mismatched backlink is `GITDIR_LAYOUT_INVALID`.

## Cross-slice handoff

Reclassifying a row changes which enforcement path its consumer must call.
The owning behaviour slices update their call sites in their own commits:
hard links (slice 10), `source_cwd` and overlap re-rooting (slice 11),
the `PLAN_CONFINEDPLAN_NEW_AUTHORITY` split (slice 13), the custody and
lineage path (slice 34) and Git registration repair (slice 9). Until each
call site moves, a `none` row is reached through `warn_or`, not
`boundary_error`. No migration is needed: the `policy.<CODE>` keys and their
values already exist in host, project and dispatch configuration.
