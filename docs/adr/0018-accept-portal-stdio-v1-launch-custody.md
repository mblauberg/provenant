# ADR 0018 — Accept `portal-stdio-v1` as review-portal launch custody

**Status:** Accepted 2026-07-28 (user, issue #489); applies [ADR
0001](0001-personal-first-product-compatible.md) and [ADR
0017](0017-specifications-own-non-derivable-intent.md)

## Context

The review-portal helper ships as one dependency-free Rust crate,
`runtime/agent-fabric-review-portal-supervisor`. It declares a single mode,
`PORTAL_MODE = "portal-stdio-v1"` (`src/lib.rs:15`), and
`parse_portal_invocation` (`src/lib.rs:315`) refuses any other argv.

The specifications described something larger. `authority.md` and `effects.md`
narrated a `supervise-v1` mode in which the daemon retained one endpoint of a
one-use registration socketpair, forked a pinned launch stub that received the
other endpoint as FD 4, exchanged a 136-byte `AFCHAL1\0` challenge, a 216-byte
`AFREGV1\0` registration and a 208-byte `AFACKV1\0` ACK across a 32-byte
one-use CSPRNG nonce, committed a process-custody row, and only then allowed
the stub to close FD 4 and `execve` the provider in place. Cross-language
byte-and-digest golden vectors were to pin the three frames in both Rust and
TypeScript.

None of that exists. `AFCHAL1`, `AFREGV1`, `AFACKV1`, `supervise-v1` and
`SCM_RIGHTS` appear in no `.rs` and no `.ts` file in the repository. The crate
never forks and never execs: it has no `fork`, `execve` or `Command::new` call
site. There is no daemon-side caller at all. Outside its own crate the
supervisor is named only by documentation (`runtime/README.md`,
`runtime/agent-fabric/README.md`, `MAINTAINING.md` and several
specifications), by the repository-structure tests and by CI; none of them
invokes it.

`review_portal_process_custody` was the schema the handshake was to write,
down to `control_fd_number INTEGER NOT NULL CHECK (control_fd_number = 3)`.
It had no production writer, and its only other mention in the tree was an
inventory list in
`runtime/agent-fabric/tests/integration/migration-runner.integration.test.ts`.
It has since been dropped from the regenerated baseline (see Consequences).

So the question issue #489 raised is not "which of two implementations wins".
It is whether the cheap layer that exists is the layer this harness should
have.

## Decision

`portal-stdio-v1` is the accepted review-portal launch boundary. The production
binary proves exact argv and environment, closed inherited descriptors, one
AF_UNIX connection and bounded opaque LF relay. Peer identity, ancestry,
termination and path custody are test-evidenced helper APIs until a production
caller wires them into launch custody. The specified three-frame pre-exec
registration handshake is not built and will not be built.

The threat it was designed to close is real and is not being closed. Between
the moment the daemon decides to launch a provider and the moment `execve`
runs, the executable the daemon measured can be replaced by a different one.
Nothing in the shipped supervisor re-measures the binary immediately before
exec, because the shipped supervisor is not the process that execs. That is a
genuine time-of-check-to-time-of-use window, and this decision leaves it open.

The residual risk is accepted because of the threat model in [ADR
0001](0001-personal-first-product-compatible.md). This harness optimises for
single-operator macOS use. An attacker positioned to win that race — able to
write the provider's install path between the daemon's measurement and the
child's `execve` — already holds the user's own privileges on the user's own
machine, and has cheaper paths to every asset the handshake protects. Against
that attacker the mitigation buys close to nothing, and it is not cheap: it
costs byte-exact frame layouts pinned across two languages, three inherited
descriptors with separate lifetimes, a one-use CSPRNG nonce, a re-hash of the
full provider closure immediately before exec, and a cross-language golden-
vector suite that both implementations must keep in step forever.

Two changes would reverse this. A multi-user host, where another principal can
reach the provider path without already holding the user's privileges, makes
the race worth winning. So does a provider binary sourced outside the user's
own control — installed, updated or patched by a party the user does not
authorise — because then the substitution need not be a race at all. Either
condition reopens the question; neither is a "review the decision periodically"
task, and no periodic review is scheduled.

## Consequences

Production custody today is limited to what the binary calls:

- the argv contract is closed. `parse_portal_invocation`
  (`src/lib.rs:315`) accepts exactly `["portal-stdio-v1"]` and rejects
  everything else; `tests/invocation_contract.rs` pins both arms.
- the environment is exactly three non-secret locators —
  `AGENT_FABRIC_REVIEW_SOCKET`, `AGENT_FABRIC_REVIEW_ACTION` and
  `AGENT_FABRIC_REVIEW_CONTRACT` (`src/lib.rs:16`–`18`) — and any fourth
  variable is refused, so no ambient parent environment reaches the helper.
- the helper proves it inherited nothing. `require_portal_descriptors_closed`
  (`src/lib.rs:346`) enumerates `/dev/fd` and fails when any descriptor above
  standard error is open, evidenced by
  `tests/portal_relay.rs:574`.
- `mark_control_fd_cloexec` (`src/lib.rs:403`) sets and verifies `FD_CLOEXEC`
  on `CONTROL_FD = 3` (`src/lib.rs:20`), evidenced by `tests/control_fd.rs:30`.
  It is a library routine with no production caller: `portal-stdio-v1` refuses
  every descriptor above standard error, so no control descriptor reaches the
  running helper, and the crate never forks or execs a child that could
  inherit one. It is recorded here as available, not as shipped launch
  contract.
- peer identity, ancestry, path custody and termination remain library helpers
  with tests, not production launch guarantees. The crate README keeps their
  certifying routes inactive until daemon integration proves the required
  wiring.
- the relay stays opaque. `read_lf_frame` (`src/lib.rs:1509`) bounds every
  frame by `MAX_LF_FRAME_BYTES` (`src/lib.rs:19`) and parses no JSON, MCP or
  UTF-8; TypeScript remains the sole semantic parser.

What is lost is the guarantee that no provider instruction executes before
durable custody exists. That guarantee was never delivered, so nothing regresses
today; what changes is that the specifications stop implying otherwise.

`review_portal_process_custody` was therefore write-dead by decision rather
than by oversight. It was dropped when the baseline was regenerated for [issue
#381](https://github.com/mblauberg/provenant/issues/381) (commit `93c3496c`),
together with the other write-dead tables; nothing in the tree names it now.

The affected specification paragraphs are corrected rather than deleted
wholesale, under [ADR
0017](0017-specifications-own-non-derivable-intent.md). What ships is restated
in the present indicative against the crate; the handshake mechanism is removed
because this ADR settles that it will not be built, and a present-indicative
sentence describing an unbuilt mechanism is the defect ADR 0017 names. The
negative requirements that survive — no descriptor passed by `SCM_RIGHTS`, no
inherited provider descriptor, no ambient environment — survive because they are
must-deny intent that holds today, not because the machinery they mention
exists.

## Rejected

- Building a proportionate subset of the handshake — for example the
  registration frame and ACK without the challenge or the closure digest.
  Rejected because a subset that does not re-measure the executable immediately
  before exec closes none of the TOCTOU window, and one that does re-measure
  carries almost the full cost of the specified design.
- Leaving the specifications as written and tracking the gap only in an issue.
  Rejected because present-indicative prose asserting an absent security
  mechanism is worse than no prose: it is the failure mode ADR 0017 exists to
  prevent, and a reader auditing the launch path would have concluded custody
  was proved when it is not.
