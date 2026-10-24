# Shell pitfalls

The interactive shell most sessions run commands in is zsh. These behave
differently from a `bash`/`sh` script and each produces a result that reads as
clean when the command in fact never ran, or ran on the wrong input.

- **zsh does not word-split an unquoted variable expansion.** A loop such as
  `for c in $LIST; do ...; done`, a positional split such as `set -- $pair`, or
  a multi-path `rm -f $FILES` passes the whole value as a single word rather
  than splitting it on whitespace. The command runs once against the joined
  string instead of once per item, and nothing errors. Wrap loop or
  multi-value logic in an explicit `bash -c '...'`, use an array, or write out
  each argument literally. In zsh, `${=var}` forces the split, so
  `for row in "a b c"; do sh x.sh $row; done` passes one argument but
  `sh x.sh ${=row}` passes three.
- **Quote heredoc delimiters (`<<'EOF'`) for every brief, comment or body.**
  Briefs contain backticked commands, and an unquoted delimiter runs them: an
  unquoted `python3 - <<EOF` once executed a `git fetch && git merge` embedded
  in the text and fast-forwarded a primary checkout. Pass variables to the
  interpreter as argv.
- **An unquoted glob in a flag aborts the whole command.** `grep -rn PATTERN .
  --include=*.ts` lets zsh expand `--include=*.ts` before `grep` sees it; when
  nothing in the current directory matches that glob, zsh aborts the command
  with `no matches found` and produces no output at all. Read carelessly, an
  empty result looks exactly like a clean search that found nothing, which is
  the opposite of what happened. Always quote a glob passed as a flag value:
  `--include="*.ts"`.
- **The last element of a compound command reports its own exit status, not
  the status of the command whose result you actually want.** `cmd > log 2>&1;
  echo "rc=$?"` and `cmd 2>&1 | tail -3` both report the exit status of the
  trailing `echo` or `tail`, which is close to always zero. Capture the target
  command's own output to a file, check its exit code immediately after it
  runs, and read the log for the tool's own summary line before reporting a
  result as green.
- **A verification gate run inside a backgrounded command is not verification
  you have read.** Appending a check to a command launched to run in the
  background sends the check's output to that background task's own log
  rather than to you, so a real failure can sit unread while the surrounding
  work proceeds as though it passed. Run a long job in the background on its
  own; run the gate whose result you intend to act on in the foreground, where
  it lands in front of you before you rely on it.
- **`${PIPESTATUS[0]}` is empty in zsh.** zsh uses the one-indexed
  `$pipestatus[1]`, so `gate 2>&1 | tail -6; echo "EXIT=${PIPESTATUS[0]}"`
  prints an empty code while the trailing `tail` or `echo` supplies a zero
  status. Redirect to a log, check `$?` straight after, then read the summary line.
- **`gh pr view --json statusCheckRollup` reports `.conclusion` as `""`, not
  null, for a running check.** A guard on conclusion alone then reads green.
  Test `.status != "COMPLETED"` first, then `.conclusion`. Two further traps:
  after a force-push the API can still return the previous run's checks, and
  checks register incrementally, so "nothing pending" may mean one skipped
  check. Poll `commits/<head-sha>/check-runs` for the exact head and require a
  known-mandatory check to be present before believing a green.
- **`git cherry`: `-` proves, `+` does not.** A `-` means a patch-identical
  commit exists upstream, which is sound grounds to drop work. A `+` only
  means none was found; work redone with a different shape still reads `+`.
  Settle a `+` by reading what upstream code now does.
- **Long waits can outlive the command window.** If a foreground shell call
  approaches its roughly ten-minute cap, the process may continue without its
  result reaching the caller. Prefer the harness's own background run, which
  re-invokes the caller on exit. Where there is none, run the job under
  `nohup`, capture `$!`, write its exit code to a file, and wait with
  `kill -0 <pid>`; never poll with `pgrep -f`, whose pattern matches the
  waiting shell's own command line. Stopping a waiter can kill its process
  group, so launch the job from its own call, not from the waiter.
- **Quote every heredoc delimiter.** `<<'EOF'` keeps the body literal; an
  unquoted `<<EOF` expands `$(...)` and backticks inside the body, and once ran
  a backticked `git` command that was only meant to be text in a commit message.
- **Wait on a background PID with `kill -0`, never `pgrep -f`.** The pattern
  matches the waiting shell's own command line, so the wait never ends.
  Stopping a background task can kill its whole process group, including a
  sibling you meant to keep; cancel owned runs with `provenant cancel` and an
  explicit target and operation ID.
- **A wildcard git pathspec matches across slashes.** `apps/*/src/**` matches
  the whole path, not one segment; use `:(glob)apps/*/src/**` for glob
  semantics.
- **Never overwrite a running script in place.** The shell reads it
  incrementally, so write a temp file and `mv` it over.
- **macOS has no `timeout`.** Use the tool's own timeout flag or a background
  run with a wait.

None of these are bugs in the tools involved; they are the documented
behaviour of the shell doing exactly what it was asked. Treat a search or
check that "found nothing" or "passed" with a shrug of suspicion until you
have confirmed which command's status you are actually looking at.
