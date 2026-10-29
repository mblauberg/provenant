## Fixes

- Attempt runtime deadlines now hold on the wall clock. The guardian measured
  them on the monotonic clock, which stops while a host sleeps, so a laptop
  asleep through a deadline kept its provider, worktree, lock and quota for
  hours. The provider is now stopped on whichever clock reaches the deadline
  first, and the attempt settles as `timed_out` with the reason `runtime
  deadline exceeded`. `lanes` shows each running lane's `runtime_deadline` and
  a resume hint for a timed-out lane; `reconcile` asks a live guardian that is
  more than two minutes past its deadline to stop. `providers.<id>.runtime_deadline_seconds`
  caps the deadline per provider.
