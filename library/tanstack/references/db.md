# TanStack DB (experimental)

Version-sensitive guidance last checked 22 October 2026; verify against the installed package.

The `@tanstack/db` 0.15.0 README badges it alpha, and the CLI lists it at v0.
Do not adopt it in app code without an accepted proof.

- Collections, live queries and optimistic mutations extend Query. Query stays
  the owner of server state elsewhere in the app.
- It ships Intent skills (`db-core`, `meta-framework`) in the package. Load
  them for the installed version, and re-read them on every upgrade.
- Pin an exact version. Report API drift instead of working around it.
