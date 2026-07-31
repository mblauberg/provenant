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
  managed manifest plus a public rename registry
  (`config/skill-renames.json`), so a renamed skill migrates without a user
  deleting global links by hand. It never claims or overwrites an unmanaged
  target.
- The delivery kernel: profiles in `config/delivery-profiles.json` for software,
  research, analysis, document and agent-product work, the neutral
  `delivery-run` schema-v1 receipt owned by `deliver`, and
  `scripts/validate_delivery_scenarios.py`.
- Risk and authority policy in `config/risk-policy.json`: the `routine`,
  `substantial`, `crucial` and `terminal` tiers and the factors that raise a
  tier. The review-pressure ladder those tiers select lives in `HARNESS.md`.
- Model routing through `scripts/model-route`, resolving the `flagship`,
  `workhorse` and `scout` aliases from runtime capability discovery, with
  receipts that separate adapter, endpoint, model family, requested and
  effective effort, capability source and any substitution.
- The Agent Fabric runtime under `runtime/`: the fabric itself, the wire
  protocol, the console, the Herdr adapter and the Rust review-portal
  supervisor, with an MCP server for agent spawn, durable messaging, budgets
  and run state.
- Gates: `scripts/check-harness` (policy checks, skill trigger fixtures, shell
  parse, `pytest`), `scripts/static-security-check.py` and
  `scripts/public-release-check`, plus a CI workflow that runs the harness gate;
  the fabric, console and Herdr typecheck, tests, evaluation, load and
  production dependency audit; the review-portal-supervisor Rust fmt, clippy and
  test jobs; and a `zizmor` workflow security lint. The aggregate `ci-status`
  job is the single required check.
- The shared worktree invariant and the checked `scripts/worktree` helper: an
  authorised linked worktree lives at the owning repository's
  `.worktrees/<task-agent>` path and nowhere else.
- Repository documentation: `docs/ARCHITECTURE.md`, specs, evals, runbooks,
  `MAINTAINING.md`, `SECURITY.md`, `ACKNOWLEDGEMENTS.md` and
  `THIRD_PARTY_NOTICES.md`, under the MIT licence.
- Community files: this changelog and the bug, feature, skill-proposal and
  work-item issue forms.
- Standalone Agent Fabric specifications for run-plan declaration, agent
  topology projection and work-facts projection.
- The `setup-repo` skill, extending the former `github-setup` owner with
  inspect-first repository process, tracker and documentation setup.
- Typed zero-touch Agent Fabric bootstrap receipts covering trust resolution,
  daemon start-or-attach, seat install-or-replay and a bounded identity/mailbox
  smoke, plus a schema-cutover gate that leaves incompatible state untouched.
- `database archive-and-fresh`, with a digest-bound read-only preview,
  byte-correctness confirmation interlock, durable exact-source archive,
  typed conflict and recovery results, and distinct exit `4` recovery handling.
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
- Four daemon-side lifecycle facts emitted by the fabric (#486).
- Review panels for `orchestrate`, with council and breadth presets (#485).
- An advisory model dossier for route selection, `docs/model-dossier.md`, with
  Gemini 3.6 Flash recorded as a reachable google route (#470, #484).

### Changed

- `AGENTS_HOME` now names only the product root. When it names a non-`~/.agents`
  checkout without an explicit instance root, the next `install-harness` run
  seeds the instance at `~/.agents` and rewrites that instance's machine-local
  product pointer to the selected checkout. Linked worktrees are refused unless
  the operator explicitly acknowledges the pointer rewrite (#549).
- Applied the `writing-great-skills` doctrine across the catalogue (epic #124):
  merged `skill-audit` + `skill-authoring` into the branched `skill-craft`,
  merged `frontend-design` + `frontend-review` into the branched `ui-ux-design`,
  gutted and renamed `autonomous-lab` to `autopilot` (run state now lives under
  `.agent-run/<mission-id>/`), made `natural-writing` the single-owner writing
  hub the domain writing skills link to, and added an autonomous
  ready-issue-implementation mode to `orchestrate` that stops at the user PR
  gate. The catalogue is now 32 skills; managed renames are recorded in
  `config/skill-renames.json`.
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
- `runtime/agent-fabric-protocol` splits projection from operator-projection to
  stay under the module size cap (#471).

### Fixed

- Prevented `scripts/configure-agent-fabric-mcp.py` from crashing under Python
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
- Content-addressed the protocol freshness preflight's root-manifest arm with a
  build-time digest stamp, so unchanged manifest rewrites no longer block
  `agent-fabric` or `agent-fabric-mcp`; `doctor` now reports a stale build and
  its exact repair instead of being blocked by the wrapper (#438, #439).
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
`scripts/managed_installation_manifest.py`, the schema `$id` values under
`runtime/agent-fabric/schemas/`, the run-state path under
`~/.local/state/agent-harness/`, and the `HARNESS.md` filename that installed
global instructions point at by name. `AGENTS_HOME` and `$HOME/.agents` are
unchanged, so no existing installation moves.

No release or tag is claimed here. Move these notes under a version only after
that tag and release are separately authorised.
