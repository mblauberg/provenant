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
  each argument literally.
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
- **Long waits can outlive the command window.** If a foreground shell call
  approaches its roughly ten-minute cap, the process may continue without its
  result reaching the caller. Run longer work under `nohup`, write its exit
  code to a file, and use a separate waiter that reports completion before
  acting on the result.

None of these are bugs in the tools involved; they are the documented
behaviour of the shell doing exactly what it was asked. Treat a search or
check that "found nothing" or "passed" with a shrug of suspicion until you
have confirmed which command's status you are actually looking at.
