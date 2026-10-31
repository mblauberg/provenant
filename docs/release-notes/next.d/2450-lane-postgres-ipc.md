## Fixes
### Lanes and settlement

- macOS lanes can initialise, query and cleanly stop PostgreSQL when launched with `--capability postgres`. Configure `dynamic_shared_memory_type=mmap` during initialisation and server startup; ordinary lanes keep database IPC denied, and signals outside the lane's sandbox remain denied (#2450).
