## Fixes
### CLI and install

- Contributors: development and test builds no longer use `RUSTC_WRAPPER`, which release packaging still sets explicitly for reproducible builds, and cargo-deny now denies duplicate versions of a dependency across the workspace (#1571).
