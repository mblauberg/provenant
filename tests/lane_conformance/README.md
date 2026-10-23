# Lane conformance

Run `TMPDIR=/tmp python -m pytest tests/lane_conformance -q` serially.
`PROVENANT_LANE_IMPL` selects `python` (the default) or `rust`; other values
are errors. Rust runs use `PROVENANT_FRONT_DOOR_BIN` or
`$PROVENANT_BIN_DIR/provenant`, defaulting to
`tools/front-door/target/debug/provenant`, and skip when it is absent.
The real Python owner remains the entry point for both supervisors at S2.
Every launched Rust attempt must retain a supervisor exit record; silently
falling back to Python fails conformance.

The real `dispatch_run.py` entry point plans through `cf_dispatch.py` and
executes FIFO-controlled provider CLIs. There are no supervisor imports,
patched lifecycle functions or replacement `CF_DISPATCH` seam. Assertions
read retained attempt/run files and the process table. Each run keeps fixture
commands, provider identity and owner diagnostics under `fixture/`.

Providers block on a FIFO held by the test. Readiness comes from retained
provider and attempt identities. Polling has a bounded failure deadline;
elapsed time never releases a provider. The startup row stays silent until
the watchdog stops it. The nested row emits a terminal result and stays
blocked, preserving ancestry until cleanup spares the live child owner.

The S2 rows cover silence with unchanged and progressing writer trees, wall
deadlines for all seven adapters, checkpoint failure, cancellation before
launch and after natural exit, SIGTERM escalation, a new-session grandchild,
terminal output with Cursor's stdin open, capture beyond the byte cap, and
an immediately released provider. S0 retains cancellation during a run.
The prelaunch row blocks OpenCode's version preflight on a separate FIFO.
The late-cancel row stops the supervising process, releases the provider,
observes EOF on a descriptor closed only by process exit, writes the cancel
marker, then resumes the supervisor. Prequeuing a command exercises fast exit;
black-box code cannot guarantee which host wins the watch-registration race.

Copilot's compatibility policy is dormant in the shipped product. Its fake
uses a private product-root overlay that enables only that adapter and adds
one fixture model pattern; all executable code links to the real checkout.

Observer death and ENOSPC require Rust and are marked
`xfail(strict=False, reason="rust-only until S2")` without execution under
Python. Build Rust with `--features fault-injection` for those rows. The
test-only `PROVENANT_LANE_FAULT` sites are `capture`, `progress`, `exit-once`
(reserve-space retry) and `exit` (persistent publication failure). A Rust
binary without injection must fail these assertions rather than silently
skip them. Observer identity comes from the retained launch argv and process
ancestry; its death must leave capture and the provider running.

Three Python defects retain strict xfails: prelaunch cancellation omits its
evidence signature, a late marker supersedes natural exit while capture EOF
is pending, and terminal grace kills Cursor before closing its stdin pipe.
Their assertions apply unchanged to Rust. Product repairs belong to the
implementation lanes.
