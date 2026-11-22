# Run launch and admission

Owner: `crates/run`, `crates/confinement`, `crates/providers`
Anchor: `crates/run/src/launch.rs`, `crates/run/src/admission.rs`

Paths are relative to `tools/front-door/`.

## Admission and custody

Admission commits the reservation and attempt together. A worktree's owning lane decides who
else writes in it; active lanes share an occupancy lease. Resource probes run outside the
admission transaction, and busy capacity is not a queue. Uncertain custody keeps its ownership
until release is proved. Reconciliation reports released custody only after exclusion and native
absence evidence prove it safe; an unknown or live holder retains custody, and uncertain custody
and unreadable reservations warn while reconciliation continues. Writer-bind refusals identify
the durable lease holder, branch, worktree, reservation age (from its last durable update) and
observed liveness.
- Why: shared work must remain possible without holding coordination locks across external work.

Nested sharing is authorised by the owning guardian's host-sealed worktree incarnation and a
kernel-observed caller relationship. A nested writer may share that exact checkout; independent
work provisions its own. Admission pins the shared checkout's directory identity, rechecked at
dispatch and after acquiring occupancy, so a removed and recreated path cannot inherit the grant.
Nested provisioning records its creating graph and node in the checkout's private Git directory,
and recovery never reclaims a checkout without that receipt. Shared launches observe the live
branch and never advance the parent's checkout. `write_scope` paths are advisory claims: an
overlap in one worktree warns the owning lane and refuses nothing.
- Why: sharing worktree files must never share staged ownership.

Each attempt owns its Git index, HEAD, refs, objects, configuration and credentials;
`GIT_INDEX_FILE` names the index inside that private state. A writer index is seeded from the
captured branch tip, never from a host or sibling snapshot, so a plain commit includes only that
attempt's staging and launching another attempt never resets a live sibling's index. Complete
private views keep native semantics for commit, merge, cherry-pick and rebase without a shell
interceptor. A commit becomes visible on the host branch only after object verification and
settlement's expected-old-tip compare-and-swap. A divergent sibling publication is refused and
retains the loser's private commits and index; there is no automatic rebase or retry. A
continuation that needs a predecessor commit launches after that predecessor settles. Scope
attribution starts at the attempt's captured launch tip, and budget commits publish an
owned-path tree into the attempt's own ref, never a later observation of shared HEAD.
- Why: every Git operation gets private state, and publication conflicts preserve both histories.

An unscoped budget stop may commit all dirty paths only if the attempt was alone throughout its
custody. Admission records a durable holder generation and whether a sibling was present, and a
later sibling stays visible after it exits. The budget operation holds the admission gate from its
history check through its commit. If sharing occurred it leaves files and staging intact and
records a typed `shared_unscoped` leftover with its paths; declared path scopes bound automatic
staging.
- Why: a point-in-time census cannot establish ownership of shared working files.

Settlement copies object files through held, no-follow descriptors into a bounded host-owned
sender before sanitising sidecars; inconsistent input fails integrity checks before publication.
Unpublished commits and staged indexes remain private recovery evidence through terminal cleanup.
Provenant holds no repository-wide lock around Git commands; explicit `*.lock` collisions get
brief bounded retries with jitter, and semantic conflicts and permission failures return
immediately. A satisfied source pin has no checkout effect and does not exclude live shared
holders; an actual checkout advance or unshared reuse still requires a clean tree.
- Why: independent worktrees must not queue behind sibling launches.

The host records each prepared private index's attempt and directory identities in the worktree
custody slot before provider spawn. Unresolved reservations keep the checkout fenced even after a
holder crash, and under exclusive removal custody the native index and every recorded attempt
index must be clean. A terminal outcome alone cannot authorise removal.
- Why: staged snapshots can survive after the native checkout appears clean.

Before handoff or guardian spawn, the launcher durably prepares the per-attempt `guardian.lock`
inode alongside the attempt, writer and custody identities. The guardian acquires and verifies it
before acknowledging custody or spawning the provider; the provider and its descendants never
inherit it. The guardian holds it through terminal publication and custody release, and kernel
teardown releases it even if the guardian cannot publish an exit. Tick's settlement driver
consumes that proof under occupancy leases and a fresh census; a retained or unknown process
observation cannot substitute for the original free locks or positive non-occupancy.
- Why: a stale process checkpoint cannot prove same-boot guardian death; the original kernel lock
  supplies that evidence without PID-reuse heuristics.

A launch is uncertain only once its reservation may record a handoff. A host that cannot hold
provider custody refuses before that point: the attempt ends with `unsupported_platform` and
its custody is withdrawn. A launch cancelled or refused before it spawns records `never_spawned`
on every host.
- Why: a launch that cannot start must not look like a lost lane.

Permanent pre-admission refusals are durable outcomes of their operation: replay returns the
refusal without preparing again, and a failure to store it is not acknowledged as success.
- Why: a lost response must not turn a refusal into another launch.

Every refusal path in lane launch carries a typed terminal cause plus the command that fixes it,
recorded in `terminal-pending.json` and shown by `result` and `wait`. A refusal whose record
names none reads `unrecorded`, never a cause the host did not observe. A pre-launch routing refusal
keeps a bounded host-authored explanation; provider diagnostics never supply it.

Terminal attempts are immutable. A retry is a new linked attempt, a terminal attempt has at most
one successor, and a writer is never replayed over its edits.
- Why: continuation must preserve both history and its attempt's custody.

## Graph naming

A named launch treats its name as a handle for the launch input. The same input replays the
existing graph and starts nothing; changed input starts the next free `NAME-k` and leaves earlier
graphs alone. An unnamed launch takes the next `BASE-k` from 1 and never replays. The name
grammar refuses with the normalised form (`--name Foo-` suggests `foo`); reserved names are in
[state](state.md#graph-names-retain-one-identity).
A suffixed launch says so in its first output line, and `--json` carries the prior handle as
`named.superseded`. `wait NAME` and `status NAME` keep reading the older graph but warn that it was
superseded and print the newer handle as the next step; they never redirect silently.
- Why: a reused name must not silently attach a different brief to old work, and the operator must
  see the new handle without losing the old graph.

## Writer binding

`--branch B --base REF` allocates a managed writer worktree under the project layout, not under
the invoking checkout. The launch owner resolves and checkpoints `REF` before any Git effect,
creates the worktree from that commit and binds it by source identity and head, identically from
a primary checkout, a linked worktree, a separate Git directory or a bare linked checkout. An
existing clean worktree at the exact checkpointed head is rebound; a different branch, head or
unclean tree is refused with the named cause.
- Why: an owned writer must land in one project-owned location wherever the launch ran, and a
  failed bind must not leave a half-made worktree.

Every writer lane's prompt opens its host preamble with one `Lane location` paragraph built once
by `lane_preamble_with`: the absolute bound worktree path, branch and base commit, and that the
primary checkout is not the lane's working copy. It is advice, never a fence, and it says an
absolute path elsewhere in the lane's context does not override it. Launch records the primary
checkout's branch and ref tips (`primary-checkout.json`) so settlement can detect
[stray work](run-settlement.md#stray-lane-work). The lane receives `PROVENANT_ROUTE`,
`PROVENANT_MODEL`, `PROVENANT_EFFORT` and `PROVENANT_ACCOUNT`.
- Why: a provider's system prompt may name the primary checkout as the workspace root of a linked
  worktree, and a brief saying only "your worktree" loses to that absolute path.

A node whose launch condition (`when`) does not hold never runs, so it needs no writer scope:
submission neither inspects nor preflights its worktree. A composite template takes no launch
control (access mode, worktree, branch, base or write scope) and the first one given is named in
the refusal; the bundled `smoke` template is the exception for worktree and branch/base, which
bind to its write node only. Remote placement takes neither control. A graph reader without an
explicit worktree or attempt directory uses its selected project as its source; `upgrade --verify`
selects a temporary committed scratch project for its smoke graph.
- Why: a verification graph must run on a real install, and a skipped node must not refuse the
  run it does not take part in.

Every refusal of a launch input names the field that caused it, or the decoded path of a document
or patch field; an unnamed `invalid input` is a defect. A refused bind keeps its typed cause to
the facade: a held writer lease is `writer_lease_held` and names its holder locally, and scope,
confinement and resource failures keep their own codes ([CLI](cli.md#run-refusals-keep-their-own-code-at-the-facade)).
- Why: the operator needs the real cause to repair a launch.

## Policy and confinement

Launch policy refuses only at the [hard boundaries](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md).
Other checks allow or warn unless configured otherwise. Policy rows live in
`crates/run/assets/policy.toml`; a non-boundary rule defaults to warn or off, may offer an explicit
`refuse` setting (a configured stop), and `Policy::warn_or` reports its source and knob.
Immutable boundary refusals use `boundary_error`. Heuristic scanners offer no stop. The
`policy.<CODE>` knobs are in `config/harness-knobs.json`. A write scope lists expected files: a
writer edits another file when the fix needs it and lists it in its handoff, and only a strict
`WRITE_SCOPE` policy refuses that, before publication, retaining private work.
- Why: advisory checks must not become accidental authority boundaries.

Every provider lane has its native tool and shell surface; read-only mode constrains filesystem
writes and does not remove shell, editing or web tools. The common confinement planner owns
worktree writes, reads of the primary and sibling checkouts, explicit read roots, isolated-mode
credential denials and network reach (open by default) for every adapter. Every launch finalises
the host Cargo home and lane target through the one environment path. Private harness instruction
files receive the global rules compiled into the binary. Native integration differences belong to
`AdapterSpec`, with a reason and source for an unavailable capability. Read-only write exceptions
come from user-owned configuration, stay within verified repository paths, are recorded on the
attempt, and exclude Git and provider control paths. Host-native launches keep confinement and
provenance but cannot resume a provider session or select model-specific controls.
- Why: provider selection must not silently narrow a lane's working surface, and only the user
  may widen a read-only lane's write authority.

### Lane access

`LaneAccess` resolves `lanes.access` to `host` (default) or `isolated` ([ADR 0042](../adr/0042-lane-access-host-by-default.md)).
Under `host`, a same-host lane inherits the host environment, provider home, profile and login,
Git configuration and credential reads, sandboxed or not. Sandboxing still confines writes,
network and explicit denies, but credential, keychain and process-inspection, signal and
task-port denials are lifted. Under `isolated`, sandboxed lanes get a private environment and
provider home, filtered Git configuration and credential denies, and hosted-cloud credential
variables are stripped; the isolated profile also keeps its protection against replacing
credential files. Cross-host credential grants follow their own rules.

One resolved choice governs every access decision. Credential-root classification identifies the
most specific declared store and its source. When a declared store or credential file overlaps a lane
root, a store cannot be resolved precisely, a grant overlaps credentials, a tool path is
discovered, delivery or continuation needs host Git transport or linked retained files, or the
sandbox backend is unavailable: `isolated` keeps its refusal or omission, and `host` warns with
the paths, effective access and source, then proceeds (recording an unavailable backend in the
sandbox receipt). `LaneAccessGrant` retains the input spelling for diagnostics, while the
canonical grant is the only path used for authority checks. Route selection precedes the
credential decision, so a host-access fallback can still proceed.

`resolve_lane_access` gives an explicit launch, template or tier knob priority over route policy,
then the host or project knob, then the default; the applied route receipt records value, source
and deciding key. Invalid knob text warns and resolves to `host`.
- Why: sandbox selection must not silently select several unrelated restrictions, and same-host
  credential classification must not become a launch boundary.

Under `host`, every adapter's `prepare` (OpenCode included) uses the native home and session
store, including in sandboxed launches; OpenCode attaches to its service when available, Claude
uses native authentication, and native discovery reads the user's own skills. An explicitly
selected account keeps its account-specific home. `isolated` uses private per-attempt state, the
managed Claude token pipe and `PrepareInput::stage_user_skills`. `lane_provider_var` passes the
captured `SSH_AUTH_SOCK` to `finalise_provider_env`, which grants the exact agent socket to a
sandboxed host-access lane and never its parent directory.
- Why: a same-host lane can authenticate through the user's login and SSH agent.

A lane sees the host's Git configuration as the host does. Every lane's private global Git
configuration includes the host's global files in full (XDG `git/config`, then `~/.gitconfig`, or
the host's `GIT_CONFIG_GLOBAL` alone); `bind_host_git_config` supplies them to an unbounded
read-only lane under `host`. A sandboxed lane also receives an exact read grant on each file under
its canonical physical path so the include resolves under confinement. The lane's Git wrapper
leaves `GIT_CONFIG_NOSYSTEM` and `GIT_CONFIG_COUNT`, `_KEY_*`, `_VALUE_*` and `_PARAMETERS`
alone, its own token helper entries are appended after the host configuration (an empty
`credential.helper` resets the list so a GitHub push uses the lane's token), and
`user.useConfigOnly` stops a missing identity falling back to a generated hostname. No host
entry is filtered, copied or rewritten.
- Why: lanes are free to see the host's Git settings, not a filtered subset (ADR 0035).

### Lane state stays outside the worktree

`finalise_provider_env` rebinds `HOME` and the `XDG_*` homes into the attempt scratch
(`scratch/home`, `scratch/xdg/<name>`, mode 0700) whenever the final value is the worktree or
under it, compared lexically and canonically, and records one warning naming the variable, old
path, new path and reason. Values outside the worktree are unchanged.
- Why: a lane whose tool state resolves inside its checkout can commit that state with its work,
  and the one place every adapter finishes is where it can be prevented.

The selected sandbox floor applies to each candidate and is sealed for continuations.
`SandboxMode::resolve_backend` refuses an unavailable backend under `isolated`; `host` warns,
proceeds, and records `effective=false` and the reason in the sandbox receipt.
- Why: a retry must disclose when requested confinement was unavailable.

## Inputs and resources

A plain read-only launch from a checkout behind its fetched upstream reviews that upstream head.
`read_only_source_ref` resolves the default branch as last fetched, without fetching, and
`bind_default_read_only_source` pins the graph's read-only nodes to it; the warning names the ref,
the commit and the stale local head it replaced. A level or ahead checkout keeps its local tree,
as do an explicit `source` of `local`, a review range, an authored document and a writer launch.
Absent or unusable Git evidence stays advisory and never refuses the launch.
- Why: file:line evidence from a stale working tree reports findings upstream has fixed.

Unreadable required host inputs reject an attempt before launch, and a pinned source view reports
its failure without exposing captured contents or credentials.
- Why: a partial source view must not be mistaken for the requested input.

Failing to adjust a build's priority never refuses a launch.
