## Behaviour changes

- Branch names follow Git's own ref-format rules, so names such as `feat/a+b` work.
- Repositories with `filter.*` (such as Git LFS), `include.*`, `includeIf.*` or `info/attributes` are no longer refused. Host-side Git reads disable every configured filter driver instead.
- The pinned Git executable is resolved once and must be owned by the current user or root and not world-writable. Group-writable directories such as Intel Homebrew's `/usr/local/bin` are accepted, and a later `brew upgrade git` no longer breaks long-lived processes.
