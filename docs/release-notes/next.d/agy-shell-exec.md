## Fixes

- Agy writer lanes can now run commands. Every `run_command` failed with
  "fork/exec /bin/zsh: operation not permitted", because the profile did not
  allow the shell to run. Agy now runs with `SHELL=/bin/zsh`, and writers get
  the exec grants Codex and Claude writers have: `/bin`, `/usr/bin`,
  `/usr/libexec`, Cargo, rustup, Git helpers, Homebrew and the working
  directory (#1695).
