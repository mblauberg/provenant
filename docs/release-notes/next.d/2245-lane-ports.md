## Additions
### Lanes and settlement

- Each writer lane now holds a private block of ports and sees it as `PROVENANT_LANE_PORT_BASE` and `PROVENANT_LANE_PORT_COUNT`, so parallel lanes running dev servers or tests do not collide. The blocks come from one host-wide range set by `lane_port_base` (default 30000), `lane_port_block` (default 16) and `lane_port_blocks` (default 64, or 0 to turn it off) in `[domains.fabric.lanes]`. A block is an advisory lock held until the lane's whole process tree has exited and dropped by the kernel then, so nothing leaks. A writer launched when every block is held does not start and reports `no free lane port block` (#2245).
