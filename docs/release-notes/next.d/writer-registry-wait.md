## Fixes
### Lanes and settlement

- Concurrent writer launches on distinct branches now wait for the repository registry within the launch wait budget. A lock held beyond that budget fails with `writer_lock_busy`, and launch failures before an attempt exists include the code and message in the JSON error.
