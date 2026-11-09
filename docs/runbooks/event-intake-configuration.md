# Event intake
Owner: Workflow maintainers
Anchor: [`event-intake::run`](../../tools/front-door/src/bin/event-intake.rs); [`event-intake-live-check`](../../scripts/event-intake-live-check)

`event-intake` keeps a private, current copy of one repository's pull request,
check, comment and review state. Existing `gh` commands do not depend on it.
The release installer does not ship it yet: build it in `tools/front-door` with
`cargo build --release --locked --bin event-intake` and run the binary from the
Cargo target directory.

## Configure

Set these environment variables for the worker and its `status` command. The
worker uses the existing `gh` login for GitHub REST reads.

| Variable | Meaning |
|---|---|
| `PROVENANT_INTAKE_REPOSITORY` | Required repository in `owner/name` form. |
| `PROVENANT_INTAKE_DIR` | Required private directory for one repository's state and worker lock. |
| `PROVENANT_INTAKE_SWEEP_SECONDS` | Sweep interval in seconds; default `300`. |
| `PROVENANT_INTAKE_BIND` | Optional loopback address, such as `127.0.0.1:8765`. Omit for polling only. |
| `PROVENANT_INTAKE_SECRET` | Webhook HMAC secret; required when the receiver is enabled. |

`scripts/check-doc-truth` compares this table with the variables the worker
reads.

Run one worker per repository and state directory; a second worker on the same
directory is refused by its lock. Keep the directory private: it holds response
bodies, delivery IDs and timing data. Do not point it at a
[PR reconciler](pr-reconciler.md) state directory.

To receive webhooks, bind to loopback and put an operator-managed, stable HTTPS
tunnel in front of `/webhook`. Configure a repository webhook with the same
secret for pull request, push, check run, check suite, status, issue comment,
pull request review and pull request review comment events. The worker neither
creates the tunnel nor registers the webhook; keep the endpoint private to the
tunnel where possible.

## Operate

The worker sweeps at startup and every interval, and reconciles signed hints
between sweeps, coalescing duplicate targets and turning unmappable checks into
repository sweeps. Requests reuse persisted ETags. Run `event-intake status`
with the same environment to see counters, the 304/200 ratio, rate-limit
headers, the sweep watermark and the oldest pending marker.

## Acceptance checks

Before relying on the service, record two live checks against an isolated test
repository with no other webhook traffic, a running receiver and tunnel, and a
completed initial sweep. Fake-forge tests do not count.

- **Latency:** at least 20 each of push, check, comment, review and merge
  events; the p95 delay until the change appears in `state.json` must be below
  30 seconds for every type.
- **Outage drill:** stop the receiver or tunnel, make changes including a
  merge, restart, and confirm against GitHub that the first successful sweep
  captured every change.

`scripts/event-intake-live-check` runs both from a plan prepared outside the
checkout. Set `PROVENANT_INTAKE_DIR` to the worker's state directory and
authenticate `gh` for the test repository:

```sh
scripts/event-intake-live-check /private/path/plan.json > /private/path/evidence.json
```

```json
{
  "status": ["/path/to/event-intake", "status"],
  "timeout_seconds": 900,
  "samples": [
    {
      "kind": "comment",
      "command": ["gh", "api", "repos/OWNER/REPO/issues/123/comments", "-f", "body=unique-sample-1"],
      "checks": [{"pr": 123, "resource": "issue_comments", "expect": [{"body": "unique-sample-1"}]}]
    }
  ],
  "drill": {
    "stop": ["/path/to/stop-worker-and-wait"],
    "change": ["/path/to/make-outage-changes"],
    "start": ["/path/to/start-worker-and-return"],
    "checks": [{"pr": 456, "resource": "pull_request", "expect": {"merged": true}, "github_path": "repos/OWNER/REPO/pulls/456"}]
  }
}
```

Each sample's `command` is an argument array that runs synchronously and causes
exactly one real event; prepare 20 mergeable PRs for the merge samples and a
check-producing app or workflow for the check samples. Each check asserts a
unique, previously absent change in one `state.json` resource:
`pull_request`, `check_runs`, `issue_comments`, `reviews` or
`review_comments`. Objects match partially; each expected array element must
match an actual element.

For the drill, `stop` stops the worker and waits for it to exit, `change` makes
the missed changes, and `start` restarts the same worker and returns. Each drill
check names its direct, unpaginated REST `github_path`, so keep the test
repository small enough for one page. Keep the sweep interval long enough that a
second sweep cannot finish before the first is observed.

The output records delivery IDs, checks, times, nearest-rank p95 per type and
status before and after the drill. Timing starts before each command, so it is
a conservative upper bound. The command exits nonzero on a missing
observation, a failed drill or any p95 of 30 seconds or more. Retain the plan
and output as evidence; after a failure, rerun with fresh PRs and unique
values.
