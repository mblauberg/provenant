## Additions
### CLI and install

- `provenant adapter capabilities [--json]` prints each adapter's declared capabilities, with the reason and source of every `no`, its harness version compatibility and measured launch, first-output and settlement times. `--adapter <id>` (repeatable) narrows the list and `--no-probe` skips the version read. `[[adapter_capabilities]]` entries in host or project config narrow an adapter's facts with `disable = [...]` and a `reason`; the matrix and `check` name the layer that narrowed them. The `domains.adapter.capabilities.measurements`, `measurement_samples` and `measurement_records` settings bound the measurements. (#4039, #4034)
