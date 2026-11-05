# Pull request work records

Owner: workflow crate. The reconciler store is a building block; event intake,
dispatch, forge adapters and landing policy are separate. It does not start a
background service or grant publication or landing authority.

A key contains repository, PR, exact head, policy digest, lens and base digest.
The caller opens a SQLite connection at its configured state location, with its
chosen busy timeout. Writers use immediate transactions. Observe an authoritative
current head before dispatch, including after restart; never treat the head in an
unordered event as current without checking the forge. Observing a different head
cancels older records atomically. A cancelled record cannot issue or retry an
intent, and its verdict never transfers to another key. Reclaim an expired lease
on the existing attempt rather than creating a second attempt.

| State | Entry and next step |
| --- | --- |
| needs-review | A new key; start its one attempt. |
| reviewing | Attempt holds a lease; record its verdict or fail. |
| verdict | Persist a verdict publication intent, reconcile its receipt, then await checks; or fail. |
| awaiting-checks | Adapter evaluates checks on the exact head; mark landable or fail. |
| landable | Adapter applies landing policy and records a merge intent; or fail. |
| landing | Reconcile the merge intent. |
| landed | A remote merge receipt was observed. |
| failed | Terminal for this key. |

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
