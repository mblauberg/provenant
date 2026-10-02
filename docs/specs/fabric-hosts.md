# Spec: Fabric across the user's own hosts

Status: **proposed**, awaiting user approval. Decision record: [ADR
0026](../adr/0026-federate-fabric-across-own-hosts-over-ssh.md). GitHub owns
delivery state; this file owns the durable requirements. It grants no
authority.

## Outcome

One user with two or more of their own Macs works from either machine as if
Fabric were one system. A chair on one host dispatches lanes to another host,
and sees every host's lanes, messages, tasks and activity in one view. Each host
keeps running its own lanes when the others are asleep, offline or on another
network. No daemon, shared database or replicated store is introduced.

The first deployment is a laptop (`laptop`) plus an always-on spare Mac
(`workshop`) reached over SSH, typically through Tailscale.

## Operating scenarios

| Scenario | Required behaviour |
|---|---|
| Peer offline | Everything local works unchanged. Default-placed lanes fall back to local and say so. Peer rows show `unreachable` with their last known state and age, never `dead` or `orphaned`. |
| Same network or direct cable | Same commands. No configuration change between LAN, Thunderbolt Bridge and tunnel paths; the SSH host alias resolves the route. |
| Peer on, chair host off | Peer lanes run to completion, commit to their branches and record results. Messages addressed to the absent chair wait on the peer and arrive when the chair next reads. |
| Hosts on different networks | Same commands over the tunnel, within the timeouts below. |
| Chair runs on the always-on host (away mode) | The configuration is symmetric: a chair on `workshop` sees `laptop` lanes whenever `laptop` is reachable, and can land work because `workshop` is the home host. |

## Model

- **Host.** A named machine in the user's host configuration, with a stable
  short name and an SSH destination. Host configuration is per-user instance
  state, not repository content.
- **Ownership.** Each host is authoritative for the records it created: its
  lanes and run directories, its Fabric rows, its named provider sessions and
  its processes. No host writes another host's store except through that host's
  peer entrypoint.
- **Home host.** Each project names one home host. It holds the project's
  landing lease and work claims, and is the default home for new shared tasks.
  The recommended home host is the always-on machine.
- **Qualified identifiers.** Runs, tasks, sessions, agents, messages and event
  IDs carry their owning host (`id@host`). An unqualified identifier means the
  local host. A selector that matches records on more than one host is refused
  as ambiguous, never resolved to the first match.
- **Federated read.** Views query every reachable host and merge the results,
  with a `host` field on every row.
- **Routed write.** A write to a record on another host runs on the owning host.
- **Project identity.** A project is identified by its home-relative primary
  checkout path. Version 1 requires that path to be identical on every host.

## Requirements

### Hosts and transport

- With no peers configured, every command behaves exactly as today.
- Peer traffic uses the user's OpenSSH client and existing SSH configuration.
  Provenant adds no listener, port, service or credential of its own.
- The remote side is a single versioned peer entrypoint with a fixed verb set
  and JSON on standard input and output. It is never an arbitrary remote shell
  command assembled from caller input. The entrypoint also carries Git object
  transfer for configured project repositories, so that one SSH key restricted
  to the entrypoint with `command=` supports every peer operation. The
  documented setup includes that restriction and is tested with it applied.
- Each peer call has a connect timeout (default 5 seconds) and a response
  deadline (default 15 seconds for reads and routed writes, longer only for an
  explicit fetch). Both are configurable. A timeout marks that host
  `unreachable` for the call; it never fails the whole command.
- Peers exchange a protocol version on first contact. A mismatch refuses
  dispatch and routed writes and flags reads.
- `provenant hosts doctor` reports, per host and run non-interactively through
  the peer entrypoint:
  - reachability, protocol version and Provenant revision;
  - project path presence;
  - each configured adapter's executable resolution, its sign-in usability and
    whether its confinement is effective rather than degraded;
  - whether lane temporary paths fit the platform socket-path limit.

### Last known state

- Each successful federated read stores a per-host snapshot of the rows it
  returned, in the reading host's store, with the time observed.
- When a host is unreachable, its rows come from that snapshot, marked
  `unreachable` with their age. A snapshot row is replaced on the next
  successful contact and is never treated as evidence of liveness or death.

### Placement and dispatch

- Each project may name a default host for dispatch, optionally per mode
  (`read_only`, `worktree_write`). An explicit host selector always wins.
- Every remote dispatch carries a caller-generated operation ID. The owning
  host treats a repeated operation ID as the same launch and returns the
  existing run instead of starting another.
- When the default host cannot be contacted before the request is sent, the
  lane runs locally. The response, attempt record and digest state the fallback
  and its reason. An explicitly selected host never falls back.
- When the request was sent but no response arrived, the outcome is
  `launch_unknown`. It never falls back. The caller reconciles by operation ID
  on the next contact, and the lane view shows the pending reconciliation.
- A lane never moves hosts after it starts.
- A remote dispatch accepts the same request fields as a local one. Prompts and
  prompt files travel in the request body. Absolute paths under the user's home
  are rewritten home-relative; any other absolute path is refused for remote
  placement.
- The remote lane is launched by the remote host's own dispatch owner, detached
  as today, so it survives the peer connection closing.
- Executables resolve on the executing host without relying on an interactive
  or login shell. A remote adapter whose confinement would be degraded, or whose
  temporary paths would exceed the socket-path limit, is refused with the
  reason rather than run unconfined.
- An adapter is refused for remote placement only when `hosts doctor` finds its
  sign-in unusable there. Other adapters are unaffected.

### Code transport for writer lanes

- A remote writer lane starts from a committed revision. The chair host sends
  that revision to the remote checkout through the peer entrypoint, under a
  Provenant-owned ref namespace. Uncommitted chair changes are not transported,
  and the response says so.
- The remote host creates the linked worktree with its own worktree helper and
  writer lease. The writer lease stays host-local.
- The executing host verifies the worktree context and cleanliness, and binds
  the result to the base revision, head revision, host and worktree identity.
  The chair then fetches the branch and verifies that the fetched head matches
  that bound result. Transport does not depend on GitHub being reachable.

### Lanes, status and control

- `lanes`, `status`, `watch` and `lanes --wait` include every reachable host and
  add `host` to each row, under a new version of the existing JSON schemas.
- Liveness is decided only by the host that owns the process. A reader never
  probes or infers a remote process from local PID state.
- `lanes --wait` treats an unreachable host's non-terminal lanes as still
  pending. It keeps its existing timeout exit code and names the hosts that were
  unreachable when it timed out.
- `cancel`, `resume`, `handoff` and named-session turns run on the host that
  owns the run or session, and carry an operation ID so that a retry after a
  lost response is idempotent.
- `output` reads bounded chunks from the owning host. Whole run directories are
  copied only on an explicit fetch.

### Messages, tasks and activity

- Agent identity is `agent_id@host`. An unqualified recipient means the local
  host.
- A message's ID is fixed when it is created and never regenerated. Sending to a
  reachable host imports the message into the recipient host's store. Sending to
  an unreachable host records it as pending outbound in the sender's store. A
  recipient's inbox read imports pending outbound messages addressed to it from
  every reachable host.
- Import is idempotent by message ID: one message produces exactly one delivery
  row on the recipient host. The sender removes a pending outbound record only
  after the recipient confirms the import. A lost confirmation causes a
  harmless repeated import.
- Claims, claim expiry with redelivery, and acknowledgements run against the
  store that holds the delivery, unchanged from single-host behaviour.
- Replies and task links may reference a message or task on another host. Such
  references are stored as qualified external references, not as local foreign
  keys.
- A task belongs to the host that created it, by default the project's home
  host. Task lists merge every reachable host. Claim and done run on the owning
  host and are refused, not queued, when that host is unreachable.
- Activity keeps per-host sequence order. The merged view interleaves hosts by
  timestamp for display only; paging uses a per-host cursor map, so clock skew
  can reorder the display but never skips or repeats an entry. A single integer
  cursor remains valid for local-only use.

### Landing and work claims

- The landing lease and work claims live only in the home host's store.
- The complete fenced landing operation runs on the home host: lease
  verification, candidate fetch from the chair host, the remote SHA check and
  the push. A lost response is reconciled on the home host by lease generation
  before any retry.
- Landing and work-claim acquisition are refused while the home host is
  unreachable.
- Changing a project's home host is an explicit operation, refused while a
  landing lease or work claim is held.
- Lease expiry is evaluated only on the home host's clock. No correctness
  requirement compares timestamps from two hosts.

### Credentials and confinement

- Provenant never copies, forwards or reads provider credentials, SSH keys or
  keychains across hosts. Each host's adapters use that host's own sign-ins.
- Sandbox, protected-path and capability policy apply on the executing host
  exactly as for a local lane.

## Delivery order

All of the following are in version 1. They land in this order, and each slice
is usable on its own:

1. Host configuration, peer entrypoint, `hosts doctor`, qualified identifiers.
2. Remote dispatch, lanes, status, wait, cancel and output, with last known state.
3. Writer lanes and code transport.
4. Messages, tasks and activity federation.
5. Home host, fenced landing and work claims.

## Open questions

- **Non-interactive Claude sign-in.** Claude Code stores its sign-in in the
  login keychain, which is commonly locked for SSH sessions. A timeboxed
  feasibility spike on the target host chooses between a long-lived setup
  token, unlocking the keychain at session start, and launching through a
  console-started `tmux` session. Until it lands, only Claude placement on a
  remote host is blocked.

## Exclusions

- Automatic placement by host load, and moving a running lane between hosts.
- A shared, network-mounted or replicated SQLite store.
- Any daemon, broker, relay service or listening port.
- Linux or Windows hosts.
- Hosts belonging to another person, or multi-user trust between hosts.
- Mapping different checkout paths between hosts (version 1 requires identical
  home-relative paths).

## Acceptance

Each criterion runs against two real hosts. The peer-failure criteria also run
with the peer made unreachable mid-test, with a stale SSH control connection,
and across a sleep and wake of each host.

1. With no peers configured, the existing Fabric and dispatch test suites pass
   unchanged.
2. `dispatch --host workshop` starts a lane whose process runs on `workshop`;
   `lanes` on `laptop` shows it with `host: workshop` and the correct state.
3. Closing the SSH connection, or putting `laptop` to sleep, does not stop a
   `workshop` lane. It finishes and its result is readable from `laptop`
   afterwards.
4. With `workshop` unreachable, a default-placed dispatch runs locally and the
   digest names the fallback; an explicit `--host workshop` dispatch fails with
   a typed `unreachable` error; `lanes` shows `workshop` rows as `unreachable`
   with their age.
5. Dropping the connection after a remote launch is sent yields
   `launch_unknown`, no local fallback, and exactly one lane after
   reconciliation. Retrying the same operation ID starts nothing new.
6. A remote writer lane starts from a committed `laptop` revision without that
   revision being pushed to GitHub. Its branch is fetchable on `laptop`, and the
   fetched head matches the head bound by `workshop`'s verification.
7. `cancel` on `laptop` stops a `workshop` lane and its provider descendants,
   verified on `workshop`.
8. A message from a `workshop` lane to `chair@laptop`, sent while `laptop` is
   asleep, produces exactly one delivery row on `laptop` after the chair's next
   inbox read. This holds when the import confirmation is lost and the read is
   repeated.
9. Two hosts racing to claim one task produce exactly one owner.
10. Records with colliding short IDs on two hosts are addressable by qualified
    ID, and an unqualified selector matching both is refused.
11. Activity paging with deliberately skewed host clocks neither skips nor
    repeats an entry.
12. A landing whose push response is lost reconciles on the home host without
    a second push.
13. A peer with a different protocol version is refused for dispatch and
    routed writes and flagged on reads.
14. `hosts doctor` reports an adapter whose sign-in is unusable, or whose
    confinement would be degraded, before any dispatch to it is attempted.
15. The documented SSH `command=` restriction is applied in the test setup, and
    every operation above still works through it.
16. No Provenant process listens on a network port on either host.
