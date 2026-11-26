# Run launch and admission

Owner: `crates/run`, `crates/confinement`, `crates/providers`
Anchor: `crates/run/src/launch.rs`, `crates/run/src/admission.rs`

Paths are relative to `tools/front-door/`.

## Admission and custody

Admission commits the reservation and attempt together. A worktree's owning lane decides who
else writes in it; active lanes share an occupancy lease. A launch that finds `writer.lock` held
checks the slot's reservations: a scoped stop intent for the inode-bound attempt or a reservation
in the settling phase is a release already under way, so the launch waits on the store lock budget
(the same sourced SQLite busy timeout that bounds the store's own locks) and wakes as soon as the
lock is free. A live holder or any other occupancy without that evidence still refuses on the first
probe, so a release wait never becomes a queue behind a foreign lane. Resource probes run outside
the admission transaction, and busy capacity is not a
queue. Uncertain custody keeps its ownership until release is proved, except for a terminal
read-only review: it holds no Git work, so a reservation left by its guardian is not uncertainty
and reconciliation settles it with a typed warning ([outcome before release](run-settlement.md#outcome-before-release)).
Reconciliation reports released custody only after exclusion and native
absence evidence prove it safe; an unknown or live holder retains custody, and uncertain custody
and unreadable reservations warn while reconciliation continues. An attempt whose stored record no
longer decodes is not held merely because the decode failed: once writer exclusion is acquired and
the recorded holder is proved gone with no settling exit, reconciliation preserves the bytes under
`quarantine/` and releases the lease with a typed warning naming the source; `release attempt:ID`
offers the same safety valve. Admission refusals keep fixed text and never carry a record path.
Writer-bind refusals identify
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
settlement's expected-old-tip compare-and-swap. A divergent sibling publication is merged into
the host branch by the host when no conflict exists, and published. A rewrite replaces the
unchanged custody-pinned launch tip with a warning and a recovery ref; a later host tip takes the
merge path regardless of remote-tracking refs. A real conflict, or
`lanes.history.rebase = "refuse"`, retains the loser's private commits and index; the refusal
names the setting and the conflicting files. A continuation that needs a predecessor commit
launches after that predecessor settles. Scope attribution starts at the attempt's captured
launch tip, and budget commits publish an owned-path tree into the attempt's own ref, never a
later observation of shared HEAD.
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
on every host. Admission and provisioning precede that boundary, so a graph may be admitted and
its planned worktrees created on such a host; the platform refusal is neither an admission nor a
route refusal.
- Why: a launch that cannot start must not look like a lost lane, and a missing custody backend
  must not hide whether the graph itself was admissible.

Permanent pre-admission refusals are durable outcomes of their operation: replay returns the
refusal without preparing again, and a failure to store it is not acknowledged as success.
- Why: a lost response must not turn a refusal into another launch.

Every refusal path in lane launch carries a typed terminal cause plus the command that fixes it,
recorded in `terminal-pending.json` and shown by `result` and `wait`. A refusal whose record
names none reads `unrecorded`, never a cause the host did not observe. A pre-launch routing refusal
keeps a bounded host-authored explanation; provider diagnostics never supply it.

A refusal while preparing the local launch (for example Git setup with no declared or inferable
remote) is `preparation_refused` with a typed `git_setup` preparation failure, never a lost or
refused handoff to a guardian. Its note carries the host's bounded reason and the remedy that
reason implies — declare `ports.git.remote` or set the branch's upstream — into `result.md` and
`provenant status`. Only a real host handoff refusal reads `launch_handoff_refused` and tells the
operator to run `provenant reconcile`. Remote inference still never guesses a remote.
- Why: the remedy must repair the step that refused; sending an operator to reconcile a Git
  setup failure hides the actual cause and strands the lane.
- A writer-lineage check at launch is the same local preparation step: its
  refusal is a typed `preparation_refused` outcome whose note names the
  retained worktree's owner, source and head to check before retrying, never an
  "invalid run record" or a generic `provenant reconcile` instruction. An
  unfinished predecessor still refuses at the authority boundary, because no
  successor may take its worktree. A new graph can adopt a retained writer
  worktree whose owner, terminal record and released custody match: it inherits
  the worktree and its commits, not the earlier graph's source and head pins.
  - Why: a repair must name the step that refused, and adoption must not be
    refused for commits the writer made after an earlier graph sealed its head.
  - Owner: `crates/run/src/launch/hand_off.rs`, `crates/run/src/controls.rs`,
    `crates/records/src/run.rs`

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
a primary checkout, a linked worktree, a separate Git directory or a bare linked checkout. During
provisioning and recovery, an existing clean worktree at the exact checkpointed head, including
one a chair pre-created with `provenant worktree create`, is rebound; a different branch, head or
unclean tree is refused with the named cause. When the managed path already holds a registered
linked checkout on the named branch, `--branch` and the branch resolved by `run implement --pr N`
bind that checkout in place as `--worktree` would, and report its path on an info line rather than
provisioning it again. Reuse takes the checkout's own head: for an existing branch `--base` is the
merge target for diffs, stray-work accounting and the PR, never an ancestry requirement or a reset.
A branch whose base is in its history is reused without a behind-base warning; a branch behind its
base is reused with `BRANCH_BEHIND_BASE`, naming how many commits it is behind and that the lane may
merge the base. A base with no merge base at all is unrelated and refuses with a `WRITER_SCOPE`
message that names `--worktree` and the merge/rebase option; an incomplete history from a shallow
clone warns and proceeds instead of refusing on absent evidence. An unheld
dirty checkout follows `lanes.writer_dirty` (`wip_commit` by default), not the provisioning
refusal. A live writer or an unsettled attempt on that checkout still refuses, naming the holder
and the exact command that settles it (or a wait-for-holder step when no attempt is named); that
refusal protects another lane's uncommitted work and never advises committing or preserving it.
The target grammar never accepts a raw attempt id, so a holder resolved to its work key is advised
as `provenant reconcile <graph>/<node>#<attempt>`; where no store can resolve the key, the remedy
names the attempt separately and advises the bare `provenant reconcile` sweep. One shared formatter
owns all four emissions (submission preflight, facade `writer_busy` and `writer_lease_held`, and the
run diagnostic) so the copies cannot drift. Relative `--worktree` and graph writer paths resolve
from the project's primary root, so a launch from a linked worktree of the same repository selects the same managed
checkouts as the primary checkout. A host-planned writer (`provision_worktree` with no attempt
`new_worktree`) is skipped by the pre-attempt integration-target check: it has no branch until
provisioning, and provisioning verifies the exact branch and head.
- Why: an owned writer must land in one project-owned location wherever the launch ran, and a
  failed bind must not leave a half-made worktree.

A writer node that reuses an existing worktree declares its comparison `base`. When none is
declared, the host resolves one from the worktree branch's upstream, else from its merge base
with the default branch; `run --explain` reports each chosen commit and its source. The base is
pinned only to review panels sourcing that writer, so each panel compares its writer's base
against its pushed head without turning one writer's inferred base into a graph-wide fallback.
A node-sourced review panel with no resolved base and no authored review base refuses the graph
before launch, naming the writer and the remedy; a plain writer without a base remains advisory
and falls back to the caller checkout.
- Why: without a pinned base every lens reviews an empty range and reports equal commits, which
  hides the missing Git evidence instead of naming it once before any lane starts.

`run --explain` runs the same pre-attempt admission checks as a launch, in read-only form:
graph exclusions, the submission snapshot, the refusal of host-authored writer-preflight
fields, protected delivery targets and every declared writer branch, read from an existing
checkout or from the node's `new_worktree` declaration. It allocates no graph execution,
attempt record or writer checkout, and its `source_admission` rows report a proposed worktree
without registering it. The dirty-worktree preflight and its WIP commit stay launch-only:
they inspect and may write each existing writer checkout, so explain neither runs nor previews
them. An explanation that passes therefore passes the same admission gates, and the launch can
still refuse at its dirty-writer preflight before proceeding to writer provisioning and
provider preparation.
- Why: an explanation must not pass where the launch's own admission would refuse it, and it
  must not write or claim a result it never observed.

Every writer lane's prompt opens its host preamble with one `Lane location` paragraph built once
by `lane_preamble_with`: the absolute bound worktree path, branch and base commit, and that the
primary checkout is not the lane's working copy. It is advice, never a fence, and it says an
absolute path elsewhere in the lane's context does not override it. Launch records the primary
checkout's branch and ref tips (`primary-checkout.json`) so settlement can detect
[stray work](run-settlement.md#stray-lane-work). The lane receives `PROVENANT_ROUTE`,
`PROVENANT_MODEL`, `PROVENANT_EFFORT` and `PROVENANT_ACCOUNT`.
- Why: a provider's system prompt may name the primary checkout as the workspace root of a linked
  worktree, and a brief saying only "your worktree" loses to that absolute path.

### Layered brief preamble

Standing lane rules come from `lanes.brief_preamble`: a file path set at the host layer
(`<state>/config.toml`), the project layer (`.provenant/config.toml`) and the launch layer
(`--preamble-file`). The layers concatenate in that order, each labelled with its source. A
file whose first line is a `<!-- provenant-roles: ... -->` header applies only to the roles it
names: writer, review or read-only. A missing or unreadable file warns with its layer, and the
launch proceeds without it. A writer's preamble also lists the project's declared no-build
Declared no-build checks from the port table appear under `Static checks:` in writer
preambles, to run before pushing. Readers do not get the list.
- A writer with valid `verify.lane` entries receives those tiers, commands, scopes and source as
  its authoritative verification guidance; the legacy format-only scope does not also apply. The
  launch receipt records the same effective entries and source. With no valid entries, the
  resolved `lanes.verification` scope or formatter and hooks default remains in force.
Project no-build checks are declared as `verify.static_checks` entries in `.provenant/ports.json` (or the host port
table): each has an `argv`, a repository-relative `cwd` and a `timeout_seconds` cap. An absent
or empty list adds no text and invents no tier for other stacks; an invalid entry warns and is
dropped rather than refusing the launch. The list is labelled with the layer and exact file and
key that declared it. Host and bound host-project declarations name their resolved ports file;
project declarations use the stable `.provenant/ports.json` path. That same list is recorded as
`static_checks` on the launch receipt beside `ci_scope` and `pr_policy`; each surviving check
entry carries that layer and file/key source. Config merging replaces arrays wholesale, so entries
in the effective list share one source. A failure is reported in the handoff and does not refuse
the push; CI still gates the merge.
Readers get neither the static checks list nor its receipt.
- Why: standing rules belong in one owned file per layer, not repeated in every brief, and a
  writer should run the cheap checks the project declares before CI does.

A node whose launch condition (`when`) does not hold never runs, so it needs no writer scope:
submission neither inspects nor preflights its worktree. A composite template takes no launch
control (access mode, worktree, branch, base or write scope) and the first one given is named in
the refusal; the bundled `smoke` template is the exception for worktree and branch/base, which
bind to its write node only. Remote placement takes no caller path: a caller `worktree` is refused,
but `--branch` with an optional `--base` is carried as peer-owned worktree intent so the peer
creates the writer in its own clone. There is no default remote provisioning: a remote write
node needs an explicit `--branch` (and optional `--base`) to own that worktree; without one the
write node is skipped with a repair warning. The sender's graph does not bind or provision a
local worktree for that writer; its checkpoint retains the peer provisioning intent for replay.

`run smoke` and `upgrade --verify` share one hermetic lifecycle. It creates a deterministic
fixture repository under `domains.smoke.fixture_root` (default `.local/state/provenant-smoke`,
home-relative) in the home of every target host. A fresh six-byte hex nonce names the fixture;
fixed author, committer and 2000-01-01 timestamps make the same nonce yield the same OIDs on any
host. A `.git/provenant-smoke.json` marker records the nonce, creation time and `FIXTURE_RECIPE`;
the sweep removes only expired marked fixtures and warns `SMOKE_FIXTURE_FOREIGN` for an unmarked
directory. The writer commits in the fixture and the review covers that commit. The verdict
counts only when the nonce appears in both the read and review result summaries; a row that loses
its nonce fails with the row named, so a false pass cannot masquerade as a result. `--project` on
`run smoke` is ignored with `SMOKE_PROJECT_IGNORED`, because a caller project would reintroduce
the checkout leak. `diff_base` defaults to the fixture head and any caller value resolves inside
the fixture; an unresolvable value is refused with `INVALID (field diff_base)`. A peer below
`SMOKE_FIXTURE_PROTOCOL` cannot verify the fixture and yields `SmokeVerdict::Unverified`, never a
pass; `domains.smoke.unverified` chooses warn (exit 0) or fail. The result reports five phases
(fixture, launch, wait, status, cleanup) and the effective values and sources of the
`domains.smoke` keys (`fixture_root`, `sweep_after_hours`, `branch_prefix`, `unverified` and
`set.{parameter}`). Afterwards the lifecycle forgets the fixture's binding and removes it on
success, failure and refused launch.
- Why: a verification graph must run on a real install and exercise a writer against evidence
  only it produced, and a skipped node must not refuse the run it does not take part in. A caller
  checkout, a caller project or an old peer must never turn a failed smoke into a pass.

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
Policy is dispatch state: a request seals its knobs, overrides and project key,
not a resolved policy table. Each launch and continuation resolves the host,
project and dispatch layers afresh, so a project policy change applies to the
next continuation while immutable boundary refusals never depend on a snapshot.
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

### Lane pull requests

`lanes.pr` resolves to `never` (the default), `draft` or `allowed`, and every
effective value names the layer that set it. The default is `never`, so a leaf
implement writer lane in a graph does not open a pull request; the chair owns
publication after settlement. Host, launch and node settings opt a lane in.
Set `attempt.pr = "draft"` or `"allowed"` on a graph node, or pass
`--set attempt.pr=draft` at launch; the node override is reported as the launch
source and takes precedence over the host setting.
The provider's `gh` tool enforces the policy before forwarding a command,
including when `lanes.publish=false`. Publishing controls credential delivery;
an unsandboxed lane can still access ambient host credentials.
`never` refuses `gh pr create|edit|close` (and the other PR mutations) and
mutating forge API calls while leaving reads working. Repository and hostname
selectors at command boundaries do not change the policy. Explicit GET or HEAD
requests with query fields
remain reads. Fields without an explicit method imply POST and are
refused; `draft` adds `--draft`
to `pr create`, permits edit, close, comment and review, and refuses promotion,
merge, reopen, lock, unlock, update-branch, revert and forge writes. Unknown PR
subcommands are refused under both restrictive policies until classified;
`allowed` forwards unchanged. A missing or unknown value falls back to `never`.
The effective value and source appear in the writer preamble, the launch
receipt and `run --explain`. This is a cooperative policy mediating ordinary
`gh` use, not a sandbox against a lane deliberately evading it: a lane that runs
the real `gh` binary or another API
client directly is outside it, and host-side delivery is unaffected.
- Why: a leaf implement lane must not open a PR against the wrong base, and a
  refusal must name the setting that caused it.
- Owner: `crates/config/src/pr_policy.rs`, `crates/providers/src/lane.rs`,
  `crates/run/src/completion.rs`, `cli/src/run/provider/preparation.rs`

### Lane scratch stays outside the attempt record

A lane's temporary directory is `<state>/attempt-scratch/<attempt id>/tmp`, and
`LaunchPaths::scratch` is its `scratch` child. Both are outside `attempts/<id>`, as
are the temporary alias and its target. The lane's `provider-state`, progress handoff,
prompt and durable output paths remain in the attempt directory. Retention, disk
classification, writer-Git readers and terminal cleanup follow the sibling root; the
alias is unlinked with its target. There is no fallback to the old scratch paths.
- Why: an incidental relative write from lane scratch must not land in the attempt's
  settlement records (#3844, #3947).

### Lane state stays outside the worktree

`finalise_provider_env` rebinds `HOME` and the `XDG_*` homes into the lane's per-attempt
scratch (`scratch/home`, `scratch/xdg/<name>`, mode 0700) whenever the final value is the worktree or
under it, compared lexically and canonically, and records one warning naming the variable, old
path, new path and reason. Values outside the worktree are unchanged.
- Why: a lane whose tool state resolves inside its checkout can commit that state with its work,
  and the one place every adapter finishes is where it can be prevented.

The selected sandbox floor applies to each candidate and is sealed for continuations.
`SandboxMode::resolve_backend` refuses an unavailable backend under `isolated`; `host` warns,
proceeds, and records `effective=false` and the reason in the sandbox receipt.
- Why: a retry must disclose when requested confinement was unavailable.

## Inputs and resources

The tree a read-only lane reads is selected by `lanes.read_source`, resolved by
`resolve_read_source`. `auto` pins the fetched upstream when the checkout is behind,
and otherwise pins checkout HEAD. `live` reads the live tree locally; on a peer it
pins HEAD and warns `READ_SOURCE_PINNED_FOR_PEER`. `default_branch` pins the fetched
default branch; any other value is a Git ref. `--source` accepts the same values
and overrides configuration. `lanes.read_source_fetch_first` fetches before resolution.
`bind_read_source` is the one binder: it pins each agent leaf, never a gate or composite, to
the writer base when one resolved and otherwise to the pinned read source. A
repository file still naming `checkout` warns `READ_SOURCE_VALUE_RENAMED` and resolves as `auto`;
it never acts as the live choice. A writer launch instead resolves one base by role before
sealing its identity: explicit `--base`; else the `--branch` default; else the HEAD of a named
`--worktree` or a composite bound with
`--set worktree=PATH`; else none, with an advisory warning that its identity falls back to the
caller checkout. The writer's own nodes, its launch identity and the stale-checkout warning skip
the read-only default; its read-only members (a plan node, or a composite's review and gate nodes)
read the writer base when one is known and the read-only default otherwise. A review range and an
authored document keep resolving their own subject. Writer-ness is the resolved graph's roles — a
launch is a writer when a node that will run writes a worktree — not one launch input. The warning
names the ref, the commit and the layer that chose it; `read_source` on the launch result and the
explain status names the same commit. Absent or unusable Git evidence stays advisory and never
refuses the launch. The `lanes.writer_base` knob chooses that default: `default_branch` resolves
the remote the read-source resolver finds (never a hard-coded `origin`), `checkout` pins the
caller HEAD at admission, and any other value is a Git ref; provisioning creates the branch from
the admitted commit. Explicit `--base` always wins; an
unresolvable value warns and falls back to the caller checkout, naming the key and its source.
The run result reports `writer_base` with its commit and source.
- Why: file:line evidence from a stale working tree reports findings upstream has fixed, and a
  plan should read the base it will implement rather than the caller's checkout.

A review launch pinned to a pull-request head binds that head as its node source. The host materialises
an attempt-private, read-only detached view of the commit and launches the review lane with that view
as its working directory. The view carries a named ref, `refs/provenant/review/base`, at the
host-supplied base, even when the source checkout has no remote default ref. The lane's prompt names
that base and head, states that both are authoritative, and forbids fetching, confirming either
commit against a remote default ref, or checking out, switching or resetting any other checkout,
including the project's primary; a pinned review never needs to touch the primary checkout.
- Why: a host-access lane can still reach the primary, so a model told only a SHA checks it out there
  itself, leaving the primary detached and displacing concurrent writers (#3943). A detached view has
  no `origin/<default>` ref and the lane cannot fetch, so a reviewer asked to confirm the host's merge
  base goes `inconclusive` unless the base is shipped and declared authoritative (#3977).

Unreadable required host inputs reject an attempt before launch, and a pinned source view reports
its failure without exposing captured contents or credentials.
- Why: a partial source view must not be mistaken for the requested input.

Failing to adjust a build's priority never refuses a launch.
