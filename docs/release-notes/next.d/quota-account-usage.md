## Additions

`provenant quota usage` shows the last seven days of recorded use across
projects, grouped by account. Use `--days` to change the window and `--adapter`
to select a provider. Lane rows show recorded token categories and missing
measurements for registered projects. Transient workspace lane attempts are
excluded. Chair rows show distinct sessions only, since chair token usage
is not recorded. These counts are not provider quota percentages.

Claude chair SessionStart hooks now record the serving registered account,
including when no account-switch handoff is pending. Repeated starts for the
same session count once per account and project in the selected window.
Chair observations use the existing bounded, rotated telemetry log. Historical
chair tokens are not recovered and transcripts are not read.

Chair observations store the serving account key and session ID. Project IDs
are recorded only when `usage.project_attribution` is enabled. Otherwise, or
when an existing project identity is unavailable, the report shows an unknown
project. Attribution never registers projects, and attribution failures do not
prevent consuming an account-switch handoff.
