## Fixes

- Lanes run the installed pnpm instead of switching to the version a project pins. The downloaded `@pnpm/exe` launcher needs a hard link that lanes cannot create, so running it failed with `ENOEXEC`. Lane environments set `manage-package-manager-versions` and `package-manager-strict` to `false` (#1944).
