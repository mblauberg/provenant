# Lane conformance

Run `python -m pytest tests/lane_conformance -q` serially. The same command
with `PROVENANT_LANE_IMPL=rust` is the contract for S2; S0 ignores that value
and exercises today's Python supervisor.

The real `dispatch_run.py` entry point plans through `cf_dispatch.py` and
executes a FIFO-controlled Claude CLI. There are no supervisor imports,
patched lifecycle functions or replacement `CF_DISPATCH` seam. Assertions
read retained attempt/run files and the process table. Each run keeps fixture
commands, provider identity and owner diagnostics under `fixture/`.

Providers block on a FIFO held by the test. Readiness comes from retained
provider and attempt identities. Polling has a bounded failure deadline;
elapsed time never releases a provider. The startup row stays silent until
the watchdog stops it. The nested row emits a terminal result and stays
blocked, preserving ancestry until cleanup spares the live child owner.
