## Fixes

- The host materialises a loose lane branch ref and configures its upstream before launch, so a writer can commit when its branch was packed-only. Remote-tracking refs and reflogs remain read-only to lanes.
