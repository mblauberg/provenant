# ADR 0026 — Federate Fabric across the user's own hosts over SSH

**Status:** Accepted 2026-10-07 (issue [#943](https://github.com/mblauberg/provenant/issues/943))

**Amends:** [ADR 0020](0020-retire-the-daemon-fabric.md) and [ADR
0022](0022-thin-fabric-mcp-execution-facade.md). Requirements:
[Fabric across the user's own hosts](../specs/fabric-hosts.md).

## Context

Provenant work regularly exhausts one 16 GB machine. Parallel writer lanes each
bring their own type checker, dev server, database cluster and browsers, and
heavy project suites queue behind a single per-machine lock. The user has a
second, mostly idle Mac of the same class and wants it to act as more capacity
behind one Fabric view, not as a second place to log into.

ADR 0020 made Fabric a single SQLite file with no daemon, on the premise of one
user on one machine. Run state outside SQLite is files plus local process
identity (PID and start time), and liveness is checked by probing local
processes. None of this can be read from another machine. The research note on
provider and runtime boundaries left open whether multi-machine work would
justify a gateway, service database or workflow engine.

The hosts are not always mutually reachable. The laptop sleeps, travels and
changes networks. The spare Mac is meant to stay on and may be the only host
awake while long lanes finish.

## Decision

Federate, don't centralise. Each host stays authoritative for what it created:
its lanes, run directories, Fabric rows, named sessions and processes. Other
hosts reach those records only through that host's own peer entrypoint, run
over the user's existing OpenSSH configuration.

- Views (lanes, status, inbox, tasks, activity) query every reachable host
  within fixed timeouts and merge the results with a `host` field. An
  unreachable host contributes its last observed rows, marked with their age.
- Identifiers are host-qualified (`id@host`). An ambiguous selector is refused.
- Writes go to the owning host (cancel, resume, claim, done, acknowledge) and
  carry operation IDs, so a retry after a lost response is idempotent.
  Liveness is judged only by the host that owns the process.
- A dispatch whose response is lost is `launch_unknown` and is reconciled by
  operation ID. It never falls back to local, so it cannot create a second
  writer.
- Messages keep the ID fixed at creation. They are imported into the
  recipient's host when reachable, or else held as pending outbound on the
  sender's host and imported on the recipient's next read. The sender clears
  pending outbound only after a confirmed import.
- Each project names one home host, normally the always-on machine. The
  landing lease and work claims live there, and the whole fenced landing
  operation runs there, so mutual exclusion never depends on two stores or two
  clocks agreeing.
- Placement is a per-project default host, with fallback to local only when
  that host cannot be contacted before the request is sent. An explicit host
  never falls back.
- Code moves directly between the hosts as Git objects over SSH, not through
  GitHub. The executing host verifies a writer lane's result and binds it to
  its base and head revisions.
- The peer entrypoint is versioned, takes JSON on standard input, accepts a
  fixed verb set including Git transfer, and never executes a caller-assembled
  shell command. One SSH key restricted to it serves every operation.
- Coordination is provider-neutral. A Claude Code, Codex or agy chair drives
  another host through the same `provenant` CLI and Fabric MCP surfaces, which
  must expose the same federated operations to every registered harness. No
  federated operation depends on a provider's own remote-control, cloud-session
  or SSH feature; such features remain optional conveniences.

The trust model is unchanged from ADR 0020 in kind: one user, on machines they
control, authenticated by their own SSH keys. Provenant adds no listener, port,
service, credential store or credential forwarding. Each host signs in to its
own providers, and executing-host sandbox and protected-path policy apply
unchanged.

## Alternatives rejected

- **One SQLite file on a shared or network filesystem.** SQLite locking is
  unreliable on network filesystems, and the store would disappear whenever its
  host slept.
- **Replicating the store** (log shipping or a CRDT layer). This adds conflict
  resolution for state that has a natural single owner, which is a large
  surface for a two-host setup.
- **A coordinating daemon or service database.** This reverses ADR 0020 and
  needs an always-reachable service, which these hosts cannot promise.
- **An asymmetric remote runner** (dispatch one way over SSH and fetch branches,
  with no federated views, mailbox or tasks). This is simpler, but it loses
  cross-host visibility and does not support a chair on the spare host.
- **Running everything on the spare host and using the laptop as a thin
  client.** This remains available as an operating mode (the symmetric
  configuration supports a chair on either host), but alone it gives up the
  laptop's own capacity and offline use.

## Consequences

- Every merged view costs one SSH round trip per reachable peer. Connection
  multiplexing and short timeouts keep that bounded.
- Run and lane JSON schemas gain a version with a `host` field. Single-host
  output is otherwise unchanged.
- Agent identity gains an optional host qualifier. Unqualified names keep
  meaning the local host.
- A project must use the same home-relative checkout path on every host until a
  path-mapping requirement is accepted.
- Landing and shared-task claims are unavailable while their owning host is
  unreachable. That is the intended failure mode, not a degraded success, and
  it is why the home host should be the always-on machine.
- Adapters whose sign-in fails in a non-interactive SSH session cannot run
  remotely until that host's sign-in is fixed. `hosts doctor` reports this.
