# ADR 0038: Cloud job ownership without a local process

**Status:** Accepted

Changed: 2026-11-05, #2889.

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

A terminal result is advisory. The job writes `.provenant-cloud-result.json`
with its job ID, submit-time random nonce, full tested commit, exit status,
tests, log location, tracked-file changes and observed model when available.
The owner rejects a different job ID, nonce or commit. The nonce binds the
result to an intent; it is not a credential or proof of origin. A result with
changed tracked files or incomplete evidence cannot pass. Requested model and
effort are distinct from the observed model. Unknown or different models are
reported. Provider-specific task protocols and log access remain behind the
adapter, not in the shared lifecycle contract. Observation and cancellation
receive the durable job context so an adapter can select its repository and
bind the result channel to the requested commit after a restart.

Claude publishes the result as the sole change in a commit whose parent is
the requested commit, on a `claude/*` branch. The owner inspects advertised
branch tips, fetches with object checks, and accepts only a matching result;
after saving it, the owner attempts to delete the branch and warns on failure.
Codex returns the result file through its cloud diff. Both adapters call the
installed CLI through a timed seam. Claude has no status command, so it remains
running until a result arrives or the deadline yields an unknown state. Codex
uses its status command and its JSON list to recover a lost submit response.
Neither adapter can confirm cancellation. Codex cannot request a model or
effort, so its observed model is reported as unknown.

Named cloud targets supply provider, environment, repository, branch, model,
effort and deadline, with per-run overrides. The sealed run request selects
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
remain advisory even when their binding checks pass.
