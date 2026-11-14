# provenant-supervisor

Provider supervision and process custody. OS primitives live in provenant-os; custody-specific native operations stay here.

See the [owner specification](../../../../docs/specs/supervisor.md).

## Native boundaries

| Owner | Operations and proof |
| --- | --- |
| `provenant_os::sys::{child, raw_exec}` | Preallocated argv/environment, strict exec, signal reset, CLOEXEC sweep, owned utility child and bounded pipe polling. |
| `provenant_os::sys::reaper` | Fixed admission reserved before spawn, non-signalling transfer, bounded fair `try_wait` turns. |
| `provenant_os::sys::pid`, `process_os::census` | Linux procfs and Darwin libproc identity/CPU/CWD, native boot identity, complete/partial/unknown census. Observation never grants arbitrary PID signalling. |
| `process_os::provider` | macOS guardian-local signal reactor, WNOWAIT leader reservation, direct-child delivery and explicit cleanup before reap. Other platforms return an unsupported error. |
| `process_os::handoff` | Bounded private same-binary channel; only the guardian inherits writer/attempt locks. Provider environment values and prompt remain ephemeral. |
| `provenant_os::sys::fs` | Held-directory open/stat/mutation, atomic publication, no-follow locks, descriptor ownership, directory sync and bounded flock. |
