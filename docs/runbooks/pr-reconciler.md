# Pull request work records

Owner: Workflow maintainers
Anchor: [`reconciler.rs`](../../tools/front-door/crates/workflow/src/reconciler.rs)

`pr-reconciler` runs signed event intake and authoritative sweeps in one
observe-only worker. It writes proposed decisions, local escalations and daily
digests. The durable work store remains a separate building block: observation
never starts attempts, publishes verdicts or issues merge intents.

Automatic actions remain parked under [#2873](https://github.com/mblauberg/provenant/issues/2873).
`mode: "act"` is rejected before reading service state or calling the forge.
Live observation and installed restart evidence must precede any action adapter.
The existing delivery graph starts new implementation and a new PR; it is not
an adapter for resuming arbitrary existing PRs.

## Durable work store

A key contains repository, PR, exact head, policy digest, lens and base digest.
The caller opens a SQLite connection at its configured state location, with its
chosen busy timeout. Writers use immediate transactions. Observe an authoritative
current head before dispatch, including after restart; never treat the head in an
unordered event as current without checking the forge. Observing a different head
cancels older records atomically. A cancelled record cannot issue or retry an
intent, and its verdict never transfers to another key. Reclaim an expired lease
on the existing attempt. When a cancelled head returns, observing it archives its
previous attempt and creates fresh work for that same exact key. Its archived
intents without receipts remain available for reconciliation, including settled
absences. On a return, archives whose intents all have receipts are pruned;
compact ID tombstones prevent attempt and intent IDs from being reused within
the key.
Before starting an attempt, reconcile every unresolved intent on other heads and
previous attempts of the same head.
An unknown outcome blocks the start; a receipt or a settled, authoritative
absence resolves it. Cancelled, archived or terminal work cannot use that absence
to retry.

| State | Entry and next step |
| --- | --- |
| needs-review | A new or returning key; start its current attempt. |
| reviewing | Attempt holds a lease; record its verdict or fail. |
| verdict | Persist a verdict publication intent, reconcile its receipt, then await checks; or fail. |
| awaiting-checks | Adapter evaluates checks on the exact head; mark landable or fail. |
| landable | Adapter applies landing policy and records a merge intent; or fail. |
| landing | Reconcile the merge intent. |
| landed | A remote merge receipt was observed. |
| failed | Terminal for this attempt. |

Persist an intent before issuing an effect. After a crash or an unknown response,
query the remote by the intent identity and exact head before retrying. `Unknown`
never permits retry; `Absent` means authoritative absence after any in-flight
operation has settled. An observation carries the intent generation read before
the remote lookup; a delayed observation cannot authorise retry after the
generation changes. Retry consumes that observation durably before the call, so
another crash again requires reconciliation. A found receipt is durable and
idempotent. Reconcile outstanding intents even for cancelled work to record an
operation that raced with a head change; cancellation never authorises retry.

Adapters must pin the head at the remote publication/merge boundary, enforce
exact-head approval and all existing landing gates, including owner acceptance
for terminal heads. They must recheck remote head and authority at that boundary:
a local cancellation cannot retract a request already in flight. Intent IDs must
be stable and remotely discoverable; if a forge cannot prove absence or identify
an effect, leave it unknown rather than retrying. No approval carries to a new
head. Lease duration, clock and database path are supplied by the caller.


## Observe-only worker

Owner: command composition edge
Anchor: `tools/front-door/src/bin/pr-reconciler.rs#run`,
`tools/front-door/src/pr_service/observer.rs#Observer`

Release archives include `bin/pr-reconciler` beside `bin/provenant`. Invoke the
worker by its absolute immutable release path; installation does not register or
start a PR service. For checkout builds, build both binaries from the same tree:

```sh
cd tools/front-door
cargo build --locked --bin provenant --bin pr-reconciler
```

The following commands are for the chair on a configured host. Replace the four
absolute paths and the test repository; use a fresh private state directory and
a repository authorised for observation. These commands do not install a service.
The generated settings are owned by `pr_service/settings.rs#Settings`.

```sh
set -eu
umask 077
release=/absolute/installed/release
checkout=/absolute/test-repository-checkout
service=/absolute/private/pr-observer-state
repository=example/test-repository
worker="$release/bin/pr-reconciler"
mkdir -m 700 "$service"
"$worker" --generate \
  --repository "$repository" --directory "$service" --checkout "$checkout" \
  --executable "$release/bin/provenant" --worker "$worker" \
  --path "$PATH" --policy-digest observation-policy-1 > "$service/settings.json"
"$worker" --config "$service/settings.json" --once
"$worker" --config "$service/settings.json" --status > "$service/baseline.json"
"$worker" --config "$service/settings.json" --launchd > "$service/worker.plist"
python3 - "$service/worker.plist" <<'PYPLIST'
import plistlib, sys
with open(sys.argv[1], 'rb') as f:
    p = plistlib.load(f)
assert p['KeepAlive'] is True and p['RunAtLoad'] is True
assert p['ThrottleInterval'] == 30
print(p['Label'], p['ProgramArguments'])
PYPLIST
```

An explicit absolute PATH must include `gh`; the worker uses existing GitHub
CLI authentication. The checkout establishes the command working directory.
The configured `executable` records the intended command binary for a future
adapter; observe mode never invokes it. Settings and plists contain no webhook
secret. The generated supervisor supports polling only. For a foreground signed
receiver, set `bind` in the settings to a loopback socket, supply
`PROVENANT_INTAKE_SECRET` through the process environment and configure the tunnel
and webhook described in [event intake](event-intake-configuration.md).
Do not point standalone `event-intake` at this worker's state directory.

`report.json` is the atomic checkpoint. It binds repository and policy, counts
worker boots and successful observations, and carries current per-head decisions,
intake metrics, error counters and `effect_calls: 0`. A changed head or base
produces a new work key. Daily files at `digests/<UTC epoch day>.json` project that
checkpoint; same-head restarts do not double-count decision transitions. Retain
reports and digests as chair evidence outside temporary build directories.

Decisions are proposals. Two distinct non-author approvals on the exact head are
only an observation signal; they do not certify ladder coverage or authorise
landing. A `land` proposal means that an action adapter would still need to
evaluate configured authority, terminal acceptance and all candidate gates.
The observer requires the newest `ci-status` check to have completed successfully
on the exact head (or the newest status context from that head's immutable REST
endpoint). Missing, stale, pending, skipped and neutral checks never suffice.
It does not transfer reviews between heads. Forks, malformed snapshots and
terminal-labelled PRs escalate locally; changes requested at the current head
propose repair and appear in the digest's escalation list. Errors leave the last
successful snapshot visible with `last_error` and error counters; do not treat
that snapshot as fresh.

Local escalation means the report and digest are available for the chair to
collect; the worker sends no email, comments or other notifications.
`hint_completion_seconds` measures admission to durable completion, excludes
tunnel delivery time, and is not the end-to-end p95 required by #2846. Use the
live event timestamp and outage procedure in the event intake runbook for that
acceptance evidence.

## Restart drill on an installed host

Use the fresh state and polling-only settings above. This foreground drill kills
only the new child it starts. It neither inspects nor changes existing services.
The subshell owns the child PID and cleans it up if a wait or assertion fails:

```sh
(
set -eu
drill_pid=
cleanup_drill() {
  if [ -n "$drill_pid" ]; then
    kill -KILL "$drill_pid" 2>/dev/null || true
    wait "$drill_pid" 2>/dev/null || true
    drill_pid=
  fi
}
trap cleanup_drill EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
"$worker" --config "$service/settings.json" &
drill_pid=$!
python3 - "$service/report.json" "$service/baseline.json" <<'PYWAIT'
import json, sys, time
baseline = json.load(open(sys.argv[2]))
for _ in range(120):
    try:
        report = json.load(open(sys.argv[1]))
        if report['boots'] > baseline['boots']:
            break
    except (FileNotFoundError, json.JSONDecodeError):
        pass
    time.sleep(1)
else:
    raise SystemExit('new worker did not finish its startup sweep')
PYWAIT
kill -KILL "$drill_pid"
wait "$drill_pid" || true
drill_pid=
trap - EXIT HUP INT TERM
"$worker" --config "$service/settings.json" --once
"$worker" --config "$service/settings.json" --status > "$service/after-restart.json"
python3 - "$service/baseline.json" "$service/after-restart.json" <<'PYASSERT'
import json, sys
a, b = (json.load(open(p)) for p in sys.argv[1:])
assert b['boots'] >= a['boots'] + 2
assert b['observations'] > a['observations']
assert b['effect_calls'] == 0 and b['last_error'] is None
assert b['intake']['oldest_pending_marker'] is None
assert b['intake']['last_successful_sweep'] >= a['intake']['last_successful_sweep']
print('Foreground restart passed; compare decisions with the live forge.')
PYASSERT
)
```

Repeat after moving a test PR head while the worker is stopped; compare its
reported key, head and decisions with the forge. For signed intake, repeat with
pending delivery markers and verify their completion after restart. Record
release identity (`"$worker" --version --json`), timestamps, before/after
reports and the forge state. A successful foreground drill proves process
recovery; it does not prove launchd KeepAlive or throttling.

Only the chair may load the generated supervisor during a separately authorised
installed-service drill. The following commands are provided for that drill and
were not run in a lane. Use the fresh test service only; its repository-derived
label must not already be loaded. After loading, record the launchd PID, kill that
PID through launchd, and verify the boot counter increases and launchd has a new
PID. Keep the status output and timestamps to assess restart throttling.

```sh
(
set -eu
label=$(python3 -c 'import plistlib,sys; print(plistlib.load(open(sys.argv[1],"rb"))["Label"])' "$service/worker.plist")
domain="gui/$(id -u)"
drill_loaded=false
cleanup_supervisor_drill() {
  if [ "$drill_loaded" = true ]; then
    launchctl bootout "$domain" "$service/worker.plist"
    drill_loaded=false
  fi
}
trap cleanup_supervisor_drill EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
# A loaded label belongs to an existing service: stop the drill if this succeeds.
if launchctl print "$domain/$label"; then
  echo 'Test service label already loaded; choose a different test repository.' >&2
  exit 1
fi
wait_for_boot() {
  python3 - "$service/report.json" "$1" <<'PYBOOT'
import json, sys, time
for _ in range(120):
    try:
        r = json.load(open(sys.argv[1]))
        if r['boots'] > int(sys.argv[2]) and r['last_error'] is None:
            break
    except (FileNotFoundError, json.JSONDecodeError):
        pass
    time.sleep(1)
else:
    raise SystemExit('supervisor did not complete a new startup sweep')
PYBOOT
}
boots=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["boots"])' "$service/report.json")
launchctl bootstrap "$domain" "$service/worker.plist"
drill_loaded=true
wait_for_boot "$boots"
launchctl print "$domain/$label" > "$service/launchd-before.txt"
boots=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["boots"])' "$service/report.json")
launchctl kill SIGKILL "$domain/$label"
wait_for_boot "$boots"
"$worker" --config "$service/settings.json" --status > "$service/launchd-after.json"
launchctl print "$domain/$label" > "$service/launchd-after.txt"
# Unload only the fresh test service created above.
launchctl bootout "$domain" "$service/worker.plist"
drill_loaded=false
trap - EXIT HUP INT TERM
)
```

A native supervisor drill, live event latency/outage captures and a period of
live observation remain required before automatic actions can be considered.
No observe report, local digest or fixture result grants action authority.
