# Spec: harness progressive disclosure

Canonical decision: [ADR 0020](../../adr/0020-retire-the-daemon-fabric.md)
owns current Fabric wording. Git owns the refactor's decision and migration
history; `.agent-run/` artifacts are not normative dependencies.

## Requirements

1. The ambient layer contains only `AGENTS.md` and `HARNESS.md`. `AGENTS.md`
   stays minimal; `HARNESS.md` is the compact constitution for topology,
   lifecycle, user gates, risk, Git, routing and memory.
2. Operational depth has one skill owner: compaction and checkpoints in
   `session`, routing and degradation in `orchestrate`, and receipt detail in
   `deliver`. Ambient files name skills, then load depth only when triggered.
3. Nothing outside an owning skill names a file under its `references/`
   directory. Cross-skill references use the skill name only. The
   `natural-writing` hub owns shared prose doctrine.
4. `AGENTS.md` and `HARNESS.md` contain no repository-relative
   `docs/`, `config/` or `scripts/` paths. Runnable commands are
   PATH-resolved. Fabric identity derives from the working directory.
5. `HARNESS.md` is the sole ambient skill resolver. It names the installed
   Claude and Codex skill roots; `AGENTS.md` names neither an installed root
   nor an instance skill directory. A named skill loads its `SKILL.md` and is
   binding, while provider-native discovery may implement resolution.
6. Ambient files carry no dates. Git records revision provenance. Source
   changes that contradict a requirement re-open that requirement against the
   current owner; they do not revive a migration record.
7. `orchestrate` remains one skill. Skill changes use targeted,
   evidence-backed pruning; no new catalogue entry or duplicate policy is
   introduced without its own accepted scope and ownership boundary. Frozen
   held-out evaluation is conditional under ADR 0014; trigger fixtures and
   machine-enforced contract tests remain mandatory.
8. The repository process and GitHub mechanics live on repository-scoped
   surfaces. Durable decisions and specifications precede dependent
   implementation; `session` owns temporary handoff lifecycle. Provider
   workflow installation and source custody follow their owning installation
   decision and tests.

## Acceptance

Static final-tree requirements:

- Ambient files contain no dates, repository-relative `docs/`, `config/` or
  `scripts/` paths, or `skills/<x>/references/` paths outside the owning skill.
  Cross-references use skill names. The resolver line exists only in
  `HARNESS.md`; PATH-resolved `provenant` invocations are not location-bearing.
- The final skill catalogue stays within its approved cap and is reviewed
  against the source catalogue.
- The former migration inventory is not a permanent full-tree checker;
  owner-specific tests enforce the surviving machine invariants.

Per-PR checks:

- `scripts/check-harness`, `scripts/static-security-check.py`,
  `scripts/public-release-check`, `git diff --check`, and the change's focused
  tests pass.
- Isolated installer fixtures cover Claude and Codex clean install, upgrade of
  a harness-managed file, and an existing unmanaged instructions file. They
  verify expected exit codes, link and manifest state, and byte-identical
  preservation of unmanaged content.
- From an isolated install, every ambient skill name resolves to its installed
  `skills/<name>/SKILL.md` on both platform layouts, including the Codex mirror.
  This is an install/discovery contract; live model-routing behaviour remains
  subject to ADR 0014's detection-in-use policy.

## Current ownership

The declared issue tracker owns current scope and stories. This specification
owns progressive-disclosure requirements and acceptance; code and tests own
implemented structure, ADRs own rationale, and Git owns change history.
