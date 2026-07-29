# Agent fabric invariant catalogue

Status: enforced
Owner: `runtime/agent-fabric`

This catalogue maps durable claims to their enforcement and regression
evidence. `0001-current-baseline.sql` is the complete pre-release schema. Its
manifest pins both the SQL digest and canonical SQLite catalogue digest. A
pre-existing database that is not that exact baseline is inspected read-only,
preserved and rejected with `SCHEMA_CUTOVER_REQUIRED`; it is never imported,
backfilled or rewritten.

| Invariant | Enforcement | Primary evidence |
| --- | --- | --- |
| The runtime ships one current schema baseline and no incremental importer or compatibility migration | manifest-pinned `0001-current-baseline.sql`, `fabric_schema` and cutover inspection | `schema-baseline-custody.integration.test.ts`, `migration-runner.integration.test.ts`, `package-migrations.acceptance.test.ts` |
| Provider adapter admission depends on current identity, handshake or protocol negotiation, and capability checks, never provider version or digest equality | `adapter-compatibility.yaml`, adapter compatibility and provider conformance modules | adapter compatibility acceptance, daemon adapter gate integration and status CLI tests |
| Agent, task, delivery, lease, provider-action, team, budget, objective-check, scoped-gate and barrier states use closed enumerations | current-baseline checks and triggers | `persistence-invariants.integration.test.ts` |
| Boolean fields are SQLite `0` or `1`; generations, counts and budget quantities stay in range | current-baseline checks and triggers | `migration-runner.integration.test.ts`, `persistence-invariants.integration.test.ts` |
| Authorities, agents, tasks, messages, deliveries, leases, events and provider targets do not cross run boundaries | current-baseline foreign keys and insert/update triggers; typed spawn/attach custody preserves creation order | `persistence-invariants.integration.test.ts` and acceptance suites |
| One process owns mutations for a database and socket | OS-backed SQLite owner locks and safe path checks | `daemon-single-instance.integration.test.ts`, daemon CLI acceptance tests |
| Mailbox, task, lease, event and unresolved-provider reads use bounded indexed paths | current-baseline partial/composite indexes and query-plan assertions | `persistence-invariants.integration.test.ts` |
| Unclean startup is detected and checked before service | private unclean marker, `quick_check(1)` and first `foreign_key_check` result | `sqlite-connection-hardening.integration.test.ts` |
| Long-lived SQLite state receives bounded maintenance | `PRAGMA optimize = 0x10002` at open | `sqlite-connection-hardening.integration.test.ts` |
| Wire frames, connections and in-flight work cannot exceed global maxima | shared bounded NDJSON parser, protocol handshake and daemon/client/adapter admission limits | `bounded-ndjson.unit.test.ts`, `daemon-transport.integration.test.ts`, adapter process tests |
| A client cannot issue fabric methods before agreeing protocol v1 | mandatory `initialize` negotiation | `daemon-transport.integration.test.ts` |
| Workspace admission is exact, canonical, private and profile-bound | machine-local trust registry plus portable-config narrowing | `workspace-trust.unit.test.ts`, configuration tests |
| Retention never implies deletion authority | report/preview output fixes prune eligibility to false; archive is receipt-copy only | `retention-cli.unit.test.ts` |
| Re-exporting unchanged committed state is byte-identical | receipt v2 canonical snapshot; export time stored separately | `receipt-export.acceptance.test.ts` |
| Status and doctor never print bearer capabilities | metadata projection omits credential paths and values | `status-cli.unit.test.ts` |
| Every automatic lifecycle action appears in one machine-readable receipt | `bootstrapMcpSeat` returns a typed action receipt covering trust, daemon, seat generation, first-recorded legacy bootstrap provenance and the bounded identity/mailbox smoke; the CLI and the `fabric_bootstrap` tool relay it | `mcp-bootstrap-legacy-provenance.unit.test.ts`, `zero-touch-lifecycle-receipt.test.ts` |
| A converged repeat invocation changes no logical custody state | the receipt reports no mutating action; the seat-root bytes, inode timestamps and `generations/` entry set are unchanged, as are the custody row counts and daemon PID. `mutated` is not a claim about bytes written: a replay still stages and discards a private tree | `zero-touch-lifecycle-receipt.test.ts` |
| Replaying an already-marked legacy generation does not rewrite its marker | `markLegacyBootstrapSeatGeneration` returns before writing when the marker already records that generation | `seat-store.unit.test.ts`, `zero-touch-lifecycle-receipt.test.ts` |
| The identity/mailbox health smoke cannot report success past its deadline | one whole-smoke budget covers connect, `whoami` and `mailbox.read`; settling is bounded under a responsive event loop and the reported outcome is bounded by measured elapsed time | `zero-touch-lifecycle-receipt.test.ts` |
| `doctor` names why a lifecycle state holds, not only which state it is | every check declares its precondition; `cause` names the worst check and `satisfied` tracks that check rather than the health rollup, so an unknown precondition is never reported satisfied | `status-cli.unit.test.ts` |
| `recovering` names only states an ordinary bootstrap converges | converging transitions, active discovery with a dead PID, and terminal crashed/unclean-stop discovery; absent discovery, stale and unreachable sockets and unrecognised codes are `blocked` so a zero-touch caller cannot retry forever | `status-cli.unit.test.ts` |
| A stale protocol build is classified by `doctor` rather than blocking it, and blocks every other entrypoint | the launcher passes the shared preflight's stale verdict and exact repair into `doctor`; non-doctor commands and the MCP wrapper still exit 78. `doctor` alone also blocks on a dist it cannot import, because it cannot report through one; the probe is scoped to it so no other entrypoint pays a Node start-up per invocation | `status-cli.unit.test.ts`, `test_agent_fabric_launcher.py` |
| Bootstrap and its retry paths never invoke a schema cutover | a schema mismatch raises a typed gate carrying the retained-work census, consequences and the exact `database archive-and-fresh` command, and leaves the SQLite source set byte-identical | `zero-touch-lifecycle-receipt.test.ts` |
| A cutover publishes no archive and displaces no source without a recorded verified-interactive or asserted-unattended approval | the pre-mutation approval gate in `database-archive-and-fresh.ts` and the module-issued interactive identity and canonical approval receipt in `database-cutover-approval.ts` | `database-archive-and-fresh.integration.test.ts`, `cli-database-archive-and-fresh.acceptance.test.ts` |
| A stale protocol build is preflighted before any election mutation | `attachOrStartDaemon` runs the typed preflight before the election lock, so no runtime directory is created | `bootstrap-client.test.ts` |
| One current MCP roster binds exact project/session/run/chair and principal generations; activation revokes the predecessor and stale tokens fail at point of use | daemon-owned seat-generation prepare/activate CAS plus locked filesystem pointer CAS | `mcp-provision.acceptance.test.ts`, `seat-store.unit.test.ts`, `mcp-credentials.unit.test.ts` |
| A run has at most four active team leaders across every supported depth | transactional Fabric admission plus current-baseline trigger defence | `stage5/team-hierarchy.acceptance.test.ts` |
| A task-bound answer-bearing provider review returns only a validated bounded answer and canonical result digest | closed provider-action codec, exact task/authority admission and adapter capability check | provider-action and MCP acceptance tests |
| Typed Git grants preserve exact human-input provenance and are rechecked after asynchronous observation | operator-action input-record digest propagation plus final authority/grant/gate/profile/remote/writer claim transaction | `operator-projection-actions.test.ts`, `typed-git-service.test.ts` |
| `workspace-write-offline` is an internal exact-workspace projection, never a caller-selectable profile; an attempted use reuses provider-action and adapter-journal custody | authority compiler rejects payload selection before workspace resolution; adapter projection parser binds the exact root and fixed offline controls before journal preparation or dispatch; Codex threads stay read-only, exact write authority exists only on its admitted turn, and ordinary turns restate read-only | `authority-compiler.unit.test.ts`, `primary-provider-adapters.unit.test.ts` |
| A fixed local Git mutation starts only with a verified native first-mutation fence | no-follow pinned path/index bytes, native `index.lock`, atomic index install, `update-ref` old-object CAS, or an exact 0700 worktree-destination reservation followed by `--no-checkout`; every other fixed-port variant reports `CAPABILITY_UNAVAILABLE` before custody | `fixed-git-mutation-port.test.ts` |
| Git profiles, secret-free remote targets and run allow-lists come only from trusted daemon composition | production child composition forwards only typed trusted Git configuration; `TrustedGitRegistry` digest-checks profiles/remotes, defers configured run allow-lists until their exact authority tuple exists, and persists every registry/allow-list revision behind immutable triggers | `production-bootstrap-wire.test.ts`, `trusted-git-registry.test.ts`, `migration-runner.integration.test.ts` |

The bundled fixed local mutation port currently admits `stage`, `unstage`,
`commit`, `branch-create` and the three `worktree-create-*` variants. Worktree
creation pins the exact source object/ref, owns one canonical direct-child
destination and never materialises checkout content. Remote effects, pull,
merge/rebase and successors, branch rename/delete, worktree move/remove and
upstream configuration remain in the closed protocol vocabulary but return
typed unavailability unless trusted daemon composition supplies a port with the
required native/remote CAS and deterministic-result contract. The Console must
display that unavailable state; it must not fall back to porcelain or arbitrary
Git execution.

The selected machine database can also be checked without provider execution:

```sh
scripts/agent-fabric doctor --json
scripts/agent-fabric retention preview
```

No trigger or test grants destructive retention, remote listening, a second
transaction owner or provider-session mutation authority.
