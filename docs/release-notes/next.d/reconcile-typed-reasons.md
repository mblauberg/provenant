## Breaking changes

- The `reason` of an `uncertain` result from `provenant reconcile` is now an
  object with a `kind` (for example `writer_custody_unconfirmed`,
  `custody_not_provably_absent` or `guardian_not_observable`) and, where it
  applies, the underlying `error` text or the `reservation` path. It used to be
  free text. The human line is unchanged. The disk report no longer counts a
  `logs` entry in an attempt directory as retained: nothing writes one (#1571).
