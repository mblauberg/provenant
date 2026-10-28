## Behaviour changes

- Writer lanes now have a complete private Git directory, including their index,
  refs and operation state, so stash, rebase and merge can run while shared Git
  metadata stays read-only. This covers #2158, #2155, #2139 and #2146.
- Settlement uses Git receive quarantine and fsck checks instead of the custom
  object verifier and full-store scans. Its durable phases resume after crashes,
  bound retries and keep cancel and reconcile responsive. Provider exit releases
  host build capacity and live custody before settlement completes. This covers
  #2157, #2138, #2141, #2151 and #2154.
- Drain writers before upgrading. Interrupted older attempts with private objects
  can resume; attempts without them name host recovery and a fresh run.
