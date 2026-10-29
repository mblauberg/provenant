## Behaviour changes
### Git and safety

- Repositories that use `filter.*` (such as Git LFS), `include.*`, `includeIf.*` or `info/attributes` are no longer refused (#2147).
- Host-side fetch and push refuse a `core.sshCommand`, `credential.helper` or `uploadpack`/`receivepack` setting that runs a program from a lane-writable location (#2147).
- The pinned Git executable keeps its stable path, such as `/opt/homebrew/bin/git`, so `brew upgrade git` no longer breaks long-lived processes (#2147).
