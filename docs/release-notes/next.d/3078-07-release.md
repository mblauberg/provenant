## Breaking changes
### Lanes and settlement

- **Replace the old disk policy keys** `policy.LAUNCH_DISK`, `policy.ADMISSION_DISK` and `policy.BUILD_DISK` with `policy.DISK_FLOOR`. Low disk space warns by default; set it to `refuse` for a hard floor. (#3078)
