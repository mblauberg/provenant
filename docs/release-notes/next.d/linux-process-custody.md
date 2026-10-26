## Breaking changes

- Process custody is supported only on macOS. Linux provider launch and
  guardian setup return an explicit unsupported error. Linux subreaping,
  adopted-child signalling and kernel-children settlement proof are removed;
  existing `linux_kernel_children` settlement records are refused. Utility
  execution and process observation retain their Linux implementations. See
  the [supervisor spec](../../specs/supervisor.md) (#1571).
- CI always runs the macOS supervisor and confinement tests, including on
  changes outside Rust paths (#1571).
