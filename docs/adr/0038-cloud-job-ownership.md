# ADR 0038: Cloud job ownership without a local process

**Status:** Superseded

Changed: 2026-11-05, #2889. Revised 2026-11-09: cloud jobs may write.
Superseded 2026-11-11, #3225: Provenant no longer owns cloud jobs. Cloud
placement was removed; `provenant cloud launch` submits a brief and
`provenant cloud collect` takes back the result branch, with no lifecycle,
polling or custody. See [Cloud launch](../runbooks/cloud-launch.md).

## Context

A provider VM can outlive the chair and has no local guardian process whose
exit proves the job's outcome. Treating a missing local process as completion
would lose running jobs and could start duplicates after an interrupted submit.
Provider transports also differ in their support for status, logs and cancel.

## Decision

The run owner retains a provider-independent remote job identity and observes
it through a provider adapter. Submission records a durable intent before
contacting the provider and records the returned remote identity before the
first wait. A lost response or a failure to persist that identity leaves
custody unknown; it does not authorise another submission. Cancellation is
confirmed only on provider evidence; otherwise it remains unconfirmed.
An observed environment setup failure is terminal and retains the target,
environment and a repair hint, including when it is discovered after dispatch.

A terminal result is advisory. The job reports its job ID, submit-time
random nonce, the full commit it started from, exit status, tests, log
location and observed model when available; a writer also reports the branch
it pushed and that branch's full head. The owner rejects a different job ID,
nonce or commit. The nonce binds the result to an intent; it is not a
credential or proof of origin. A result with failed tests or incomplete
evidence cannot pass. Changed tracked files are a writer's normal output and
do not fail a result. Requested model and effort are distinct from the
observed model. Unknown or different models are reported.

A cloud job may be a writer. Dispatch seals a named branch with the
placement. The job commits and pushes without force to that branch, may merge
the base branch and may run heavy proof there. Where the platform restricts
branch names, the job uses the branch it is allowed and the record names that
branch. The owner accepts a writer's result only after it verifies the push
on the remote: the branch head is the reported head and descends from the
starting commit. That branch and head become the lane's result, so the normal
pull request and land flow applies. The push, not the cloud result, starts
the required CI on the exact head. Provider-specific task protocols and log access remain behind the
adapter, not in the shared lifecycle contract. Observation and cancellation
receive the durable job context so an adapter can select its repository and
bind the result channel to the requested commit after a restart.

Claude has no status command. Its completion signal is a commit pushed to
the job's branch whose message ends with a `Provenant-Cloud-Result:` line
carrying the result. The owner inspects advertised `claude/*` and named
branch tips, fetches with object checks, takes the branch and head from the
remote ref, and accepts only a matching result whose head descends from the
starting commit. A finished job therefore settles at the next observation.
An unfinished job past its deadline is reported unknown with its session URL.
After saving a verification job's result, the owner attempts to delete its
result-only branch and warns on failure; a writer's branch is kept. Claude
receives the requested model and effort as CLI options.

Codex returns the result in its final agent message through the thread
transport, or as an uncommitted result file in its cloud diff through the
official transport. A thread turn that has posted a final answer carrying
this job's result settles even while the service still reports the turn in
progress. Both adapters call the installed CLI through a timed seam. Official
Codex uses its JSON list to recover a lost submit response. Neither adapter
can confirm cancellation. Codex cannot request a model or effort; the thread
transport reports the thread's model and the official transport reports
unknown.

Named cloud targets supply provider, environment, repository, base branch,
model, effort and deadline, with per-run overrides; a writer's named branch
comes from the dispatch. The sealed run request selects
cloud custody before local launch. Reconciliation persists observations and
projects them into the existing lane views without inventing a local process
exit or granting the remote result gate authority.

The existing lane Git, credential, object-integrity and authority boundaries
continue to apply. Host credentials do not travel in prompts, results or
provider environments; an environment's own configured access is separate.
Cloud evidence does not replace exact-head required CI or grant landing
rights.

## Consequences

A chair can recover an identity without a local process, provided the provider
returned one and the owner saved it. An ambiguous submission may need provider
reconciliation rather than a retry. Providers without confirmed cancellation
must report that limit. Provider CLI output formats remain to be captured on
real hosts; parsing failures include the output observed, and cloud results
remain advisory even when their binding checks pass. A cloud writer needs an
environment whose GitHub access can push the named branch; the owner never
pushes, opens a pull request or integrates on its behalf.
