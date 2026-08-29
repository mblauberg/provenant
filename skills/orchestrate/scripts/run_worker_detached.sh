#!/usr/bin/env bash
# Run one worker with durable completion evidence for a detached caller.
set -u

RUN_DIR=""
VALIDATE=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --validate)
      VALIDATE=1
      shift
      ;;
    --run-dir)
      [ "$#" -ge 2 ] || { echo "--run-dir needs a value" >&2; exit 2; }
      RUN_DIR="$2"
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

read_single_value() {
  value_path=$1
  [ -f "$value_path" ] || return 1
  [ "$(wc -l <"$value_path" | tr -d ' ')" -eq 1 ] || return 1
  value=$(sed -n '1p' "$value_path")
  [ -n "$value" ] || return 1
  printf '%s' "$value"
}

validate_run() {
  validate_dir=$1
  validate_done="$validate_dir/done"
  validate_worker_pid_path="$validate_dir/worker.pid"
  validate_wrapper_pid_path="$validate_dir/wrapper.pid"
  validate_wrapper_pid=$(read_single_value "$validate_wrapper_pid_path") || {
    echo "completion evidence missing wrapper PID" >&2
    return 2
  }
  case "$validate_wrapper_pid" in
    ''|*[!0-9]*) echo "completion evidence has invalid wrapper PID" >&2; return 2 ;;
  esac

  validate_worker_pid=$(read_single_value "$validate_worker_pid_path") || {
    if kill -0 "$validate_wrapper_pid" 2>/dev/null && [ ! -s "$validate_done" ]; then
      return 1
    fi
    echo "completion evidence missing worker PID" >&2
    return 2
  }
  case "$validate_worker_pid" in
    ''|*[!0-9]*) echo "completion evidence has invalid worker PID" >&2; return 2 ;;
  esac

  if [ ! -s "$validate_done" ]; then
    if kill -0 "$validate_wrapper_pid" 2>/dev/null; then
      return 1
    fi
    echo "completion evidence missing: wrapper exited without marker" >&2
    return 2
  fi

  validate_done_lines=$(wc -l <"$validate_done" | tr -d ' ')
  validate_done_wrapper=$(sed -n 's/^wrapper_pid=\([0-9][0-9]*\)$/\1/p' "$validate_done")
  validate_done_worker=$(sed -n 's/^worker_pid=\([0-9][0-9]*\)$/\1/p' "$validate_done")
  validate_done_exit=$(sed -n 's/^exit=\([0-9][0-9]*\)$/\1/p' "$validate_done")
  case "$validate_done_exit" in
    ''|*[!0-9]*)
      echo "completion evidence marker has invalid exit status" >&2
      return 2
      ;;
  esac
  if [ "$validate_done_lines" -ne 3 ] || [ "$(printf '%s\n' "$validate_done_wrapper" | wc -l | tr -d ' ')" -ne 1 ] ||
    [ "$(printf '%s\n' "$validate_done_worker" | wc -l | tr -d ' ')" -ne 1 ] ||
    [ "$(printf '%s\n' "$validate_done_exit" | wc -l | tr -d ' ')" -ne 1 ] ||
    [ "$validate_done_wrapper" != "$validate_wrapper_pid" ] ||
    [ "$validate_done_worker" != "$validate_worker_pid" ]; then
    echo "completion evidence marker is malformed or does not match persisted PIDs" >&2
    return 2
  fi
  if kill -0 "$validate_wrapper_pid" 2>/dev/null; then
    return 1
  fi
  if kill -0 "$validate_worker_pid" 2>/dev/null; then
    echo "completion evidence worker PID is still live" >&2
    return 2
  fi
  printf '%s %s %s\n' "$validate_worker_pid" "$validate_wrapper_pid" "$validate_done_exit"
  return 0
}

[ -n "$RUN_DIR" ] || { echo "--run-dir is required" >&2; exit 2; }
if [ "$VALIDATE" -eq 1 ]; then
  [ -d "$RUN_DIR" ] || { echo "run directory does not exist: $RUN_DIR" >&2; exit 2; }
  [ "$#" -eq 0 ] || { echo "--validate does not accept a worker command" >&2; exit 2; }
  validate_run "$RUN_DIR"
  exit $?
fi
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

transcript="$RUN_DIR/transcript.txt"
if ! : >"$transcript"; then
  echo "run transcript is not writable: $transcript" >&2
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

if ! write_atomic "$wrapper_pid_path" "$$"; then
  echo "cannot persist wrapper PID before launch" >&2
  exit 2
fi

# The provider command is the direct child. The helper's PID is recorded
# separately, so callers never mistake the shell wrapper for the worker.
"$@" >"$transcript" 2>&1 <&0 &
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
