## Fixes

- `lanes --wait` and `lanes --watch` retry run-store I/O errors until their
  deadline, like peer failures. If reads keep failing, they report the last
  error and exit non-zero instead of reporting the condition as met.
