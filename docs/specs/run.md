# Execution: runs

Owner: `crates/run`, `crates/records`, `crates/fabric-runtime`
Anchor: `crates/run/src/lib.rs`, `crates/records/src/stop.rs`

Run admits attempts and records their outcomes. [Launch](run-launch.md),
[settlement](run-settlement.md), [Git and delivery](run-git.md) and
[attribution](run-attribution.md) specify their boundaries. Paths are relative
to `tools/front-door/`.
The handoff parsing rule lives in [settlement](run-settlement.md): a closed
`handoff` fence wins, a final `json` fence is accepted with a warning, and
adjacent repeated blocks settle as one declaration (identical, or the last of
differing copies), each with a warning. A writer that exits without a handoff
keeps its uncommitted paths and pinned head in the reason and sealed terminal,
so the `handoff_missing` stop routes to the chair as a continuable stop.

## Operation IDs

The same operation ID and request digest replay the recorded outcome without
repeating effects; a different digest conflicts. Named graphs replay matching
inputs across the name family and allocate a new name for different inputs.
Collected names remain reserved. Each attempt has a unique work key, including
its graph-assigned attempt number.
- Why: lost replies and concurrent requests must not duplicate work or identity.

## Host Cargo home

Every lane uses the host-resolved `CARGO_HOME` (the host process's environment,
otherwise its `HOME/.cargo`), preserving the absolute source path Cargo fingerprints.
Adapter-selected homes and launching-shell values cannot replace that binding.
The launch's `cargo_home` diagnostic names both the path source and the effective
`domains.fabric.lanes.cargo_source_writes` setting source.

Lanes share the host's registry index, cache and sources and Git databases and
checkouts, and may write them by default, so a lane fetches what it needs once for
every later build. Cargo's package-cache locks and global-cache database (including
its rollback journal) are always writable. Cargo runs online whenever the lane has
open network access. `cargo_source_writes = false` makes the source stores
read-only and runs Cargo offline; a missing dependency then fails, and the Cargo
shim names the knob, its source and suggests fetching on the host. Cargo credentials (`credentials` and
`credentials.toml`) remain mandatory read/write denials, including relocated homes.
Host `config` and `config.toml` are read-only data; target seeding checks them with
the same compiler-selection validation as checked-in Cargo configuration.
Build directories default to the worktree's own target in its lane cache
(`lanes.cargo_target = "private"`; attempt scratch for read-only lanes), or the
writer worktree's own `target/` when `project` is configured.
No separate lane download store, Cargo home or symlink setup exists.
The host's own build output supplies target seed generations.
- Why: Cargo fingerprints dependency source paths; changing the home path defeats reuse even when symlinks reach the same content.

## Cargo target directory

The `lanes.cargo_target` knob configures how lanes acquire their Cargo target:
- `private` (default): the worktree's own target in its lane cache, `~/Library/Caches/provenant-lane-cargo/<digest>/target` (lint-only runs in `target-lint`), seeded warm from the project's host seed generation (below). A clone shares blocks with the seed on APFS. Where the filesystem cannot clone, the target stays unseeded: the outcome is a `clone_failed` fallback, the host warns once and the first build is cold. A copy is never made, because it cannot finish inside the seed budget. Settlement starts a detached lane cleanup that removes the target once writer custody is free (`src/fabric/settlement.rs` → `gc_cleanup::release_settled_cache`); one it misses, worktree removal and lane-cache GC remove later. A continuation reuses a target still present and seeds a new one otherwise. `provenant worktree seed-target PATH` seeds the same directory for a checkout outside a lane and prints it for `CARGO_TARGET_DIR`; an existing target is kept. Outside a lane the command resolves the toolchain as cargo and rustup do, treating an unset or empty `RUSTUP_HOME` as `$HOME/.rustup` and `CARGO_HOME` as `$HOME/.cargo`, then verifies it and the Cargo configuration with the lane rules. An explicit `RUSTUP_HOME` elsewhere is refused. The command reports the source of the Cargo home it verified (`host CARGO_HOME` or `host HOME/.cargo`), because a shell's `CARGO_HOME` can differ from the host a lane runs under; it uses the same resolution as lanes. Every fallback outcome carries `expected` and `found` text for a mismatch (toolchain, host triple, Cargo profile, pin, environment class), free of paths and environment values, and the command prints a repair step.
- `project`: the writer worktree's own target, `target/` beside the Cargo workspace the verify port selects, with lint-only runs in `target/provenant-lint`. Builds by a person or chair in that checkout share it, and it goes with the worktree, but a new worktree starts cold. The host neither creates it nor grants it separately: it lies in the writer-controlled worktree, which the lane may already write and execute. Read-only lanes and projects without a Cargo manifest use the lane cache.
- Absolute path: uses the path directly. It comes only from host or project configuration or the launch knob; a `Cargo target:` line in a brief can pick `project` or `private` but never a path, because a path grants the lane read, write and exec there and a brief is requester text.

No mode shares one target between worktrees of a repository. The `shared` value, which gave every worktree one host target, is removed: `provenant config check` reports it and the default applies, and `provenant gc` retires its old directories under `~/Library/Caches/provenant-shared-cargo/` (a preview lists them; `--apply` removes each whose Cargo locks are free and that has no write within the build-directory grace).
- Why: Cargo hashes a workspace member's units by its workspace-relative path, so every worktree maps to the same units, and it decides their freshness by comparing source mtimes with the last build's dep-info. A worktree whose sources are older than another worktree's build reuses that build's artifacts and reports symbols missing that exist in its source (#3561). Content-hash freshness (`-Zchecksum-freshness`) would remove the mtime dependence but is nightly-only.
- Why the seed is the host generation, not a settled lane's target: generations already exist per project, are built by the host from the verify base (landed code) rather than from writer-controlled output, and record the compiler, host triple and Cargo profile a clone must match. A lane's target carries none of those guarantees and would need a second store and retention.

The preamble and `provenant config show` report the resolved target directory and its source, e.g. `(source: default; lanes.cargo_target)` or `(source: host; lanes.cargo_target)`.
Target seeding and clone machinery are used only for `private`. A lane's seed outcome is in `cargo-target-setup.json`; a fallback also warns once on the host's stderr.

## Cargo target seed

A target seed store is shared across registered repositories on the host. Each
generation records the verified canonical shared Git directory and a file-only
identity of its dependency inputs. Currentness and lane selection require the
same project key, identity hash and `deps` contents; the recorded commit is the
built-from record. The identity hashes Cargo.lock, tracked manifest dependency,
feature, patch and replace tables, workspace profile tables and applicable
`.cargo/config.toml` or extensionless `.cargo/config` profile tables (the extensionless file wins when both exist in one directory), rustc version and host triple, contents and
the effective Cargo build profile. Cargo-home configuration is host state and is not an identity input; profile settings reach lanes through the effective Cargo build profile. Manifest declarations are canonicalised, so
formatting and package metadata do not affect the hash. Edits that do not change
resolved units can still invalidate it. Both sides use Git's tracked file list;
no Cargo metadata runs during identity calculation, so lanes can calculate it
before confinement. Failure to calculate it leaves the lane cold with a warning.
A private lane that finds no matching generation atomically queues
`<state>/cargo-target-seeds/requests/<project_key>/seed-request.json` with
schema 1, project key, lane commit, identity, request time and attempt. The
newest request per project replaces an older one under the store lock. Tick
admits pending projects into its existing single-flight queue; a lane never
builds a seed. The host checks out the requested commit, recomputes its
commit-tree identity and checks the prepared checkout before publication. A
mismatch records `identity_mismatch` and publishes nothing. Each attempt writes
an atomic `seed-result.json` in the project request directory containing schema
1, the request, outcome and generation when built. A terminal result removes
only the matching identity and request time, so an older acknowledgement cannot
remove a replacement. Busy and failed requests remain pending with an incremented
attempt and retry through the queue's exponential backoff, capped at one hour.
The regular scheduled refresh still uses the verify base.
Compiler identity uses the selected commit's toolchain pin. A seed build is not a lane: the
compiler-selection keys the lane eligibility check lists (`RUSTC_WRAPPER`,
`RUSTUP_*`, `RUSTFLAGS`, `CARGO_BUILD_TARGET` and the other classes) are inherited
from whoever launched the build, so the seed build removes them from its Cargo
and rustc children rather than refusing. It keeps the values it sets itself
(`RUSTUP_AUTO_INSTALL=0` and the effective Cargo profile), warns once on stderr
with the cleared key names and never their values, and records those names in
`seed-result.json`. A cleared key is not recorded as a build flag. A lane whose
own environment changes the compiler still cannot use such a seed: the
eligibility check warns and leaves that lane cold.
`build.prepare` is an ordered list of argv arrays. After the verify base is
checked out and before the first Cargo command, every command runs in order with
the checkout root as its working directory (not the selected workspace
subdirectory), in the same sanitised child environment as the seed build's
Cargo commands plus `PROVENANT_SEED_ORIGIN` for seed-generation prepares.
The runner reads the live project and host port tables; the
repository only declares the command, nothing is added to it, and an unreadable
or invalid table warns and runs no prepare command. A failing command fails that
build: the project's outcome is `prepare_failed` with the command and the last
2 KiB of the combined output in `seed-result.json` and the refresh record, while
the operator note gives a repair and never echoes the output. Local redaction
never masks captured text: the stream is bounded at the tail window, and long
human causes keep the end and mark the omitted bytes.
The manual `provenant worktree seed-build` path shows that bounded output as an
indented cause in human output and in `error.cause` under `--json`, so the
operator need not reproduce the failed command to see why it failed. The same prepare
list runs before every seed generation build; the slot path runs it after
snapshot sync, as specified in [Build strategy](#build-strategy). Writer selection uses matching generations and Cargo's normal per-unit freshness checks before reusing artifacts.

Each prepare child also receives `PROVENANT_SEED_ORIGIN`, the absolute path of the origin checkout the standalone seed clone was made from. Provenant sets this marker only for seed-clone preparation; the verify-base Cargo path that sets `CARGO_TARGET_DIR`, and a caller's shell, never carry it. A consumer can therefore tell a seed clone from an ambient target directory and share tool caches with the origin checkout.

A generation holds dependency units only. After `cargo build`, `cargo test --no-run` and `cargo check --workspace --all-targets --locked`, the seed build enumerates the checkout with `cargo metadata --format-version 1 --locked` using the same child environment and target overrides, and runs `cargo clean -p` for every path package it finds (packages with no `source`): workspace members and non-member path packages alike. Registry and other non-path dependencies stay in the seed, including build and check-mode units that clippy can reuse. The manifest records this as `contents: "deps"`; older generations without the marker (or with another value) hold a full build. Those older full generations are not current: refresh builds a deps-only replacement for the same identity, and lane selection skips them with the stable `full_generation` fallback instead of reusing member units whose sources can move with a lane. Collection removes full generations as obsolete outside the keep-count and whatever the keep-count is; they never displace a deps generation from a retained slot, and the removal is reported with a distinguishing `obsolete_full_generation` reason.

Private writer lanes seed `target` and `target-lint` independently from the generation. Each clone gets half the effective clone budget and its own cache-size reservation; a fallback for either target does not prevent the other attempt. `cargo-target-setup.json` retains the main target outcome at the top level and records the lint outcome in a typed `lint_target` object with the same fields. When the whole lane is skipped, the same reason applies to both targets and the `lint_target` object is omitted.

A generation records the allocated size of its built target, measured once at seed-build time, so cloning need not walk the whole seed first. At launch the seed reservation carries that recorded allocated size to the collector, which resolves it against the byte unit its own census used: near-zero clone-private bytes on a reflink filesystem, the full allocated size where private size is unavailable. Because a lane clone reflinks the seed's blocks, under a private-byte census it privately owns almost nothing at clone time, so it reserves no budget and cannot evict an unrelated under-budget target for blocks it never privately consumes; under an allocated-byte census the recorded size is charged and an under-budget target near the limit is evicted to make room. A generation built before the size field existed records none, and the clone reserves nothing for it. Such a seed is accepted quietly rather than warning on every launch: its outcome records `size_recorded: false` in `cargo-target-setup.json`, and eviction accounting resumes once the seed is refreshed with a recorded size. A clone ages every
dep-info file to the epoch before it is published, so each unit that lists source files (workspace
members) rebuilds once against the worktree's sources while registry packages, which list none, stay
fresh. Cargo does not promise a build-directory layout (`.fingerprint/<unit>/` before Cargo 1.100,
`build/<package>/<hash>/fingerprint/` after) or which units keep dep-info, so the pass is driven by the
fingerprints themselves. One host knob `lanes.cargo_seed_budget` (default 5m, ceiling 1h) bounds each step: stage cleanup, generation selection and dep-info aging each start with a fresh deadline from the full budget, while each of the two lane clones gets half the budget when that step begins, so a slow step cannot consume another's time. In particular stage cleanup gets its own deadline that is not charged to selection, and dep-info aging starts its deadline only after the clone returns. A fallback whose budget ran out names the step and reports the effective setting with its source (host or compiled default) in `cargo-target-setup.json` and the warning. An unreadable host config warns with its path and uses the compiled default, reported as such. Within the clone's depth limit, its own budget and cancellation, it finds the
`*.json` fingerprints in `fingerprint` and `.fingerprint` directories, reads each one's `local` entries
(ignoring unknown fields) and ages exactly the files its `CheckDepInfo` entries name, relative to the target
root. A fingerprint that cannot be read, a named dep-info that is missing or not a path inside the target, or
a non-empty clone in which none is named is not published: the outcome is the `seed_layout_unrecognised`
fallback and the lane builds cold. A unit left un-aged would reintroduce #3561. A test builds with the pinned
Cargo and checks from its JSON output that a member recompiles and a registry-style dependency stays fresh. Published generations are
immutable, and collection cannot race an active clone. `max_generations` keeps
history per project; `max_bytes` is a host-wide allocated-byte collection target.
Collection always keeps each project's newest complete generation, even when
that generation alone exceeds the target; busy stores and active clones remain
protected. Full (non-`deps`) generations are collected as obsolete outside this
count, so they are reclaimed whatever the count is without displacing a deps
generation from a retained slot. Seeding is per project. Scheduled refresh resolves each project's verify base and records its outcome in the tick `seed-result.json`: `built`, `current`,
`busy`, `not_cargo`, `identity_mismatch`, `failed` with its cause, or `skipped` with a typed reason
(`verify_base_unset`, `verify_base_unresolved`, `workspace_absent_at_base`) and a repair step.
A skipped or failed project never stops or hides another project's seed, and a skip is
not a failed pass. Failed and busy projects retry with exponential backoff, capped at one hour;
these scoped retries run on a later tick without rebuilding the others: a busy retry replaces
only its own projects' outcomes and keeps every other record. Full requests supersede
queued busy retries and reevaluate every project. Detached pending and active scopes
survive process interruption and are retried after a minute. Pass-level failures
(an unreadable registry) are recorded beside the projects, and the number of project
records is bounded, with the omitted count kept.
`provenant tick` summarises the saved result as counts, for example `seed=ok:1 skipped:1`,
and adds a bounded note with the cause category and the repair step. The note does not
expose persisted command output, repository paths or environment values, and does not
change the tick outcome or retry schedule. Because seeding follows a complete or partial
collection and never a failed one, an interrupted collection is retried promptly (see
[scheduled collection](state.md#retention-keeps-evidence)) and seeding resumes with it.
Seed health is advisory. The last refresh outcome is persisted atomically at
`<state>/cargo-target-seeds/refresh/<project_key>.json` with a schema, an instant and
an outcome of `built`, `current`, `busy`, `failed`, `prepare_failed` or `skipped`;
a reason is bounded to 2 KiB. A writer lane with a private Cargo target and a
project that has a Cargo manifest warns once at start when no complete generation
exists, when the newest complete generation for the project is older than
`build.seed.stale_after` (the age comes from its `complete` marker), or when the
last refresh outcome is `failed` or `prepare_failed`. `provenant worktree doctor`
shows the newest generation and age, the last outcome and reason, the effective
`build.seed.stale_after` with its layer, and the same warnings. Every warning
names the last outcome, the knob with its effective value and source, and tells
the reader to run `provenant worktree seed-build` in the repository. A missing,
corrupt or newer-schema refresh file is treated as no record; a malformed knob
warns and falls back to 24h from `default`. None of these facts refuses a lane.
- Why: reuse must not introduce incompatible or partially collected build output.

## Publication order

Settlement imports and publishes private work before releasing writer custody;
the worktree lock remains held through both. A sealed terminal decision can be
replayed after publication contention without relaunching the provider.
- Why: a second writer must not race unfinished publication.

## A writer stops on a token budget

Natural completion takes precedence over budget action. A warning does not stop
work; a hard boundary stops it. Providers without live usage expose an advisory
limit rather than a fabricated measurement. The token-budget stop keeps its
existing safe commit of dirty work. A wall deadline and explicit cancellation
never create a commit: settlement records the verified head, commits ahead of
the launch-bound remote observation, dirty path count, and push result in the optional
`AttemptRecord.stop_work` summary. Explicit cancellation imports and publishes
only commits already present at the stop; dirty paths remain in the worktree.
The host pushes the lane's own branch only when the persisted `lanes.publish`
grant is enabled and the launch-bound host transport is available. A failed or
unavailable push leaves the committed head and unpushed count visible.

`status` and `result` report the budget's unit beside the provider's totals: the
unit name (`fresh_tokens`: uncached input, cache writes and output/reasoning,
with cache reads excluded because each call rereads the context), the limit's
value and the layer that set it, the fresh tokens counted for the attempt and
the provider's cumulative input including cache reads for comparison. Codex
totals are session-wide unless an attempt marks them per-attempt, so a counted
or reported figure can be absent; absence is never a guessed zero. An adapter
that cannot interrupt a turn names that caveat in the same receipt, and the
receipt is projected from the same limit and usage record enforcement reads,
so the two outputs cannot disagree about the unit.

| Record field | Reader compatibility |
| --- | --- |
| `AttemptRecord.stop_work` | Optional; older records read as absent, and older readers retain it as an unknown field. |
| `status.budget_usage`, `result.budget_usage` | Optional; older readers retain them as unknown fields. |

- Why: budget enforcement must preserve completed work and report its evidence; a budget that excludes cache reads must not look unenforced beside a larger provider-reported input.

## Build strategy

Lane admission, verification and landing dispatch build work through one
runtime in `crates/run/src/build/`. `build::run` is the only entry point; it
appends exactly one `JobOutcome` row for every job, on every path. The `build.*`
leaves are typed at read time: an unknown value warns with the leaf and its
source and falls back to the builtin value (source `default`), so one bad leaf
never refuses a port table. The strategy and its source are frozen in
`PROVENANT_BUILD_CONTEXT` at lane launch; outside a lane they are read live from
the port table.

### Strategies and the job model

`strategy` selects `slots`, `lane_seed`, `lane` or `command`. `lane_seed` and
`lane` execute locally; `lane_seed` also requests target seeding at launch, as
does `slots`. `command` prepends `build.command.argv` to the intercepted argv
and keeps the exit-code disposition of the removed `build_admission` backend.
`wrapper` is declared but unavailable: it warns `BUILD_STRATEGY_UNAVAILABLE`,
names the leaf and its source, and uses `lane_seed`. One cap serves every
strategy: `N = [admission.build].slots`, derived from cores and memory when
unset, `0` disabling admission.

A `Job` carries a project key, origin (`lane`, `chair`, `landing`,
`maintenance`), owner, kind (`intercepted`, `tier`, `gate`, `maintenance`),
priority, source, optional verify base, ordered steps, output mode, timeout and
the caller's environment. Every step runs in one hold, in order. A prelude runs
after materialisation and before the first step and returns extra environment.
`local.rs` owns process execution for every strategy: a child process group,
SIGINT/SIGTERM/SIGHUP forwarded to the group then SIGKILL ten seconds later, a
timeout, bounded capture or streamed output, and code or signal exits (a
signalled child yields 128 + signal). Nested calls detect a live ancestor slot
and run directly, without a ticket.

### Slot lifecycle and snapshot

Slots live under `~/Library/Caches/provenant-slots/<project_key>/<n>/`. Under
the held index `slot.lock` is held; custody intent is published in `job.json`
before spawn and upgraded to the child's process identity after spawn. The
`intent.lock` lease is inherited by the build's process tree, so an intent
without a running marker keeps the slot occupied until that lease is free; a
live process group named by a running `job.json` is killed (SIGTERM, five
seconds, SIGKILL) with a warning. The slot is cloned from
the store when absent (`--no-checkout`, `core.hooksPath=/dev/null`,
`gc.auto=0`, objects hardlinked, never shared alternates). `build.prepare` runs
when its argv digest differs, after the clock guard and snapshot sync and
before the first step, so a reset re-runs it. Sync fetches the snapshot and base
refs, checks out the snapshot with `--force --detach`, points
`refs/remotes/<remote_ref>` at the base so `git diff origin/main...HEAD` works,
and cleans by `build.clean`: `none`, `untracked` (`git clean -ffd`) or `ignored`
(`git clean -ffdx`). After the steps,
`git status --porcelain=v2 -z --untracked-files=all` fills `slot_changes`; one
stderr line reports a nonzero count as "not copied back" and nothing is copied
back. `slot.json`, `after_job` and lock release finish the hold.

A working-tree snapshot copies the lane index file to the slot's `tmp/`
(falling back to `read-tree HEAD`), runs `add -A`, `write-tree` and
`rev-parse HEAD^{commit}` against that copy with optional locks off and system
config off, then writes a commit with fixed author/committer and epoch date, so
the same tree and parent give the same commit. The objects go into the lane's
private object directory and reach the store by `git fetch` with
`allowAnySHA1InWant`, reading any `info/alternates`. The lane's index, refs,
worktree files and mtimes are never written. A `commit` source pins that commit
as-is. Under slots, `verify --commit` and a landing gate use this path
directly.

### Job environment, output and units

The job environment starts from the caller's and removes `PROVENANT_*`,
`GIT_*`, `PWD`/`OLDPWD`, the `CARGO_*_DIR` overrides, the lane-lint target var
and `TMPDIR`/`TMP`/`TEMP`. It sets `CARGO_TARGET_DIR` to the slot's `target/`,
`TMPDIR` to its `tmp/`, `HOME`, `CARGO_HOME`, `RUSTUP_HOME` when known, and the
host `PATH` without lane shim dirs, and leases a slot-local lane-port block
from the host plan (warn and set none when none is free). Caller `RUSTFLAGS`,
`RUSTC_WRAPPER` and `CARGO_PROFILE_*` are kept as real inputs. Stream output
maps the slot repo path back to the worktree component by component, line by
line, passing lines over 1 MiB through unchanged; target paths are real and
unmapped, and no `--message-format` flag is injected. Capture output is not
path-mapped. Unit counts come from a fingerprint census after the job:
directories modified after the start are rebuilt, their package names fill
`rebuilt_packages` (sorted, at most 256), and the rest are fresh. Peak RSS is
sampled every five seconds, null when unavailable.

A job runs locally under slots when it is sandboxed, its argv is not eligible,
`offload` is `off`, staging or the pool is unavailable, the local overflow
window ends, every slot is quarantined, or no snapshot exists. Each case
records its `exception` and warns; an all-quarantined pool names
`provenant build reset`. Unavailability warns and runs locally rather than
refusing a build.

### Queue

`build.priority` maps job classes to `interactive`, `test`, `train` and
`prewarm`, ordered from most to least urgent. A free slot is taken at once in
affinity order, then by ascending index; only a busy pool writes a ticket at
`<state>/build/queue/build/<id>.json` and holds an exclusive flock on it.
Waiters list tickets every 80 to 120 ms, reap those whose lock is free, and
order live tickets by effective level, then enqueue time and id. Effective
level falls by one per `build.queue_aging` the ticket has waited, so every
level's wait is bounded; priority orders waiters and never preempts a running
job. A ticket with the same owner, priority and command digest as a newer one
exits 75 (EX_TEMPFAIL); running jobs are never superseded. A signal while
waiting deletes the ticket and exits 128 + signal. With `overflow = auto`,
slots queue until admitted or cancelled and the lane strategies run locally
after `[admission.build].wait_seconds`; `queue` and `local` select those paths
explicitly. An unreadable slot directory is unavailability, not a refusal.
Waiting and held markers stay the status surface.

### Hygiene

After each job, only an infrastructure failure inside 24 hours advances the
reset ladder: first light, second full, third quarantine. Cancellation and
clock rollbacks do not count. A clock rollback performs a full reset before
reuse with infra kind `clock` and no ladder step. A light reset checks out HEAD
and cleans `-ffdx`; a full reset removes the slot's `target`, `repo` and `tmp`
and the caller re-clones. A slot Git-sync failure before the first step is
retried once after the reset; once output has streamed there is no retry.
`hygiene::gc` (through `gc_projects`) removes slots idle past each project's
`build.slot_idle` or at indices at or above N, skips locked slots, expires store
pins older than `build.snapshot_retention` and runs `git gc --prune=1.hour.ago`
under the store lock. It also rotates `jobs.jsonl` above 64 MiB into
`jobs-<yyyymmdd>.jsonl` and drops rotations older than 30 days. Tick calls it
after a completed collection, beside the seed pass, resolving idle and
retention per registered project.

`build maintain` runs one best-effort prewarm of
`cargo check --workspace --all-targets --locked` on the verify base in an idle
slot at Prewarm priority, after a non-blocking admission probe. It is skipped
with a warning when the project had no job in 24 hours, the sampled build load
is at or above `verify.load_per_cpu × cpus`, the hourly `build.prewarm_budget`
is spent, or no slot is idle. A slot whose `last_canary` is older than
`build.canary_every` runs a canary instead; a canary that exits non-zero on the
CI-green base gets a full reset and a `freshness_failure` warning on its ledger
row. A successful canary stamps `last_canary` in `slot.json`. Tick spawns one
detached `provenant build maintain --project P` per recently used registered
project after the collection; a scheduling failure warns and never fails the
tick.

### Ledger and report

Each `JobOutcome` is schema 1 and its field order is a persisted pin: job id,
project key, owner, origin, kind, priority, strategy, placement, exception,
slot, snapshot, previous snapshot, base, enqueue/start/finish timestamps,
wait and wall milliseconds, exit, timeouts and cancellation, reset, infra kind,
fresh and rebuilt unit counts, rebuilt packages, peak RSS, slot changes and
warnings. Rows append to `<state>/build/jobs.jsonl` under `jobs.lock`; when
host state is unwritable they go to the attempt's scratch ledger and the report
counts them missing. `build report` computes wait p50/p95 by priority
(nearest-rank), peak concurrent admitted jobs from the overlap of slot rows
(start/finish, bounded by N), peak RSS against physical memory, local
exceptions by reason and their fraction, churn, and freshness and recovery
failures. Churn compares each slot row's rebuilt packages with the affected set
from `provenant_select` between its previous and current snapshot, evaluated in
a temporary detached worktree of the build store and cached at
`<state>/build/report-cache/<prev>-<snap>.json`, plus the always-rebuild set:
the configured root package, packages whose build script has no `rerun-if`
directive, and Cargo's `build-script-build` fingerprint unit. A full selection
expects every workspace member. An uncached or failed selection leaves the row
unknown, and unknown rows cannot pass. It prints one PASS, FAIL or NOT YET line
per D1 criterion with thresholds from `build.soak.*`, with fewer than
`min_runs` lane rows or less than `min_days` of span giving NOT YET. A gap of
jobs that neither ran nor reported is not observable from rows, and the report
says so.

### Callers

A lane's unchanged shims call `provenant admit build -- <real> "$@"`; admit
builds an `intercepted` job from `PROVENANT_BUILD_CONTEXT` with the lane's Git
binding, one step, stream output and the attempt as owner. Nested checks, the
local-Cargo advisory, disk and memory trim and `CARGO_SOURCE_WRITES` keep their
behaviour. The chair uses the same path with `chair-<pid>` as owner.
`provenant verify [TIER] [--commit REV] [--affected]` builds one `tier` job
whose single step runs the verify engine; the builtin engine is the bundled
`verify-tier` script and the command backend substitutes `{tier}`. Selection
runs in the prelude and sets `PROVENANT_VERIFY_SELECTION`. The report carries
the snapshot commit, strategy, slot and exception; the runner contract's final
`{commit, tier, exit_status}` line is unchanged. A landing Commands gate builds
one `gate` job at `train` priority from the candidate commit, one `sh -c` step
per configured command, stopping on failure, with the gate deadline covering
admission. Each step's exit maps to the existing `CheckEvidence`, and evidence
binding is unchanged.

### Failure modes

| Failure | Detected by | Effect |
| --- | --- | --- |
| admit killed mid-job | `intent.lock` still held or live group in `job.json` at next sync | slot stays occupied until the lease frees; orphan group killed, then sync |
| waiter killed | free ticket flock | ticket and its lock reaped by any waiter |
| store fetch fails | git exit | `no_snapshot`, local with a warning |
| clone or prepare fails | git or prepare exit | `pool_unavailable`, local; creation retried |
| checkout or clean fails | git exit before spawn | infra `git`, reset ladder, one retry |
| ENOSPC or EIO in our fs ops | io error kind | infra `disk`, reset ladder |
| SEGV, BUS, ILL or ABRT in the child | wait status | infra `crash_signal`, ladder |
| clock rollback | `before_job` | full reset |
| OS purges Caches | missing slot directory | slot recreated with its record |
| store corrupt | fetch or connectivity error | `no_snapshot`, local; the next fetch recreates a missing store |
| ledger unwritable | append error | row goes to scratch and counts missing |

- Why: lanes, the chair and landing must get the same strategy, one outcome row
  and one host cap, and a pool problem must never fail a lane's `cargo check`.
- Owner: `crates/run/src/build/`, `src/admit.rs`, `src/verify.rs`,
  `src/pr/landing/gate.rs`

## Lane verification and build admission

A writer preamble's verification paragraph is `lanes.verification` (`check`, `targeted` or `full`). Unset, the sourced default is targeted: build in the worktree's own warm target (`lanes.cargo_target`), run cargo check, clippy on touched crates and targeted tests, and leave the workspace suite to CI. A brief line `Verification scope: targeted` (or `check` or `full`) overrides the knob and the paragraph names source `brief`. When the knob and that line disagree, the paragraph follows the brief and the stop cause names both scopes. Admission, slot and queue behaviour are specified in [Build strategy](#build-strategy).

Admission attribution has the same host scope as slot occupancy. Before confinement,
`configure_build_slots` registers each lane under `<state>/admission/<class>/lanes/`,
including its project, lane name and scratch marker location. The registry indexes
live holder and waiter markers through their held locks, so stale registrations or
markers left after a crash do not imply occupancy. `admission::host_holders` and
`host_waiters` combine registered lane markers with standalone markers, independent
of the querying project or its run store. Both ordinary status and lane wait reasons
name the holding project and lane; JSON retains their attribution. Registry
publication failures warn and allow the build to proceed. Lanes keep their existing
scratch write grant and read-only slot access; registration does not grant writes
to host state or other lanes' records.
- Why: a chair brief must be able to widen what a lane verifies, and parallel lanes must share one host cap instead of each taking the machine.

## Every limit names the layer that set it

`dispatch_profile` seals, for every effective knob not at its registry default,
the layer that set it: `launch` for a dispatch override, `host` for the host's
`[knobs]` table or its selected preset. `provenant_config::knob_source` is the
one reader; lane limits, CI scope, the gh budget and sandbox admission take
their reported source from it, so status JSON and the preamble show the true
layer.
- Why: a value reported under the wrong source sends the operator to the wrong
  place to change it.

## Provider config notices are warnings

A provider's own configuration-deprecation notice (a deprecated key in the
config the account home inherits, such as Codex's `[features].multi_agent`)
is advice for the user, not a failed provider turn. The adapter classifies it
at the decode site as a warning carrying the provider's text and the config
file it names; it never sets the stream's error flag or fails the lane. The
same notice on stderr is likewise held out of the attempt's provider-error
headline. Each distinct notice is shown once per attempt beside the applied
route, names `config.toml` when the message gives no fuller path and points at
`providers.codex.clean_config` as the way to stop inheriting stale keys.
- Why: a stale inherited key must be actionable for the user without being
  reported as a provider failure, and two copies of the same notice must not
  read as two errors.
- Owner: `crates/providers/src/codex.rs`, `crates/providers/src/stream.rs`,
  `crates/cli/src/run.rs`, `crates/cli/src/run/output.rs`

## Lane Git and import

Writers of one verified worktree incarnation share its files and hold
occupancy together. Each attempt has its own private Git state (HEAD, index,
refs, objects, configuration and credential setup) in its attempt directory,
so siblings never see each other's staging or unpublished commits. Contexts
outlive attempt scratch, and later admissions never reset live Git state.
The host does not execute with lane-mutable Git configuration. For sandboxed
lanes, confinement protects the repository's shared Git state.
Import uses Git's quarantined receive and verification; publication
compare-and-swaps the host branch against the tip it observed. A lane whose
history forked from the branch is refused at once and its private Git is
retained for recovery. The base remains pinned until publication.
The worktree's native index follows the branch from a recorded commit and the
digest of the index Provenant left, advanced under a per-worktree native-index
lock (never the admission gate) after each publication and repeated on every
settlement or resume until it matches the tip. The advance is advisory and
never fails settlement. If Git refuses it, the index changed since the record,
or the record or index cannot be read, the index and record are left unchanged
and status reports `index_out_of_sync` with the reason, the paths and one
command that resets the index and the record together.
Import retention refs belong to each attempt, even for an identical commit;
publication and cleanup remove only that attempt's pins. Settlement clears an
attempt's import pins on every terminal outcome, not just successful
publication, and GC removes the pins of any settled or missing attempt so ref
listings stay small. Bulk pin removal is one batched ref transaction, never one
Git process per pin. The same condition gates the import pin and the
import-extra refs: when no writer slot is available or the fence listing has
issues, cleanup keeps both as recovery evidence. GC counts the import pins a
preview would remove and deletes them only when applied. An unreadable run
store leaves the pins untouched: GC warns on stderr and carries on with the
settled-lane and path-based passes instead of skipping the project.
Lane Git import runs with `-c advice.graftFileDeprecated=false`, and raw git advice
(such as `hint:` lines) is filtered from stderr before warning so advice lines
never surface as Provenant warnings.

The GitHub credential receipt governs token delivery even when sandboxing is
off: `none` withholds host token environment variables, `injected` passes only
the selected token, and `ambient` preserves the native host environment.
`ambient` also pins `GH_CONFIG_DIR` to the host's gh configuration directory,
resolved as gh does on the host (the host's `GH_CONFIG_DIR`, else its
`XDG_CONFIG_HOME/gh`, else `~/.config/gh`), in the lane environment and in the
lane's `gh` shim, because an adapter may re-point the lane's own
`XDG_CONFIG_HOME` at private state. The receipt's `detail` names the directory
and which host setting chose it. The shim directory leads the lane's `PATH` as
set by the host; a shell profile that reorders `PATH` does not unpin gh, since
the pin lives in the environment. A lane opens its own pull request with
`gh pr create --body-file`; `provenant pr open` needs a settled attempt.
- Why: untrusted lane state must not alter shared Git or publish over another writer.

## Writer pin and re-pin

A graph pins its source commit at submission. A retry keeps that pin and the
predecessor's terminal head. Before any plan node spends tokens, the host
preflights the writer worktree: a dirty tree is refused at submission, naming
the dirty paths and the setting `lanes.writer_dirty` with the layer that set it.
That one overridable setting chooses `refuse` (the default), `wip_commit` (a
local WIP commit whose id is recorded in the graph events before launch), or
`continue_in_place` (a warning that names the retained paths). This preflight
covers writer leaves in composites and loops before the submission snapshot is
sealed. WIP mutation holds writer exclusion, refuses unsettled reservations and
rechecks the head and dirty paths under that exclusion. Credential and
Git-internal roots are a hard refusal under every value.

On a manual retry, when the worktree head descends from the pinned commit — the
writer's own commits are the only difference — the retry re-pins to that head
and warns with both ids. When the head is not a descendant it refuses, naming
the pinned commit and the current head. A pin mismatch never uses the launch
handoff refusal text; its cause names the pin and the head and says what to do.
- Why: a refusal the host could predict must not waste a plan node's tokens, and
  an opaque "host configuration" cause sends the operator to the wrong place.
- Owner: `crates/run/src/writer_dirty.rs`, `crates/run/src/writer.rs`,
  `src/fabric/graph_execution.rs`, `src/fabric/owner.rs`.

## PR contributions JSON reference

The [attribution contract](run-attribution.md) governs contribution records.
The [contribution owner](../../tools/front-door/crates/run/src/pr_contribution.rs)
and [command renderer](../../tools/front-door/src/pr/contributions.rs) define the payload.

## Stop intent

Supervisor owns one typed stop-intent protocol. `stop-intent.json` names the
attempt scope, reason, requester and operation ID; guardian, admission,
import, publication and continuation readers use `read_stop_intent`.
`cancel.json` is reserved for explicit cancellation and cannot carry a steer.
A valid explicit cancellation takes precedence over steering. A foreign,
malformed or unreadable intent grants no continuation or publication authority.

A delivered steer may preserve committed work while its exact authorised
intent still matches. A current explicit owner cancellation separately permits
preservation under proven quiescent custody, including after an earlier steer
or natural exit. It vetoes continuation without changing the provider's sealed
stop cause. A replacement request invalidates an in-flight preservation token;
recovery must validate the new intent before acting. Stop delivery
ordering belongs to the provider exit: requesting a stop after a natural exit
does not turn the finished attempt into a cancellation.
- Why: every stop reader needs the same meaning, and a steering request must
  never accidentally cancel the work it intends to continue.
- Owner: `crates/supervisor/src/custody.rs`, `crates/run/src/settlement.rs`

## What follows a stop

Settlement classifies terminal outcome, sealed stop and graph node outcome together; no settlement sent to the chair succeeds a graph node.
Each cause declares provider, host or owner source once. Route-quality evidence uses provider-caused outcomes and successful work certified by the settlement declaration; a host stop keeps its host source and host failures remain non-quality evidence. Missing account identity remains unknown usage attribution, not a model-quality failure.
Each cause has one default next step, with one per-node override point. Pinned
causes ignore overrides. A new route starts a fresh session. Unproved custody
always goes to the chair; no successor writer starts while custody is uncertain.
A restart preserves the worktree and archives prior attempts. Bounds may turn a
restart into a chair decision. See [routing](routing.md) for route selection.
- Why: one decision keeps stop handling consistent and protects writer custody.

A writer's declared stop settles as `stopped_early`, or as a blocked cause when its
final handoff is blocked: `blocked_environment` or `blocked_dependency` when its
`blocked_on` says so, and `blocked` for an owner decision, a missing capability or
no kind given. The node's advance code records whether the stop left
committed work: `writer_declared_stop` when the writer's head moved past its
source commit, and `stopped_no_work` otherwise, whether the worktree is clean or
holds uncommitted edits. For a `stopped_early` handoff both codes take the
`stopped_early` row below unless the node overrides it: a new route, which restarts the writer in a fresh session
on its retained worktree. A junior route escalates to senior; otherwise the
restart uses a declared route not yet launched, and an alias route may resolve
again. A different route is always preferred when one exists.

With no different route, a stop that can continue on its own route
(`stopped_early`, `lane_token_lost` and `context_handoff`) takes the same route
with a fresh session on the retained worktree and branch, briefed with the
previous handoff. This fallback is bounded twice: by the graph-wide
`limits.continuations`, whose value and source the restart and pause reasons
name, and by progress, because after the first round each further round must add
a commit or change the handoff's `remaining[]`. A round that does neither pauses
for the chair, which keeps a usage-limit stop that a writer wrote up as
`stopped_early` from looping. A node `on_stop` entry for the cause wins over
the fallback, so `chair` or `new_route` still pauses or restarts as the node
says. Any other stop with no route, or with the continuation budget spent,
pauses for the chair.

A `blocked` stop is pinned to the chair, and its pause says which kind of block
it is: only `owner_decision` and `capability` read as an owner decision or a
missing capability, and a handoff with no `blocked_on` reads as "blocked" and
says the kind was not given. A `blocked_environment` stop retries once on the
same route in a fresh session, then pauses for the chair worded as an
environment problem; it is not pinned, so a node `on_stop` entry can send it to
the chair at once. A `blocked_dependency` stop pauses for the chair as a wait,
and is not pinned either.
- Why: the chair must see whether a restart resumes committed work, without a
  second stop rule that could treat the two differently; a writer that stops
  early on its only route should continue, not wait for a manual relaunch.
- Owner: `crates/fabric-runtime/src/graph/transitions.rs`,
  `crates/fabric-runtime/src/graph/next_step.rs`

| Cause | Default | Pinned | Retries |
| --- | --- | --- | --- |
| `stopped_early` | `new_route` | no | 0 |
| `blocked` | `chair` | yes | 0 |
| `blocked_environment` | `chair` | no | 1 |
| `blocked_dependency` | `chair` | no | 0 |
| `handoff_missing` | `chair` | no | 0 |
| `handoff_malformed` | `chair` | no | 0 |
| `empty_result` | `new_route` | no | 1 |
| `uncommitted` | `chair` | no | 0 |
| `input_required` | `chair` | yes | 0 |
| `context_handoff` | `new_route` | no | 0 |
| `token_budget` | `chair` | no | 0 |
| `deadline` | `chair` | no | 0 |
| `cancelled` | `chair` | yes | 0 |
| `provider_stop` | `new_route` | no | 1 |
| `process_crash` | `new_route` | no | 1 |
| `auth_required` | `new_route` | no | 0 |
| `usage_limited` | `new_route` | no | 0 |
| `model_unavailable` | `new_route` | no | 0 |
| `adapter_unavailable` | `new_route` | no | 0 |
| `permission_denied` | `chair` | no | 0 |
| `identity_mismatch` | `chair` | yes | 0 |
| `lane_token_lost` | `new_route` | no | 0 |
| `source_view_refused` | `chair` | no | 0 |
| `route_refused` | `chair` | no | 0 |
| `access_policy_refused` | `chair` | no | 0 |
| `disk_floor` | `chair` | no | 0 |
| `disk_unobserved` | `new_route` | no | 1 |
| `prompt_unreadable` | `chair` | no | 0 |
| `cloud_placement_removed` | `chair` | no | 0 |
| `write_grants_refused` | `chair` | no | 0 |
| `worktree_busy` | `chair` | no | 0 |
| `launch_handoff_refused` | `chair` | no | 0 |
| `retained_writer_import` | `chair` | no | 0 |
| `unsupported_platform` | `chair` | no | 0 |
| `unknown_pacing_account` | `chair` | no | 0 |
| `unregistered_account` | `chair` | no | 0 |
| `credential_helper_failed` | `chair` | no | 0 |
| `command_unavailable` | `chair` | no | 0 |
| `host_native_unconfigured` | `chair` | no | 0 |
| `spawn_failure` | `new_route` | no | 1 |
| `settlement_failed` | `chair` | no | 0 |
| `lost` | `chair` | no | 0 |
| `unsettled` | `chair` | no | 0 |
| `quarantined` | `chair` | no | 0 |
| `steered` | `chair` | yes | 0 |
| `unclassified` | `new_route` | no | 1 |