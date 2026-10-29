## Fixes

- Lanes now run the installed `pnpm` instead of switching to the version a project pins, which failed with `ENOEXEC`. Lane environments set `manage-package-manager-versions` and `package-manager-strict` to `false` (#1944).
