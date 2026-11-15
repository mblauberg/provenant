- The `pre-tool-use` answer hook no longer denies a tool when it cannot use its input. It only
  offers queued human answers; shell and file policy belong to confinement. Input from another
  harness's event shape (Cursor runs Claude hooks with its own events), a malformed body or a
  timeout now abstains with a diagnostic, so Cursor lanes can run shell commands.
