## Behaviour changes
### Git and safety

- Repositories that use `filter.*` (such as Git LFS), `include.*`, `includeIf.*` or `info/attributes` are no longer refused. Host-side Git reads disable configured filter drivers, and includes that point into a lane-writable location are checked per worktree (#2147).
- Host-side fetch and push refuse a `core.sshCommand`, `credential.helper` or `uploadpack`/`receivepack` setting that runs a program from a lane-writable location. Helpers kept elsewhere still work (#2147).
- The pinned Git executable keeps its stable path, such as `/opt/homebrew/bin/git`, so `brew upgrade git` no longer breaks long-lived processes. Its target must not be group- or world-writable, though root- or user-owned group-writable directories such as `/usr/local/bin` are accepted (#2147).
