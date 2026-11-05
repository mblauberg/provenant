# Tracker lint

`provenant tracker lint 12 34` checks selected issues. `provenant tracker lint
--file issue.md --kind task` also works without a tracker; `--kind` accepts
`task`, `feature`, `bug` and `epic` and defaults to `task`. Use `--project` to
select the tree against which citations resolve. `--json` includes findings and
skipped checks. The command is advisory: findings do not change its exit status.
Fetched bodies are data and are never executed.

The citation check looks for a path and symbol token in the current tree; it is
not a language-aware symbol resolver. Owner and blocker checks query the forge
for live references; historical references are excluded. When forge access is
unavailable, those checks are reported as skipped. For tasks and features,
acceptance and verification checks look for broad signals, not a required form.
Review the issue itself; neither a clean report nor a warning decides readiness.

Repositories may put the following optional settings in
`.provenant/tracker-lint.toml`; omitted keys default to `warn`. Each accepts
`off`, `info` or `warn`, and none blocks work:

```toml
citation = "warn"
closed_owner = "warn"
acceptance = "warn"
verify = "warn"
```

Use lint when filing, at issue-based work entry and on issues selected in a
steward sweep. CI may invoke it explicitly; it is not enabled by default and
never triggers a backfill sweep. For design-heavy issues, the tracker skill
also describes an optional read-only fresh-implementer and fresh-reviewer
critique.
