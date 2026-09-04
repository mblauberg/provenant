# Spec: configurable multi-provider execution core

Canonical requirements specification for the Provenant execution core. It is
the merged baseline, not a plan: every requirement below describes shipped
behaviour, and GitHub owns whatever is still outstanding. This
file grants no authority; current user and project instructions plus an
authorised run are still required.

Promoted from issue #681, whose 26 sub-issues delivered it. GitHub owns
delivery state; this file owns the durable requirements.

## Outcome

Provenant stays a small, configurable provider-execution core with composable
skills around it. A chair authorised over a workspace may use any configured
provider for ordinary relevant workspace work, including same-family Luna or
Gemini swarms, without per-family consent or a trust ceremony.

Provider output is retained once in files and returned to the chair as a
compact status and a path. Stronger independent or cross-family assurance is
opt-in and claims only what actually ran.

## Baseline

The merged implementation provides:

- one configured-provider dispatch owner;
- fixed bounded read-heavy batches with partial results;
- file-backed prompts, results and diagnostics;
- inspect, retry, reduce, blocked-question and cancellation controls;
- actual provider, model, status and process-exit reporting;
- serial writers and separately authorised implementation worktrees;
- project-scoped Fabric tasks, messages and activity shared by ordinary linked
  worktrees;
- exact-checkout `provenant check` routing and an early current-worktree
  dependency preflight;
- thin Fabric MCP dispatch and batch tools over the existing execution owners;
- an operational split between the product checkout and the installed instance
  root, without an automatic live move.

Fabric owns project coordination and the thin MCP entry surface. The existing
orchestration owners and provider adapters still own execution, attempts,
cancellation and retained output. Governed delivery stays separate from
lightweight advisory dispatch.

## Requirements

- Ordinary dispatch and bounded batches stay provider-family agnostic and
  output-file first.
- The MCP surface delegates to the existing dispatch, batch and Fabric owners
  and returns compact file references.
- Named provider sessions use provider-native continuation where the provider
  supports it, without resident manager processes.
- Working agents exchange and await Fabric messages without direct database
  polling or background watchers.
- Assigned writers may work in their designated file or isolated worktree;
  concurrent shared writes stay rejected.
- Assurance requirements stay optional and are reported accurately.
- Current docs and issues describe the merged behaviour and the remaining work.

## Non-goals

- No scheduler, daemon, recursive swarm manager or second lifecycle engine.
- No universal hashes, environment fingerprints, version locks or catalogue
  registry.
- No routine redaction, per-provider confirmation or family restriction for
  authorised workspace content.
- No mandatory full delivery receipt for scouts, brainstorming or ordinary
  advisory batches.
- No concurrent writer batch without a demonstrated workflow that cannot use
  serial or isolated writers.

## Related decisions

- [ADR 0019](../../adr/0019-installed-file-class-ownership.md): installed
  file-class ownership by product, instance or seeded template.
- [ADR 0020](../../adr/0020-retire-the-daemon-fabric.md): the daemonless Fabric
  runtime this core dispatches through.
- [`runtime/fabric/README.md`](../../../runtime/fabric/README.md): the current
  Fabric command and configuration surface.
