## Fixes

- A failed automatic graph advance is now visible: `fabric graph status`
  reports `advance_error` in human and JSON output until a later advance
  succeeds. An unreadable writer reservation stalls only the attempt whose
  writer directory holds it, not graph advancement for other attempts in the
  project, and `graph resume` from a different directory in the same project
  no longer conflicts (#1712).
