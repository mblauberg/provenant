# Context hygiene

Context is a cache, not the source of truth. Keep the smallest current briefing
that can route an agent back to owned evidence; archive durable history and
remove only ephemeral material whose ownership is proven.

## Three tiers

1. **Current routing context** — project instructions, approved spec/design,
   open decisions, canonical work-tracker links and the stable effort route
   map. Keep it short; follow the linked owners for current state.
2. **Recoverable run evidence** — manifests, synthesis, receipts, accepted
   findings and verification records. Preserve paths and provenance; a fresh
   agent should not need raw logs to resume.
3. **Ephemeral payload** — raw transcripts, copied command output, caches,
   temporary patches and superseded worker drafts. Bound while active; retire
   after useful conclusions have graduated.

Never promote tier 3 wholesale into tier 1. Compress reversibly: the summary
names its evidence path, source version/date and invalidation trigger.

## Lean dispatch and compaction

- Send a bounded assignment packet: objective, authority, inputs by path/hash,
  output contract, checks, stop condition and deadline. Do not paste the whole
  parent transcript or repeat project instructions already discoverable.
- Workers return a delta: verdict, supported claims, changed/artifact paths,
  checks and blockers. Large reasoning and command output land in a named run
  artifact; the parent receives its path and digest, not a transcript dump.
- Fan-out agents do not recursively inherit unrelated worker findings. Cross
  pollination happens at an explicit reducer/barrier using curated artifacts.
- Before model compaction, write a checkpoint with current stage, authority,
  base/result revision, in-flight actors, accepted/rejected decisions, evidence
  pointers and exact next action. A cold-resume agent must re-open owners and
  verify the checkpoint before dispatching.
- Claude may use `/compact` only after that checkpoint; `/clear` requires a
  durable handoff or completed stage barrier. Codex uses host compaction or a
  fresh session against the same checkpoint. Never treat compacted prose as
  authority.
- Pair messages are wakeups and deltas, normally under 4 KiB. Put long prompts,
  review bodies and plans in namespaced artifacts and send `path + sha256 +
  requested action`.

## Checkpoints

Run a hygiene pass before compaction or handoff, after a large multi-agent wave,
at substantial-change closure, and when any of these signals appears:

- rolling state exceeds about 120 lines;
- an agent-facing Markdown file exceeds about 15 KB;
- an effort trail exceeds about 20 entries;
- a live directory exceeds about 25 entries;
- a raw log exceeds the run's declared size budget;
- two live files claim the same owner or truth.

Use the read-only auditor from any project root:

```sh
python3 "<installed-session-skill>/scripts/context_audit.py" .
```

Replace `<installed-session-skill>` with the absolute skill directory disclosed
by the client that loaded this skill.

For a canonical `delivery-run` receipt, preview expired manifest-owned scratch with
`cleanup_run_artifacts.py RUN.json`. Execution additionally requires
`--execute --authorised-by ... --authority-evidence ...`; it never removes
unknown, canonical, evidence, handoff or external artifacts.

Thresholds are signals, not deletion authority. Project instructions may tune
them. Use `--json` for a receipt. Structural errors fail by default (`--strict`
is a compatibility alias); `--warnings-as-errors` is an explicit project-level
adoption of advisory caps.

The project root's `.worktrees/` is protected Git infrastructure, not context
or scratch. The auditor does not descend into sibling checkouts. Inspect and
remove an authorised worktree from its own root with
`provenant worktree`; never prune its directory as
an ignored cache or delete it with a filesystem command.

## Freshness and invalidation

- Rolling state and context digests declare `Updated` or `Last verified` near
  the top. Their claims point to the current owning file, command or external
  source.
- Refresh means re-open the owner and reconcile the existing summary. Do not
  append a second version. Mark contradicted claims superseded or remove them
  from current context while preserving history in the archive.
- Time-sensitive facts state an `as of` date and an invalidation trigger such
  as dependency upgrade, schema migration, policy change or new evidence.
- Harness-private memory never certifies current project state. Project-owned
  docs win, and a stale or unverifiable claim is labelled stale/unknown.

## Harness-private memory

Use private memory only for genuine cross-project user preferences. Put
project knowledge in its durable owner, such as a project instruction, spec,
ADR or runbook, and keep moving status in the work tracker or state file.

Keep the memory store plain and easy to edit:

- write one fact per Markdown file;
- give it small frontmatter with `name`, `description` and `metadata.type`;
- add a one-line pointer to the file in `MEMORY.md`;
- update a changed preference by editing the file, or replace it and note the
  supersession while updating the index.

Before writing, check that the fact contains no secret or credential, PII, or
transient project or run status/result. If it does, omit it or put the durable,
non-sensitive knowledge in its proper project owner. Memory changes are normal
file edits; they need no auxiliary approval artifacts or lifecycle workflow.

## Split or merge

Split a live file when it mixes owners, audiences, lifecycles or change rates,
or when stable reference bulk hides the current decision/state. Keep a compact
index at the old routing surface and make the canonical owner explicit.

Merge files when they duplicate the same truth, are always changed together,
or force an agent through several tiny hops without adding an authority
boundary. Preserve redirects/tombstones for live links. A user should reach
current claim -> owner -> evidence in at most three hops.

## Cleanup and retention

At closure, classify every run artifact:

- **graduate** durable conclusions into their existing owner;
- **retain** the minimal manifest, synthesis, verification and failure receipts;
- **archive** consumed handoffs, superseded durable drafts and completed effort
  maps according to project policy;
- **remove** caches, duplicate captures and raw payload only when this run
  created them, no manifest/current doc points to them, and the retained
  synthesis is sufficient.

Never delete unknown, user-owned or pre-existing untracked files. Never hide a
failed leg: retain its status, reason and any evidence needed to reproduce it.
Linked worktrees remain protected until Git reports them clean, their owning
agents/panes have stopped, and their durable handoff has been consumed.
For logs, prefer tool-native rotation or a new bounded file; do not rewrite an
append-only audit log. Secrets and sensitive raw payload follow the project's
data-retention policy, not this generic contract.
