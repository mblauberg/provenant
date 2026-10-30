## Fixes

- The Console lists the open requests of every registered project on the host, each row naming its project, and answers them. It uses the new `--all-projects` mode for listing, inspecting and answering requests, regardless of its working directory. Host-wide lists support cursors and filters; without the flag, inbox commands remain scoped to the current project and report `no_project` with a repair when no project resolves.
- `Provenant Console.app` now has the Provenant icon, compiled from `assets/brand/Provenant.icon` by `bundle` when Xcode is installed.
