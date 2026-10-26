## Breaking changes

- Installer cut-over support is removed: the `--legacy-checkout` install flag,
  the `LEGACY_CHECKOUT` error code, pre-1.0 cleanup and Git launcher ownership
  are removed. `check install` no longer reports the finding rules
  `legacy_entry`, `legacy_hook_conflict`, `legacy_instruction_variable` or
  `legacy_checkout_unavailable`. Receipts with a non-null `legacy` object need
  a reinstall; see [receipt recovery](../../runbooks/install.md#receipt-recovery).
  Current receipts with `"legacy":null` upgrade normally; newly written
  receipts omit the key (#1571).
