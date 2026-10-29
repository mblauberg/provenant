## Behaviour changes
### CLI and install

- The CLI is now organised around `run`, `status`, `land`, `quota` and `install`. `help --all` and `help COMMAND` show the advanced commands (#1947).
- `run` now waits up to 240 seconds for its result and prints a short tail. On timeout it prints the exact wait command, and `--wait` accepts durations such as `30m` (#1947).
- `status` is now the overview of active and recent lanes, with `--all` and `--json` for full records (#1947).
- `run`, `dispatch`, `cancel` and `resume` print a generated operation ID when none is given, so pass one explicitly for planned retries. Run and attempt targets accept a unique hex prefix of eight or more characters (#1947).
