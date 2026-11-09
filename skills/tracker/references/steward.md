# Steward and closing

Inspect the bundled brief with `provenant templates steward --json`.

## Scheduled proposal sweep

Enable repositories on the host that should run their sweeps. Add one small
section to that host’s `<state>/config.toml` (create it with
`schema_version = 1` if absent):

```toml
[domains.steward.repositories.tracker]
directory = "/absolute/path/to/checkout"
cadence_seconds = 86400
```

The map key is a local alias. Each entry selects a checkout and a positive
cadence in seconds; no repository-specific code or template is needed. The
section is host-local: adding it on one host does not enable other hosts. Keep
one scheduling host per repository. Without entries, `provenant tick` reports
`steward=idle` and launches nothing. Remove an entry to stop scheduling future
sweeps; use `provenant stop TARGET` for a graph already launched.

Run `provenant tick --json` through the host’s existing tick schedule. A due
entry submits `use = "steward"` through the ordinary graph service, with the
bundled junior tier. Tick reports `launched`, `running`, `not_due` or `failed`;
a failure includes its reason. Scheduling state is durable and shared by all
aliases of the same project. A live or paused sweep prevents a second run, even
after its cadence expires. Uncertain submissions reuse their operation identity.
Inspect and recover paused or failed graphs through `status`, `output` and
`retry`; tick does not bypass graph admission or provider policy.

Inspect the brief with `provenant templates steward --json` or
`provenant run --use steward --explain`. Manual runs use the same template;
the tick single-flight guard covers scheduled runs. The brief discovers the
repository from its checkout, gathers hybrid-search candidates with `is:issue`,
then judges their bodies and acceptance criteria. Its only tracker write is a
proposed-duplicate comment on the newer issue. Comments contain a pair-specific
`<!-- provenant-steward:duplicate-of=OWNER/REPO#NUMBER -->` marker; the sweep
reads all comments before posting and skips marked proposals. It never closes
issues. High-confidence parent links and thin bodies stay in graph output for
the chair; parent application needs a separately authorised run.

Validate adoption with `--explain` before enabling the section. Building and
testing the scheduler requires no live tracker writes. A configured sweep does
authorise its proposal comments, so enable it only on the intended host.

## Sweep checks

- **Stale:** a referenced PR merged, a dependency closed or cited evidence moved.
  Re-verify against current state, then correct or close with the reason.
- **Orphaned:** the parent closed or the sub-issue link no longer resolves. Re-parent
  or make it standalone.
- **Mis-closed:** closed with no merge evidence or stated reason. Reopen, or add the
  missing evidence.
- **Superseded:** close as duplicate-of or not planned, but only after comparing its
  acceptance to the merged code.
- **Thin:** a body a stranger cannot act on, or one citing scratch paths. Fix or flag.

## Delegation

The chair gives each steward the repository, issue and PR links, the authorised
actions and a stop condition. Before retrying any write, read current issues and
PRs with `gh`: a local record cannot prove a remote write happened
exactly once. If the outcome stays uncertain, report it before writing again. A
steward returns at most 15 lines: actions taken, links, no-ops, decisions needed.
