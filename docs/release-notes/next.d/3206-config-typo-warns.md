## Fixes
### CLI and install

- A misspelt or invalid knob in `config.toml` now warns, naming the file, line and key and suggesting the nearest valid knob, instead of refusing `status`, `config show` and every launch. A knob that grants authority, such as `lanes.publish`, takes its restrictive value on an invalid entry, and a configuration problem is reported as `CONFIG` rather than `IDENTITY`; `config set`, `config apply` and `config unset` validate names before writing (#3206).
