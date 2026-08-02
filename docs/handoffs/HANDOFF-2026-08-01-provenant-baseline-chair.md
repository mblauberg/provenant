# Provenant stable-baseline chair handoff

Status: active
Effort: provenant-stable-fused-baseline
Outgoing chair: Codex, session `019fbceb-f7c9-7fa1-a27f-fc80d0183aac`
Incoming chair: Claude Opus session in Herdr pane `w5:p2C`
Updated: 2026-08-01, Australia/Brisbane
Consumed-at: 2026-08-02, Australia/Brisbane, by the incoming Claude Opus chair after
independent verification of this file's SHA-256, all five lane heads and diffs, the three
writer PIDs, the installed daemon, the evidence paths and live GitHub state. Two deltas
against this snapshot: writer PID `69123` (`worker-outcome-consolidated`) has exited leaving
an uncommitted twelve-path diff, and PIDs `72869`/`6908` were relaunched at 12:57 and 13:03
on 2026-08-02 rather than being long-running.
Supersedes: none

## Goal and authority

Build a clean, stable, reliable, flexible, extensible, interoperable and easy-to-maintain Provenant baseline without inappropriate production-scale complexity. Provenant is primarily a local personal tool. Keep the current fused `$AGENTS_HOME` layout until the user confirms the baseline; the user will perform the `.agents` / Provenant repository split afterward.

The finished installation must automatically register and use Agent Fabric when Claude Code or Codex starts in arbitrary Git and non-Git projects on this computer. The incoming Claude session is the chair and remains the orchestrator. It should use GPT-5.6 Luna workers for implementation and its own native Opus 5 subagents for independent architecture, correctness and minimality review. Sol is optional only at critical adjudication points; Agy/Gemini may add advisory cross-family pressure after Fabric is healthy.

The user expressly authorises the incoming chair to use GitHub as needed, including issue/PR updates, pushes, merges and related tracker work. Repository Git safety still applies: one writer per worktree; no force pushes, destructive ref rewrites, unrelated worktree removal or broad deletion without exact authority. Do not perform the product/instance split yet.

## Mandatory first stage

Before landing more work, independently reopen the live tree, current instructions, specs/ADRs, worktrees, GitHub state and the evidence below. Use Opus subagents with distinct lenses to assess:

1. overall architecture and whether the issue set exposes common root causes;
2. reliability, lifecycle and trust boundaries;
3. flexibility, extension points and Claude/Codex interoperability;
4. maintainability, duplication and accidental coupling;
5. overengineering against a solo, mostly personal local-tool threat and operating model.

Reduce findings by evidence rather than vote. Prefer the smallest architectural repair that closes several recurring defects. Preserve deliberately strong integrity boundaries where they protect credentials, authority, filesystem custody or irreversible effects. Update the work plan and GitHub tracker only after this audit has reconciled local branch ancestry and open PRs.

## Current integration baseline

- Worktree: `$AGENTS_HOME/.worktrees/baseline-integration`
- Branch/head: `baseline-integration` at `8a4ea700`
- Relationship: `main` is an ancestor; integration is 71 commits ahead.
- State at handoff: clean.
- Chair verification passed: `npm run build` and `npm run schema:check:generated`.
- Most recent integrated fixes include provider-assurance negotiation, optional Kiro degradation, TypeScript scratch freshness, canonical path identity, overload admission and delayed transport-write supervision.
- Do not merge the five branches below blindly. Their bases and overlapping files differ; land serially and rerun coherent checks after every tree change.

## Active Luna writers: revalidate PIDs before taking ownership

These direct Luna processes were intentionally left running for the incoming chair. A PID may have exited by pickup. Never start a second writer in the same worktree until `kill -0 <pid>` fails and the worktree/report has been inspected.

| PID at handoff | Worktree | Objective | Current state |
|---|---|---|---|
| `72869` | `provider-integrated-final-fix` | repair provider-assurance exact-review findings | active; seven-file dirty diff |
| `6908` | `daemon-build-freshness-fix` | repair runtime-identity, observer preflight and status/doctor findings | active; recheck diff |
| `69123` | `worker-outcome-consolidated` | repair immutable/no-follow host-boundary implementation | active; focused suite rerunning |

Use `ps -p <pid> -o pid=,etime=,time=,command=` and worktree `git status`. Preserve partial output if a worker failed. Direct CLI was a documented degraded fallback because Fabric could not round-trip; do not treat same-family Luna review as final certification.

## Completed but not yet landed lanes

### Paired completion, issue #608

- Worktree: `$AGENTS_HOME/.worktrees/paired-completion-608`
- Commits: `137efa62`, `c61efe40`, `9b476a89`
- Uncommitted final repair: two files, deriving bootstrap expiry from persisted project creation provenance and rejecting forged expiry before CAS.
- Fresh Luna verification: 8 focused authority tests and schema checks passed. Full typecheck/integration was blocked by the branch's stale external protocol-package link missing `MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE`, not by the two-file repair.
- Exact review finding being repaired: [luna-608-9b47-exact-review.md](../../.agent-run/baseline-2026-08-01/findings/luna-608-9b47-exact-review.md).
- Rebase/replay or verify on the coherent integration dependency graph, then independently review and commit.

### Automatic trust provenance

- Worktree: `$AGENTS_HOME/.worktrees/auto-trust-provenance-fix`
- Commits: `0f2325c2`, `eae44999`; the latter was an intermediate rejected JSON+SQLite migration design.
- Current six-file uncommitted redesign removes DB migration/writes. V2 records truthfully persist a versioned trust digest; migrated V1 records retain a validated legacy-compatible digest; new/retrust records use canonical V2.
- Fresh Luna verification: 83 unit tests, 5 adjacent tests, schema check, typecheck, build and diff check passed. Unix-socket lifecycle tests were sandbox-blocked with `EPERM` (1 pass, 9 blocked/failures). No commit was made.
- Prior rejected design review: [luna-autotrust-eae4-exact-review.md](../../.agent-run/baseline-2026-08-01/findings/luna-autotrust-eae4-exact-review.md).
- Verify lifecycle behavior in a correctly prepared worktree, review the cumulative final tree, then commit without rewriting the intermediate history unless repository/user policy expressly permits it.

### Provider assurance

- Worktree/head: `provider-integrated-final-fix` at `8835df64`, currently being repaired by PID `72869`.
- Blocking review: [luna-provider-8835-exact-review.md](../../.agent-run/baseline-2026-08-01/findings/luna-provider-8835-exact-review.md).
- Findings: one workflow auto-apply boolean bypass, two advisory-route compatibility errors, and a stale `lockfile-install-attestation` fixture token.
- Issue #557 needs explicit reconciliation: the newer assurance design intentionally makes only `full-vendor-identity` certifying and may supersede the issue's requested lockfile enum. Do not retain two competing certification concepts.

### Daemon build freshness

- Worktree/head: `daemon-build-freshness-fix` at `be8b0c66` after `ff742789`, currently being repaired by PID `6908`.
- Blocking review: [luna-daemon-be8b-exact-review.md](../../.agent-run/baseline-2026-08-01/findings/luna-daemon-be8b-exact-review.md).
- Findings: source identity hashed protocol source while loading protocol dist; observer provisioning bypassed freshness/election gates; status and doctor disagreed on ambiguous discovery.
- Preserve schema-v1 ready-receipt compatibility, typed stale evidence, no signalling/terminal marking/second daemon for a live stale incumbent, and the measured roughly 0.19-second identity cost.

### Worker host boundary

- Worktree/head: `worker-outcome-consolidated` at `5556365a` after `f6a8e084`; PID `69123` owns the uncommitted repair.
- Critical Opus/Sol adjudication: [sol-worker-host-boundary-followup.md](../../.agent-run/baseline-2026-08-01/findings/sol-worker-host-boundary-followup.md).
- Required posture: workflow does not self-certify or auto-apply; it ends at `awaiting-host-certification/application`. The host finalizer certifies immutable attempt evidence, and the ordinary host chair applies/verifies/commits.
- First repair suite exposed 38 failures because component-wise no-follow rejected macOS `/var -> /private/var`. The active Luna is binding a canonical trusted root/parent descriptor while retaining no-follow traversal below that boundary.
- This branch overlaps provider changes in `workflows/implement-run.js`, `cf_dispatch.sh`, finalizer and tests. Integrate it last or deliberately resolve the overlap so canonical provider assurance and immutable host evidence both survive.

## Live Fabric failure and recovery gate

- Installed daemon PID at last probe: `12858`, four days old, launched from source.
- Last evidence: 185 descriptors, including 142 Unix sockets. Earlier admission ceiling was 32.
- Both `provenant project status` and `provenant fabric workspace status` hung until interrupted.
- This confirms the overload/stale-daemon problem; it is not a test-only defect.
- Do not restart/replace PID `12858` merely to make status green before the daemon-freshness repair is independently accepted and landed. Preserve before-recovery evidence.
- After landing the fence, perform a safe installed-daemon recovery, then prove `doctor`, status, whoami, mailbox and request/reply. The old client/new daemon path must remain compatible or fail with typed recovery evidence.
- Fabric trust is exact-root only. Never trust a parent, home, wildcard or sibling collection.
- `FABRIC-ROUNDTRIP-UNAVAILABLE`: current answer-bearing lanes used direct-Luna artifacts because the installed daemon could not round-trip. Fabric certification is still mandatory before final acceptance.

## Tracker and architecture evidence

- Live chair fetch at handoff: 38 open issues and 7 open PRs.
- Full Luna matrix: [luna-live-github-reconciliation-final.md](../../.agent-run/baseline-2026-08-01/findings/luna-live-github-reconciliation-final.md).
- Earlier architecture review: [sol-integration-architecture-review.md](../../.agent-run/baseline-2026-08-01/findings/sol-integration-architecture-review.md).
- Worktree provisioning adjudication: [sol-585-630-final-adjudication.md](../../.agent-run/baseline-2026-08-01/findings/sol-585-630-final-adjudication.md).
- That adjudication rejects automatic install/build inside generic worktree creation. Keep creation structural; use caller-bound Provenant setup, scratch-local preflight and install-free host Git verification.
- Recurrent clusters from the tracker: trust/project boundaries; daemon lifecycle/custody/transport; provider identity/assurance/fallback; gate/result/evidence contracts; delivery/worker terminal proof.
- High-value simplification questions: one provider-assurance contract; one host certification boundary; one test-bound/fixture helper; issue closure mechanically tied to merged evidence; smaller issue-scoped PRs rather than cross-cutting branches.

## Ordered continuation

1. Reopen `AGENTS.md`, `HARNESS.md`, relevant skills and this handoff; verify all heads, diffs, PIDs and GitHub state rather than trusting this snapshot.
2. Run the Opus architecture/minimality panel described above and publish one evidence-backed adjudication. Replan if it finds a smaller common repair.
3. Collect the three live Luna writers. Independently review their exact final diffs with non-authoring Opus subagents; send bounded Luna repairs for real findings.
4. Verify and commit the paired-completion and trust-provenance uncommitted diffs on coherent dependencies.
5. Serially integrate accepted provider, paired-completion, daemon, trust and worker branches into `baseline-integration`, resolving overlaps deliberately and rerunning focused checks after each landing.
6. Address the still-open architecture P1 from issue #607: mandatory review certification must go through Fabric and bind the exact provider prompt/output/route evidence. Do not recreate a parallel direct-CLI certification path.
7. Run the full root build/typecheck/generated-schema/TypeScript/Python/harness matrix and targeted race/load tests. Classify sandbox or stale-dependency failures explicitly.
8. Recover the installed daemon safely and prove real Fabric request/reply across Claude and Codex.
9. From fresh arbitrary Git and non-Git directories, prove zero-touch project activation and automatic Fabric registration/use for both clients. Verify exact-root trust and no parent widening.
10. Use Agy/Gemini plus fresh Luna and Opus for final cross-family/other-primary review. Record unavailable legs honestly.
11. Reconcile every GitHub issue/PR with merged evidence; update, supersede, close, push and merge under the user's authority. Keep split issues #563/#565 gated until the fused baseline is accepted.
12. Present the user with the stable-baseline evidence and ask for acceptance before the repository split.

## Useful commands

```sh
git -C $AGENTS_HOME/.worktrees/baseline-integration status --short --branch
git -C $AGENTS_HOME/.worktrees/baseline-integration log --oneline main..HEAD
for w in provider-integrated-final-fix paired-completion-608 daemon-build-freshness-fix auto-trust-provenance-fix worker-outcome-consolidated; do
  git -C "$AGENTS_HOME/.worktrees/$w" status --short --branch
  git -C "$AGENTS_HOME/.worktrees/$w" diff --check
done
ps -p 72869,6908,69123 -o pid=,etime=,time=,command=
lsof -p 12858 | wc -l
gh issue list --repo mblauberg/provenant --state open --limit 200
gh pr list --repo mblauberg/provenant --state open --limit 100
```

Exact verification commands for each lane should be taken from its worker report and owning package scripts after dependencies are prepared. A new worktree may need `uv sync --locked --only-group test`; missing root `node_modules` or stale standalone protocol links are environmental until reproduced on a coherent dependency graph. Do not leave temporary dependency symlinks behind.

## Chair transfer

The handoff file is the transport of record. Fabric request/reply could not perform the transfer. Herdr steering is only a wake-up and does not prove consumption. The incoming Claude chair must mark `Consumed-at` with a timestamp and update `docs/handoffs/README.md` after reopening and verifying this state.
