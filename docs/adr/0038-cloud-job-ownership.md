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

A terminal result is advisory and must come from a channel authorised for that
job. It binds the full tested commit, exit status, tests and log location;
changed tracked files or incomplete evidence cannot pass. Requested model and
effort are distinct from the observed model. Unknown or different models are
reported. Provider-specific task protocols and log access remain behind the
adapter, not in the shared lifecycle contract.

The existing lane Git, credential, object-integrity and authority boundaries
continue to apply. Host credentials do not travel in prompts, results or
provider environments; an environment's own configured access is separate.
Cloud evidence does not replace exact-head required CI or grant landing
rights.

## Consequences

A chair can recover an identity without a local process, provided the provider
returned one and the owner saved it. An ambiguous submission may need provider
reconciliation rather than a retry. Providers without confirmed cancellation
must report that limit. A transport and authenticated result channel still
need integration with run ownership and the lane views before cloud placement
is usable.
