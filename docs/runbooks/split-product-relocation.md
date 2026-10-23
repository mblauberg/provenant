# Product and instance roots

The product checkout supplies code and shipped assets. The instance root owns
user instructions, configuration and machine-local state. The Rust state design
uses explicit `Roots` for these locations and installs a verified release under
the selected state root.

The release and managed-file operations are library entrypoints. They do not
read an old receipt or `.agent-fabric/product-root.json`, and this slice does
not migrate an existing installation. CLI wiring for root selection, explicit
harness selection and relocation remains in the later harness cut-over.

The old `scripts/install-harness` entrypoint refuses the retired Python install
path. This runbook therefore has no relocation command until the Rust CLI
cut-over lands. Uninstall is receipt-scoped: it removes only unchanged owned
entries and never recursively deletes a checkout or state directory.
