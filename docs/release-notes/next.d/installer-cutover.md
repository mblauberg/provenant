## Breaking changes

- Installer cut-over support is removed: the `--legacy-checkout` install flag,
  the `LEGACY_CHECKOUT` error code, pre-1.0 cleanup and Git launcher ownership
  are removed. `check install` no longer reports the finding rules
  `legacy_entry`, `legacy_hook_conflict`, `legacy_instruction_variable` or
  `legacy_checkout_unavailable`. A receipt's `legacy` record is now ignored and
  dropped on the next install write, so receipts from the cut-over installer
  upgrade normally. Any Git hook launchers that installer placed under the
  recorded `git_hooks_dir` are no longer managed; remove them by hand if wanted
  (#1571).
