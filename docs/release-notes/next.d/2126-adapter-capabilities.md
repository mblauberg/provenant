## Changes
### Routing and dispatch

- Each adapter now declares its launch capabilities (effort, resume, steering, network modes, image input, write mode). Dispatch, `run`, resume and graph validation refuse an unsupported request with the adapter and field named, before any worktree, reservation or receipt exists. Cursor takes no effort control, so `--effort` on a Cursor model is refused (#2126).
