#!/usr/bin/env bash
# Run one worker with durable completion evidence for a detached caller.
set -u

RUN_DIR=""
TRANSCRIPT=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --run-dir)
      [ "$#" -ge 2 ] || { echo "--run-dir needs a value" >&2; exit 2; }
      RUN_DIR="$2"
      shift 2
      ;;
    --transcript)
      [ "$#" -ge 2 ] || { echo "--transcript needs a value" >&2; exit 2; }
      TRANSCRIPT="$2"
      shift 2
      ;;
    --)
      shift
      break
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

[ -n "$RUN_DIR" ] || { echo "--run-dir is required" >&2; exit 2; }
[ -n "$TRANSCRIPT" ] || { echo "--transcript is required" >&2; exit 2; }
[ "$#" -gt 0 ] || { echo "worker command is required after --" >&2; exit 2; }

mkdir -p "$RUN_DIR" "$(dirname "$TRANSCRIPT")"
done_path="$RUN_DIR/done"
worker_pid_path="$RUN_DIR/worker.pid"
wrapper_pid_path="$RUN_DIR/wrapper.pid"
rm -f -- "$done_path" "$worker_pid_path" "$wrapper_pid_path"

# The provider command is the direct child. The helper's PID is recorded
# separately, so callers never mistake the shell wrapper for the worker.
"$@" >"$TRANSCRIPT" 2>&1 &
worker_pid=$!
printf '%s\n' "$worker_pid" >"$worker_pid_path.tmp.$$"
mv -f -- "$worker_pid_path.tmp.$$" "$worker_pid_path"
printf '%s\n' "$$" >"$wrapper_pid_path.tmp.$$"
mv -f -- "$wrapper_pid_path.tmp.$$" "$wrapper_pid_path"

wait "$worker_pid"
status=$?
printf 'wrapper_pid=%s\nworker_pid=%s\nexit=%s\n' "$$" "$worker_pid" "$status" >"$done_path.tmp.$$"
mv -f -- "$done_path.tmp.$$" "$done_path"
exit "$status"
