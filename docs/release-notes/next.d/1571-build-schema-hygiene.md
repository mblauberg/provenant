## Behaviour changes

- Everyday development and test builds no longer invoke the rustc path-remapping wrapper, keeping panic and debug paths local and avoiding wrapper overhead. Release packaging and distribution builds configure `RUSTC_WRAPPER` explicitly for reproducible builds (#1571).
- Dependency verification in cargo-deny denies duplicate versions of crates across the workspace graph (#1571).
