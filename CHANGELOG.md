# Changelog

Notable changes to Provenant are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Changes remain under `Unreleased` until a tag and release are separately
authorised. [`MAINTAINING.md`](MAINTAINING.md) requires evaluation runs to
record the harness revision they ran against.

## Unreleased

The current pre-release tree includes:

### Added

- `HARNESS.md`, the constitution: Claude Code and Codex as equal primary
  orchestrators with one session chair, the scope-to-retrospect lifecycle, the
  user gates, and the rule that no two agents write the same source surface at
  once.
- 32 Agent Skills under `skills/`, covering delivery (`scope`, `deliver`,
  `implement`, `tdd`, `refactor`, `diagnose`, `code-review`, `evaluate`,
  `release`, `retrospect`, `session`, `work-map`), orchestration, writing,
  design and diagrams, web engineering, and harness development.
- `scripts/install-harness`, which installs the skills and the instruction
  bootstrap into Claude Code and Codex, preserves unmanaged content, and leaves
  portable `skill-craft` canonical over Codex's bundled `skill-creator`.
- `scripts/manage_installation.py`, giving `plan` and `reconcile` against a
  managed manifest that records ownership and supersession history. It retires
  safe managed leftovers without claiming or overwriting an unmanaged target.
- The delivery kernel: profiles in `config/delivery-profiles.json` for software,
  research, analysis, document and agent-product work, the neutral
  `delivery-run` schema-v1 receipt owned by `deliver`, and
  the delivery-scenario evaluator.
- Risk and authority policy in `config/risk-policy.json`: the `routine`,
  `substantial`, `crucial` and `terminal` tiers and the factors that raise a
  tier. The review-pressure ladder those tiers select lives in `HARNESS.md`.
- Model routing through `scripts/model-route`, resolving the `flagship`,
  `workhorse` and `scout` aliases from runtime capability discovery, with
  receipts that separate adapter, endpoint, model family, requested and
  effective effort, capability source and any substitution.
- Fabric, at `runtime/fabric`: messages, shared tasks and an activity log for
  the agents working on one project, over MCP or a shell CLI. One SQLite file,
  no daemon, and identity derived from the working directory, so there is
  nothing to trust, bootstrap or provision.
- Gates: `scripts/check-harness` (policy checks, skill trigger fixtures, shell
  parse, `pytest`), `scripts/static-security-check.py` and
  `scripts/public-release-check`, plus a CI workflow that runs the harness
  gate, the Fabric typecheck, tests, MCP smoke and dependency audit, a
  split-root contract rig and a `zizmor` workflow security lint. The aggregate
  `ci-status` job is the single required check.
- The shared worktree invariant and the checked `scripts/worktree` helper: an
  authorised linked worktree lives at the owning repository's
  `.worktrees/<task-agent>` path and nowhere else.
- Repository documentation: `docs/ARCHITECTURE.md`, specs, evals, runbooks,
  `MAINTAINING.md`, `SECURITY.md`, `ACKNOWLEDGEMENTS.md` and
  `THIRD_PARTY_NOTICES.md`, under the MIT licence.
- Community files: this changelog and the bug, feature, skill-proposal and
  work-item issue forms.
- The `setup-repo` skill, extending the former `github-setup` owner with
  inspect-first repository process, tracker and documentation setup.
- Certifying-profile pin observation in `doctor` and the separate uncached
  `npm run profile:pin` live-provider repair command. Doctor accepts capability
  observations cached within the last six hours; the repair command edits the
  digest-bound profile and must be reviewed like any other repository change.
- A read-only worker-liveness helper that reports worker CPU, session-log age
  and worktree state without signalling or supervising the process.
- CI checks for deterministic adapter builds, seeded adapter-digest mismatch
  rejection and the extracted model-routing catalogue validator.
- A model-preferencing layer in `config/model-preferences.json`, expressing
  per-task-class and per-role preference, family/model/adapter deprioritisation
  and fair-round-robin spreading inside the hard routing tiers rather than
  across them (#478).
- Review panels for `orchestrate`, with council and breadth presets (#485).
- An advisory model dossier for route selection, `docs/model-dossier.md`, with
  Gemini 3.7 Flash recorded as the current reachable Google Flash route.

### Removed

- The daemon fabric and everything that served it: `runtime/agent-fabric`, the
  wire protocol, the operator console, the Herdr adapter and the Rust
  review-portal supervisor, with their specifications, operations runbooks, CI
  jobs and gate scripts. Roughly 350,000 lines. The capability boundary the
  daemon enforced was never real on a single-user machine, and the eighteen
  preconditions between a fresh directory and its first message were why agents
  could not use it in ordinary projects. See
  [ADR 0020](docs/adr/0020-retire-the-daemon-fabric.md); the tree is preserved
  on the `legacy/agent-fabric` branch.
- The model router's daemon-activation gate and `config/agent-fabric.yaml`.
  There are no in-process provider adapters left to activate, so every
  cross-provider dispatch is a direct CLI call. Adapter compatibility, which
  constrains which model families each provider CLI accepts, is unchanged.
- The `allowed_fabric_operations` and `denied_fabric_operations` delivery
  receipt fields, which scoped authority against the retired protocol's
  operation enum.
- `provenant doctor` and `provenant project`, which delegated to the daemon CLI.
  `provenant fabric` now runs the new Fabric CLI.
- The obsolete `project-activation` skill and its trust/bootstrap front door.
  Run `provenant fabric whoami` from the working directory instead; there is no
  separate project activation step.

### Changed

- Fabric MCP now publishes concise protocol instructions for clients that
  materialise tool guidance, and read-only diagnostics reject inaccessible or
  unstable database snapshots instead of reporting healthy absence or stale
  counts.
- Raised the OpenAI worker route from `medium` to `high` effort via
  `openai.role_effort_defaults.worker.workhorse`, completing the Luna reorder.
  Moving `gpt-5.6-luna` to the front of `aliases.workhorse` on its own bought
  Luna at `medium`, which `docs/model-preferences.md` had already identified as
  a downgrade rather than the intended trade, and was the stated reason the
  array had been left alone. The raise is scoped to the OpenAI family rather
  than applied to the `legwork` task class, so Anthropic and Google workhorse
  routes stay at `medium` instead of inheriting an unasked-for cost rise. This
  is the same mechanism the catalogue already uses to lift `critical-review` to
  `max` and `orchestration` to `ultra`.
- Raised `codex-implementer` from `haiku` to `sonnet`, keeping `effort: low`.
  The dispatcher builds sandbox flag sets, decides write scope, may provision a
  worktree, and must verify what landed in the tree rather than believing the
  Codex transcript. That verification step is where too small a dispatcher
  fails, because the cheap wrong answer is to trust the transcript. The
  expensive reasoning stays with Codex and is paid for on OpenAI's tokens.
- Gave the Codex dispatchers a write-scope table mapping the task to exact
  flags, a conditional rule for when to provision a worktree rather than work in
  place, and the worktree lifecycle commands. Replaced the instruction to read a
  transcript's tail with bounded `grep` extraction: one transcript line can be
  an entire JSON catalogue, so even `tail` can dump hundreds of kilobytes into
  the dispatcher's context and charge the caller twice for the same reasoning.
- `AGENTS_HOME` now names only the product root. When it names a non-`~/.agents`
  checkout without an explicit instance root, the next `install-harness` run
  seeds the instance at `~/.agents` and rewrites that instance's machine-local
  product pointer to the selected checkout. Linked worktrees are refused unless
  the operator explicitly acknowledges the pointer rewrite (#549).
  **Breaking, with a migration.** `AGENTS_HOME` previously doubled as an
  instance-root fallback. A setup that relied on that second sense, with
  `AGENTS_HOME` naming the instance and no `AGENT_FABRIC_INSTANCE_ROOT` set,
  now resolves its instance to `~/.agents`, and the configuration under the old
  location stops being layered. To keep the old instance, name it explicitly
  with `AGENT_FABRIC_INSTANCE_ROOT`; to adopt the new default, re-run
  `install-harness`. Both roots were ambiguous precisely because one variable
  meant two things, which is what this change ends.
- An instance contributes its local configuration layer only when its
  machine-local `.agent-fabric/product-root.json` pointer names the product
  actually in use, rather than whenever an instance root happened to be set
  explicitly. Both paths are canonicalised before comparison, so a product
  reached through a symlink still pairs. A pointer that is absent leaves the
  instance quietly unpaired, but one that is present and cannot be read,
  parsed or recognised now raises rather than silently dropping the layer:
  local configuration can only narrow, so failing quiet would widen the
  effective limits (#549, #563).
- Applied the `writing-great-skills` doctrine across the catalogue (epic #124):
  merged `skill-audit` + `skill-authoring` into the branched `skill-craft`,
  merged `frontend-design` + `frontend-review` into the branched `ui-ux-design`,
  gutted and renamed `autonomous-lab` to `autopilot` (run state now lives under
  `.agent-run/<mission-id>/`), made `natural-writing` the single-owner writing
  hub the domain writing skills link to, and added an autonomous
  ready-issue-implementation mode to `orchestrate` that stops at the user PR
  gate. At that earlier consolidation point the catalogue reached 33 skills;
  the current pre-release tree is 32 and the managed manifest records
  supersession history.
- Completed the progressive-disclosure refactor tracked by #335: compact
  ambient instructions, repository-managed Claude workflows, enforced
  cross-skill reference boundaries and conditional comparative evaluations.
- Landed the implemented #141 Attention Deck slices through phases A, B1, B2,
  B3 and C: renderer extraction, session-local filters and pins, declared run
  plans, topology and workflow facts, deterministic adaptive activity grouping
  and exact run-scoped drill-down across work, agents, evidence, activity and
  issues. Remaining work stays tracked by issue #141.
- `doctor` now reports causal lifecycle state, provider identity
  drift/staleness and certifying-profile pin drift or unknown observations. It
  is read-only and quota-free by default; `--consume-provider-quota` opts into
  live provider capability probes and a private cache refresh.
- Provider routing now derives OpenAI effort capabilities at runtime, rejects
  unsupported Claude `ultra`, records Claude effort as `provider-unverified`,
  derives the Claude probe alias from the catalogue and applies general
  risk-tier overrides only to the resolved model.
- Stale protocol builds now fail before daemon election with exit `78`, and
  bootstrap, seat renewal and Console attachment reject stale daemon result
  shapes instead of accepting an older contract.
- Authorised post-merge cleanup now proves the merge before pruning an
  implementation branch's worktree and local or stale remote-tracking refs:
  ancestry for a merge commit, and the pull request's merged state plus an empty
  path-scoped content diff for a squash merge, which leaves no ancestry link
  (#430).
- `database archive-and-fresh` now requires an approval the calling agent
  cannot grant itself, asserted through `--unattended-approval-asserted-by`
  (#475).
- The review profile catalogue is pinned by digest and verified code-adjacent,
  at the point it is read rather than at load time, with
  `npm run profile:catalogue:pin` and `profile:catalogue:deploy` owning the pin
  and deployment (#473, #481).
- Both review validators now share one leg-status vocabulary (#479).

### Fixed

- Stopped a bare cross-family dispatch defaulting to the flagship model.
  `cf_dispatch.sh` hardcoded `--alias flagship`, so any call that named no alias
  ran on `gpt-5.6-sol`, which no reordering of the workhorse alias could reach.
  The alias now follows the role: `flagship` for `lead`, `orchestrator` and
  `critical-review`, or when a risk tier or an explicit model is named, and
  `workhorse` otherwise. A bare codex dispatch resolves `gpt-5.6-luna`.
- Stopped `cf_dispatch.sh` overriding a caller's explicit
  `AGENT_FABRIC_PRODUCT_ROOT`. Deriving the product root from the script's own
  checkout is right when nothing else says otherwise, but it silently replaced a
  deliberate setting in vendored and nested layouts.
- Required a unique per-dispatch slug in both Codex dispatcher agents. The slug
  was derived from the task, so two concurrent dispatches overwrote each other's
  prompt, report and transcript files and a run answered someone else's
  question. Observed live, three dispatches deep.
- Corrected the remaining claims that outlived their evidence: `cli-headless.md`
  still called the linked-worktree commit failure intermittent, and
  `codex-analyst.md` described `-s read-only` as enforcing a repository
  boundary. It is a write boundary and not a read boundary, so it is not a
  confidentiality control.
- Kept a successful capability probe's stderr visible as a diagnostic instead of
  discarding it. Rejecting unrecognised stderr would restore the outage this
  release fixes, so the streams stay advisory rather than fatal.
- Stopped a child process's stderr corrupting every capability snapshot.
  `run_bounded` merged stderr into stdout, and all three capability producers
  parsed that merged stream as machine-readable data, so a single warning line
  from a provider CLI broke discovery. It now takes `merge_stderr=False`,
  spooling and bounding the two streams independently, and the Codex, Claude and
  Agy producers parse stdout alone while reporting stderr as the diagnostic.
  The Codex and Claude routes failed closed on this, taking the adapter offline;
  the Agy route failed silently, recording the stderr text as a phantom
  effortless model and still exiting 0.
- Moved the Claude unknown-effort warning check to stderr, where the CLI
  actually writes it. It had been a stdout string match that only worked because
  the streams were merged, so parsing stdout alone would have retired it
  silently and turned a caught error into an accepted wrong answer.
- Stated the Agy limits the agent files had left implicit: the route is
  `prompt_only` rather than `enforced` because `--sandbox` is not a read-only
  boundary, exit 0 does not prove the work happened, and only Gemini identifiers
  may be selected, since `agy models` also fronts Anthropic and GPT-OSS models
  and routing to one of those makes a cross-family review circular.
- Reached explicit Codex models again. codex-cli 0.146.0 accepts an explicit
  `-m` on a ChatGPT subscription account, so the codex adapter no longer
  dispatches `account-default` and `gpt-5.6-luna` is selectable rather than
  fixed to the account default. Luna now leads the OpenAI `workhorse` alias
  ahead of `gpt-5.6-terra`, matching the dossier since the July 2026 price cut.
  The generic account-default machinery is retained and tested through a
  synthetic adapter.
- Pinned Codex dispatch to `service_tier=default` and prohibited the priority
  tier outright. The fast path is a config key rather than a flag, so it was
  inheritable from `~/.codex/config.toml` without appearing in any transcript,
  at roughly double the usage for about 1.5x speed.
- Stopped `codex-analyst` claiming a read-only sandbox it did not have.
  `sandbox_workspace_write.writable_roots` adds to the always-writable set
  rather than restricting it, so the repository under analysis was writable and
  the guarantee was `prompt_only`, not `enforced`. The analyst now runs
  `-s read-only` and recovers its report through `--output-last-message`.
- Corrected the Codex worktree commit failure from "intermittent" to its actual
  deterministic cause, a linked worktree's git metadata falling outside the
  sandbox's always-writable set, and documented the narrow
  `--add-dir <primary>/.git` grant that fixes it.
- Replaced the claim that Codex model discovery is impossible headlessly.
  `codex models` is not a subcommand, but `codex debug models` works headlessly
  and is already what `codex_capabilities.py` shells out to.
- Prevented `scripts/configure-fabric-mcp.py` from crashing under Python
  3.14 when its standard-output stream is already closed (#396).
- Accepted a readable SQLite database plus WAL source set without SHM, while
  still rejecting SHM without WAL and mixed rollback/WAL state. Recovery
  results now name the private claim directory and its disposition, interrupted
  cutover residue reports `CUTOVER_RESIDUE_DETECTED` at exit `4`, and
  `--confirm-source-set` is documented as a byte-correctness interlock rather
  than a user-authority gate (#441).
- Degraded an effort shortfall only to the highest supported effort at or below
  the request, anchored Claude alias-to-model identity, rejected colliding
  alias/resolved-model snapshot keys, and rejected task-class effort that
  diverges from its probe policy's `minimum_effort` (#440).
- Killed and reported incomplete macOS codesign probes that exceed the
  15-second bound instead of leaving a provider-identity check hung (#423).
- Rejected stale daemon result shapes before bootstrap, seat renewal or Console
  attachment rather than continuing against an older protocol contract (#428).
- Autobuilt the install root's stale protocol `dist` instead of failing the
  caller, and guarded protocol `dist` writers with a shared build lock so
  concurrent builds cannot interleave (#480, #464).
- Stopped the Console stamping derived values as `Observed`, and bounded
  activity-projection cost and the page node budget (#476, #463).
- Regenerated every version-bearing SDK pin field rather than only the first,
  and reported first-recorded legacy bootstrap provenance (#483, #462).
- Removed four test flakes by mechanism rather than by retrying or raising a
  timeout: the portal crash-helper phase marker is published atomically (#444),
  the orchestration FIFO oracle no longer requires a transient dispatch state to
  persist (#449), a synchronous SQL invariant no longer stands behind a daemon
  and socket fixture, and an MCP restart no longer assumes daemon exit proves a
  separate proxy process observed the close (#429).

## Notes

The name Provenant is the public identity. Several internal identifiers keep the
older `agent-harness` string on purpose, because renaming them would break
existing installations: the installation manifest owner in
`scripts/managed_installation_manifest.py`, the run-state path under
`~/.local/state/agent-harness/`, the `AGENT_FABRIC_*` environment variables, and the `HARNESS.md` filename that installed
global instructions point at by name. `AGENTS_HOME` and `$HOME/.agents` are
unchanged, so no existing installation moves.

No release or tag is claimed here. Move these notes under a version only after
that tag and release are separately authorised.
