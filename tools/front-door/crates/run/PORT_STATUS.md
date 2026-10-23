# Run port status

Inert library under PORTING.md version 4. No command uses this crate.
Implementation route: `codex/gpt-6.1-sol@high` via Fabric. Source/oracle revision:
`7021182d535f6c730c29713e77d5bca76893fd06`.

| Unit | Status | Contract |
| --- | --- | --- |
| `skills/orchestrate/scripts/run_lineage.py#interface` | Existing interface | Parent/child shapes, validation, journals and cancellation capability contract |
| `skills/orchestrate/scripts/lease.py#unit-01` | Partial | Lease transitions, generation fencing, retained fields, contained handoff and atomic publication |
| `skills/orchestrate/scripts/run_worker_detached.py#unit-01` | Partial | Argument and completion evidence policy, claim/publication effects, execution over an owned-worker seam |

Detached execution is blocked on the supervisor's safe session-worker owner.
`supervisor::bounded` is a utility runner that cleans up descendants and cannot
replace a wrapper whose death leaves its worker running. The missing API must
spawn a fresh session, attach null stdin and one shared append transcript,
retain identity through exit/reap, and expose bounded exit observation and
owned-worker SIGTERM cleanup. `pending::{SessionHost, SessionWorker, WorkerExit,
ProcessProbe}` describe that seam. Each wait call is bounded; the source places
no total deadline on a healthy worker. No observation timeout authorises a stop.

`pending::publish_marker` remains because the main safe FS API does not expose
the source's deterministic `.tmp.<pid>` writer with default creation modes and
no fsync. It must not be substituted with the private-mode durable lease
writer. The supervisor also owns the remaining custody capability and bounded
path reader; core owns lexical parent selection and Unicode digit predicates.
The digit table is pinned to Unicode 16.0.0, matching the Python 3.14 oracle.

Run lineage and lease records belong to this crate under the graph; they are
not generic records helpers. Python value formatting, equality and truthiness
use `provenant_core::py`; JSON ingress uses `provenant_records::checked_json`.
All run differentials share one interpreter gate, honour
`PROVENANT_PORT_PYTHON`, use `-I` and an explicit environment, print `skip:`
when unavailable, and fail when `PROVENANT_PORT_DIFF_REQUIRED=1`.

The detached tests cover startup grace, all marker refusals, Unicode digits,
invalid UTF-8, contention, failed exec, null stdin, merged output, signal exit,
publication failure and owned-worker cleanup. Production session ownership,
wrapper-death and PID-reuse conformance remain supervisor integration gates.
J1/J2 and Linux/macOS process conformance still gate cut-over. The Python
sources remain the runtime owners.

Lease publication now uses supervisor safe filesystem handles. The remaining
`pending::lock` needs a supervisor append/open-or-create operation with Python's
default creation mode, final-symlink following and blocking flock semantics;
`create_new(0600)` plus `try_lock` does not provide that contract. The typed CLI
still needs exact argparse abbreviation/help/diagnostic parity and arbitrary
surrogateescape option values. Non-finite constants and lone-surrogate JSON
strings need a Python-compatible dynamic record boundary; checked JSON remains
strict. These gaps keep lease off the completed-unit list. Retained state and
handoff reads now have a 1 MiB bound (RP-RRC3-001); cut-over needs an explicit
boundary decision. Unicode decimal parsing also awaits a core owner API.

The pre-existing lineage interface is a shape contract, not a completed
implementation unit. Its permissive unknown-field NaN/Infinity handling and
ordered object diagnostics remain implementation gaps: serde values reject
those constants and sort object keys. This lane moves its domain types out of
`pending` and adopts shared numeric/formatting helpers without claiming the
implementation complete.

`pending::epoch_seconds` also awaits the core SystemTime-to-Python-float clock
boundary (the existing core helper converts float seconds to datetime, the
opposite direction). Pure completion validation accepts the observed float age;
full platform timestamp conformance remains part of the partial OS adapter.
