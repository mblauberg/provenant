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

run_parent=$(dirname -- "$RUN_DIR")
mkdir -p -- "$run_parent" || {
  echo "cannot create run-directory parent: $run_parent" >&2
  exit 2
}
if ! mkdir -- "$RUN_DIR"; then
  echo "run directory already exists or cannot be claimed: $RUN_DIR" >&2
  exit 2
fi
if [ ! -w "$RUN_DIR" ]; then
  echo "run directory is not writable: $RUN_DIR" >&2
  exit 2
fi

transcript_parent=$(dirname -- "$TRANSCRIPT")
mkdir -p -- "$transcript_parent" || {
  echo "cannot create transcript parent: $transcript_parent" >&2
  exit 2
}
if [ -e "$TRANSCRIPT" ]; then
  echo "transcript path already exists: $TRANSCRIPT" >&2
  exit 2
fi
if ! : >"$TRANSCRIPT"; then
  echo "transcript path is not writable: $TRANSCRIPT" >&2
  exit 2
fi

done_path="$RUN_DIR/done"
worker_pid_path="$RUN_DIR/worker.pid"
wrapper_pid_path="$RUN_DIR/wrapper.pid"

write_atomic() {
  target=$1
  value=$2
  temporary="$target.tmp.$$"
  if ! printf '%s\n' "$value" >"$temporary"; then
    rm -f -- "$temporary"
    return 1
  fi
  if ! mv -f -- "$temporary" "$target"; then
    rm -f -- "$temporary"
    return 1
  fi
}

# Claim and validate every marker path before starting an untracked worker.
preflight_path="$RUN_DIR/.write-test.$$"
if ! ( : >"$preflight_path" && rm -f -- "$preflight_path" ); then
  echo "run-directory marker paths are not writable: $RUN_DIR" >&2
  exit 2
fi
if ! write_atomic "$wrapper_pid_path" "$$"; then
  echo "cannot persist wrapper PID before launch" >&2
  exit 2
fi
if ! write_atomic "$worker_pid_path" "pending"; then
  echo "cannot persist worker PID before launch" >&2
  exit 2
fi

# The provider command is the direct child. The helper's PID is recorded
# separately, so callers never mistake the shell wrapper for the worker.
"$@" >"$TRANSCRIPT" 2>&1 <&0 &
worker_pid=$!
if ! write_atomic "$worker_pid_path" "$worker_pid"; then
  echo "cannot persist worker PID after launch; terminating worker" >&2
  kill "$worker_pid" 2>/dev/null || true
  wait "$worker_pid" 2>/dev/null || true
  exit 1
fi

wait "$worker_pid"
status=$?
if ! {
  temporary="$done_path.tmp.$$"
  printf 'wrapper_pid=%s\nworker_pid=%s\nexit=%s\n' "$$" "$worker_pid" "$status" >"$temporary" &&
    mv -f -- "$temporary" "$done_path"
}; then
  rm -f -- "$done_path.tmp.$$"
  echo "cannot persist durable completion evidence" >&2
  exit 1
fi
exit "$status"
