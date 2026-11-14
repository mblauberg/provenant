Provenant 2.0.0 runs from a fresh state root, `~/.local/share/provenant/v2`.
The first install copies your 1.x configuration (`config.toml`, `host-id` and
the install receipt) out of `~/.local/share/provenant/v1` and leaves that root
in place, untouched. Run records and other 1.x state stay behind, so drain
active lanes and land or park their work first. Then run `provenant upgrade`
and `provenant check install` on each host. To roll back, run
`provenant uninstall` and reinstall 1.x, which finds its root intact.
