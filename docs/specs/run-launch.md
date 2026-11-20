# Run launch and admission

Owner: `crates/run`, `crates/confinement`, `crates/providers`
Anchor: `crates/run/src/launch.rs`, `crates/run/src/admission.rs`

Paths are relative to `tools/front-door/`.

## Admission and custody

Admission commits the reservation and attempt together. A worktree's owning lane
decides who else writes in it; active lanes share an occupancy lease. Resource
probes run outside the admission transaction. Busy capacity is not a queue. Direct-run custody uses
the same captured project and actor identity as the run store. Uncertain custody
keeps its ownership until release is proved. Writer-bind refusals identify the durable lease holder, branch, worktree, reservation age and observed liveness. Reservation age is measured from its last durable update. Reconciliation reports released custody only after exclusion and native absence evidence prove it safe; an unknown or live holder retains custody. Uncertain custody and unreadable reservations are reported as warnings while reconciliation continues; they remain fenced until safe release is proved. Stable lock inodes remain in place so concurrent observers cannot split exclusion.
- Why: shared work must remain possible without holding coordination locks across external work.

Nested sharing is authorised by the owning guardian's host-sealed worktree
incarnation and a kernel-observed caller relationship. An existing nested writer
may share that exact checkout; independent work provisions its own checkout.
Admission pins the shared checkout's directory identity, rechecked at dispatch
and after acquiring occupancy, so a removed/recreated path cannot inherit the
grant. Nested provisioning records its creating graph/node in the checkout's
private Git directory; recovery never reclaims an existing checkout without
that receipt. Missing creation evidence retains the checkout for host recovery.
Shared launches observe the live branch and never advance the parent's
checkout. Direct host requests retain authority to select existing checkouts.
Optional `write_scope` paths are advisory claims: overlaps in one worktree warn
the owning lane in its admission receipt and checkpoint and refuse nothing.
By maintainer decision (#3451), path overlap is advisory rather than exclusive;
a live conflicting writer is not refused. Each attempt owns its Git index,
HEAD, refs, objects, configuration and credentials. `GIT_INDEX_FILE` names the
index inside its private Git state. A writer index is seeded from the captured
branch tip, never from a host or sibling staged snapshot. A plain commit can
therefore include only that attempt's staging; launching another attempt never
resets or merges a live sibling's index.
- Why: sharing worktree files must never share staged ownership.

Complete attempt-private Git views preserve native semantics for commit, merge,
cherry-pick, rebase and other commit-producing commands. They avoid a shell
interceptor that would need to reproduce every Git publication operation.
Commits become visible on the host branch only after object verification and
settlement's expected-old-tip compare-and-swap. A sibling's divergent publication
refuses and retains the losing attempt's private commits and index for recovery;
it never replaces the winner. There is no automatic rebase or two-tree retry
of a divergent lane. A continuation that needs a predecessor commit launches
after that predecessor settles, or explicitly selects an independently available
source. Scope attribution starts at the attempt's captured launch tip.
Budget commits build an owned-path tree in a disposable index, obtain the OID
from `commit-tree`, and publish it by compare-and-swap into the attempt's own
ref. They never attribute a later observation of shared HEAD to the attempt.
- Why: every Git operation gets private state, and publication conflicts preserve both histories.

An unscoped budget stop may commit all dirty paths only if this attempt was
alone throughout its custody. Admission records a durable holder generation and
whether a sibling was already present. A later sibling admission remains visible
after that sibling exits. The budget operation retains the worktree admission
gate from its history check through its commit, preventing admission in between.
If sharing occurred, it leaves files and staging intact and records a typed
`shared_unscoped` leftover with its paths. Declared path scopes bound automatic
budget staging. Convergence restore still requires no live sibling.
- Why: a point-in-time census cannot establish ownership of shared working files.

Settlement copies object files through held, no-follow descriptors into a byte-
and entry-bounded host-owned sender before sanitising sidecars. Inconsistent
input fails integrity checks before publication. Unpublished commits and staged
indexes remain private recovery evidence through terminal cleanup. Settlement
advances only the native committed index with Git's index-only two-tree merge
under its index lock, leaving working files intact. Conflicting native staging or
operations stay intact; private staging and stopped-merge metadata are never copied into the native index.
Git owns the atomicity of config, index, refs and worktree registration. Provenant
holds no repository-wide lock around Git commands. Explicit `*.lock` collisions
receive brief bounded retries with jitter; semantic conflicts and permission
failures are returned immediately. Prepared ref deletion retries only before
Git has acknowledged preparation. A satisfied source pin has no checkout effect
and does not exclude shared holders or their edits while another holder is live;
with no live holder, ordinary reuse falls through to exclusive occupancy and verifies
cleanliness, so an actual checkout advance or unshared reuse still requires a clean tree.
- Why: independent worktrees and coherent config reads must not queue behind sibling launches.

The host records each prepared private index's attempt and directory identities
in the worktree custody slot before provider spawn. Unresolved reservations keep
the checkout fenced even after a holder crash. Settlement retires a clean index
record before scratch cleanup; staged indexes remain recovery evidence. Under
exclusive removal custody, the native index and every remaining recorded attempt
index must be clean. A terminal outcome alone cannot authorise removal.
- Why: staged snapshots can survive after the native checkout appears clean.

Before entering handoff or spawning a guardian, the launcher durably prepares
the per-attempt `guardian.lock` inode alongside the attempt, writer and custody
identities. The guardian acquires and verifies that original inode before
acknowledging custody or spawning the provider, then records its native process
identity. The prepared record is sufficient even if the guardian dies before
publishing its own identity or updating the pre-handoff reservation: the
original free holder and custody leases exclude a live launcher or guardian.
Only the guardian holds the life lock after preparation;
the provider and its descendants never inherit it. The guardian retains it
through terminal publication and custody release. Kernel process teardown
releases it even when the guardian cannot publish an exit.
Tick's settlement driver consumes that proof under the appropriate occupancy
lease and a fresh worktree occupancy census. A retained or unknown process observation
cannot substitute for the original free locks or positive non-occupancy.
- Why: a stale process checkpoint cannot prove same-boot guardian death; the
  original kernel lock can supply that evidence without PID-reuse heuristics.

A launch is uncertain only once its reservation may record a handoff. A host
that cannot hold provider custody refuses before that point: the attempt ends
with `unsupported_platform` and its custody is withdrawn, never reported as an
unknown launch awaiting a receipt. A launch cancelled or refused before it
spawns needs no custody, so it records `never_spawned` the same way on every
host.
- Why: a launch that cannot start must not look like a lost lane.

Permanent pre-admission refusals are durable outcomes of their operation. Replay
returns the refusal without preparing again; a failure to store it is not
acknowledged as success.
- Why: a lost response must not turn a refusal into another launch.

A pre-launch routing refusal retains a bounded host-authored explanation in
its result alongside the safe cause and remedy. Provider-originated diagnostics
never supply that public explanation. Every refusal path in lane launch carries
a typed terminal cause plus the command that fixes it, recorded in the attempt's
terminal record (`terminal-pending.json`) and displayed by `result` and `wait`.
Refusals never settle without a typed cause. A refusal whose record names none
reads `unrecorded`, never a cause the host did not observe.

Terminal attempts are immutable. A retry is a new linked attempt, and a writer
is never replayed over its edits. A terminal attempt has at most one successor.
- Why: continuation must preserve both history and its attempt's custody.

## Graph naming

A named launch treats its name as a handle for the launch input. The same
input replays the existing graph and starts nothing. Changed input under that
name starts the next free `NAME-k` (`NAME-2`, `NAME-3`, ...) and leaves earlier
graphs alone; an unnamed launch takes the next `BASE-k` from 1 and never
replays. A name is one lowercase letter, then lowercase letters, digits and
single hyphens, ending in a letter or digit. A grammar refusal suggests the
normalised form, so `--name Foo-` is refused with `try \`foo\``.
- Why: a reused name must not silently attach a different brief to old work.

A suffixed launch says so in its first output line:
`started NAME-2 (NAME exists with a different input)`. The `--json` receipt
carries the prior handle as `named.superseded` alongside `named.graph`.
`wait NAME` and `status NAME` keep reading the older graph, but warn that it
was superseded and print the newer handle as the next step
(`next: provenant wait NAME-2`). They never redirect silently.
- Why: the operator must see the new handle without losing access to the old graph.

## Writer binding

`--branch B --base REF` allocates a managed writer worktree under the project
layout, not under the invoking checkout. The launch owner resolves and
checkpoints `REF` before any Git effect, then creates the worktree from that
commit and binds it by source identity and head. This is identical from the
primary checkout, a linked worktree, a separate Git directory or a bare linked
checkout: the project's saved primary layout owns the allocation in every case.
An existing clean worktree at the exact checkpointed head is rebound; a
different branch, head or unclean tree is refused with the named cause.
- Why: an owned writer must land in one project-owned location regardless of
  where the launch command ran, and a failed bind must not leave a half-made
  worktree.

Every writer lane's prompt, for every adapter, opens its host preamble with one
`Lane location` paragraph built once by `lane_preamble_with`. It states the
absolute bound worktree path, the bound branch and its base commit, and that the
primary checkout (also an absolute path, when the project has one) is not the
lane's working copy: run commands, edit, branch and commit only in the bound
worktree. The paragraph is advice, never a fence, and it says an absolute path
elsewhere in the lane's context does not override it. Launch also records the
primary checkout's branch and ref tips (`primary-checkout.json` beside the
attempt) so settlement can detect stray work (see run-settlement, Stray lane work).
The lane receives `PROVENANT_ROUTE`, `PROVENANT_MODEL`, `PROVENANT_EFFORT` and
`PROVENANT_ACCOUNT`. Its preamble describes its own route as
`Route: adapter/model@effort, account <label> (source <s>)`.
- Why: opencode's system prompt names the Git common directory's project, the
  primary checkout, as the workspace root of a linked worktree. A brief that said
  only "your worktree" lost to that absolute path, and Hy 4 lanes edited,
  branched and committed in the primary checkout while the bound worktree stayed
  at base (#3611).

A node whose launch condition (`when`) does not hold never runs, so it needs no
writer scope: submission neither inspects nor preflights its worktree, and a
launch whose only writer is skipped runs from a primary checkout wherever a
read-only lane runs. A composite template takes no launch control: `--access`,
`--worktree`, `--branch`, `--base` and `--write-scope` are refused naming the
first one given. The bundled `smoke` template is the exception for `--worktree`
and `--branch`/`--base`, which bind to its write node only and enable it. Without
either, `smoke` runs its read-only nodes and its launch receipt warns which node it
skipped and why. Remote placement takes neither control.
- Why: a verification graph must run on a real install, and a skipped node
  must not be able to refuse the run it does not take part in.

Every refusal of a launch input names the field that caused it, or the decoded
path of a document or patch field. An unnamed `invalid input` is a defect; the
graph, peer, owner and unified paths build their refusals with
`FacadeError::field`, and a test pins that none of them returns an empty one.
- Why: the repair step says to check the named field.

A refused bind keeps its typed cause all the way to the facade. A held writer
lease is `writer_lease_held` and names its holder locally; scope, confinement
and resource failures keep their own codes and repair actions instead of
reporting `confinement_refused` with no resource. See
`docs/specs/cli.md#run-refusals-keep-their-own-code-at-the-facade`.
- Why: the operator needs the real cause to repair a launch.

## Policy and confinement

Launch policy refuses only at the [hard boundaries](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md).
Other checks allow or warn unless configured otherwise. A write-scope refusal happens before publication and retains private
work. The lane instructions say the same: a write scope lists the expected files,
so a writer edits another file when the fix needs it and lists it in its
handoff; only a strict `WRITE_SCOPE` policy refuses it.
- Why: advisory checks must not become accidental authority boundaries.

Policy rows live in `crates/run/assets/policy.toml`. Non-boundary rules default
to warning or off; a rule with a meaningful stop path may offer an explicit
`refuse` setting, which produces a configured stop. `Policy::warn_or` reports the source and knob, while immutable
boundary refusals use `boundary_error`. Heuristic scanners cannot offer a stop.
The `policy.<CODE>` knobs are documented in `config/harness-knobs.json`.

Every provider lane has its native tool and shell surface. Read-only mode
constrains filesystem writes; it does not remove shell, editing or web tools
from the provider. The common confinement planner owns worktree writes, reads
of the primary and sibling checkouts, explicit read roots, isolated-mode
credential denials and network reach. Network defaults to open, and explicit network reach is
applied by that same planner for every adapter. Every launch finalises the
host Cargo home and lane target binding through the common environment path.
Private harness instruction files receive the canonical global rules compiled
into the binary, so source checkouts and packaged releases use identical content
without depending on the installed asset layout. Native integration differences
belong to `AdapterSpec`, with a reason and source for an unavailable capability.
- Why: provider selection must not silently narrow a lane's working surface.

Credential-root classification identifies the most specific declared store
and its source. Under `lanes.access=isolated`, a declared store or exact
credential file that overlaps a lane root, and a store that cannot be resolved
precisely, keeps its existing preparation refusal. Under `host`, the same
overlap or unresolved store warns with the paths, effective access and source,
then preparation continues. Credential-overlapping grants and discovered
tool paths follow that access choice as well: isolated keeps its refusals and
omissions; host accepts the grant and keeps the discovered path. In a host
profile, credential, keychain/security and process inspection, signal and
task-port denies are absent. Sandboxing
continues to enforce its write roots, network policy and explicit denies. The
isolated profile retains its credential write restrictions, including the
protection against replacing credential files. `LaneAccessGrant` retains the
input spelling for a credential-refusal diagnostic; the canonical grant remains
the only path used for authority checks. Route selection precedes this
credential decision so a host-access fallback can still proceed.
- Why: same-host credential classification must not become a launch boundary.

### Lane access

`LaneAccess` resolves `lanes.access` to `host` by default or `isolated`. The
host model specifies that a same-host lane has the host environment, provider
home, profile and login, Git configuration and credential reads, whether
sandboxed or not. This slice applies that model to credential roots, keychain
and host-process access: host-mode sandboxing still confines writes, network and explicit
denies, while credential, keychain/security and process inspection, signal and
task-port denials are lifted. An
overlapping or unresolved credential root warns and proceeds in host mode;
isolated retains the existing refusals. Each warning names the effective
access and its source. Cross-host credential grants are unchanged. Remaining
#3712 slices apply the model to environment, provider-home and Git behavior.
`isolated` retains today's restrictions, including the private sandboxed
environment and provider home, filtered Git configuration, credential denies,
and hosted-cloud credential-variable stripping.

`resolve_lane_access` gives an explicit launch, template or tier knob priority
over route policy, then uses the host or project knob, then the default. The
applied route receipt records the value, source and deciding key. Invalid knob
text warns and resolves to host. The real boundaries remain other lanes' refs,
commits and uncommitted work; data integrity; merge, release and ruleset
authority; and credentials crossing to another host or an ungranted provider.
- Why: sandbox selection must not silently select several unrelated restrictions.

### Lane state stays outside the worktree

`finalise_provider_env` rebinds `HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, `XDG_DATA_HOME` and `XDG_STATE_HOME` into the attempt scratch (`scratch/home` and `scratch/xdg/<name>`) whenever the final value is the lane's worktree or lies under it, compared lexically and canonically when the path exists; it creates each rebound directory mode 0700, records one warning naming the variable, old path, new path and reason, and leaves values outside the worktree unchanged.
- Why: a lane that resolves tool state under a HOME inside its own checkout can commit that state with its work; the rebound happens in the one place every adapter finishes rather than per tool.

The selected sandbox floor applies to each candidate and is sealed for
continuations. Unsupported confinement is reported rather than represented as
sandboxed execution.
- Why: a retry must not silently weaken the requested boundary.

A lane sees the host's Git configuration as the host does. Its Git wrapper no
longer forces `GIT_CONFIG_NOSYSTEM`, and launch no longer drops
`GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_*`, `GIT_CONFIG_VALUE_*` or
`GIT_CONFIG_PARAMETERS`. The lane's own token helper entries are appended on
top of that configuration, with the empty `credential.helper` resetting the
helper list so a GitHub push keeps using the lane's token. A lane that is not
sandboxed keeps the host's own Git environment.
- Why: lanes are free to see the host's Git settings, not a filtered subset
  ([ADR 0035](../adr/0035-three-hard-boundaries-and-flexible-everything-else.md)).

Read-only write exceptions come from user-owned configuration, remain within
verified repository paths and are recorded on the attempt. Git and provider
control paths are excluded.
- Why: only the user may widen a read-only lane's write authority.

Host-native launches retain confinement and provenance and cannot resume a
provider session or select model-specific controls.
- Why: native execution must not claim a provider contract it cannot satisfy.

Every lane's private global Git configuration includes the host's global files
in full: the XDG `git/config`, then `~/.gitconfig`, or the host's
`GIT_CONFIG_GLOBAL` alone. A sandboxed lane also receives an exact read grant
on each of those files, named by the same canonical path Git's include uses,
so the include resolves under the confinement boundary. Both Git's include and
the grant name the physical file, never a symlink spelling. The private
configuration sets `user.useConfigOnly` so a missing host identity cannot fall
back to a generated hostname identity. No host entry is filtered, copied or
rewritten, and the launch receipt carries no Git-config filtering field.
- Why: a lane sees the host Git configuration in full, and the read grant is
  what lets an included host file be loaded inside the sandbox.

## Inputs and resources

Unreadable required host inputs reject an attempt before launch. A pinned source
view reports its failure without exposing captured contents or credentials.
- Why: a partial source view must not be mistaken for the requested input.

Lane download stores do not copy the host registry, and fresh provider homes do
not carry provider history.
- Why: shared build inputs must not disclose host or provider state.

Build output is collected only when its Cargo locks are free and recent writes
are protected. Priority adjustment failure does not refuse launch.
- Why: resource management must not corrupt active work or prevent a launch.
