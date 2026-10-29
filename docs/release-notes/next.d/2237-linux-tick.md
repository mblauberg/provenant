## Additions
### Pipelines and graphs

Linux install and upgrade write a systemd user timer and service that run
`provenant tick` every 60 seconds, matching the macOS LaunchAgent. Uninstall
disables the timer and removes both units; reinstall is idempotent.
