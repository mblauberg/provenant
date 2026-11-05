# Event intake configuration

Set these environment variables for `event-intake` and its `status` command.
The worker uses the existing `gh` authentication for GitHub REST reads.

| Variable | Meaning |
|---|---|
| `PROVENANT_INTAKE_REPOSITORY` | Required repository in `owner/name` form. |
| `PROVENANT_INTAKE_DIR` | Required private directory for one repository's state and worker lock. |
| `PROVENANT_INTAKE_SWEEP_SECONDS` | Positive sweep interval in seconds; default `300`. |
| `PROVENANT_INTAKE_BIND` | Optional loopback address, such as `127.0.0.1:8765`. Omit for polling only. |
| `PROVENANT_INTAKE_SECRET` | Webhook HMAC secret; required when the receiver is enabled. |

The operator configures a stable HTTPS tunnel to the loopback `/webhook`
endpoint and configures the matching repository webhook in GitHub. The worker
does not create a tunnel or register a webhook. See the
[event intake runbook](github-workflow.md#event-intake).
