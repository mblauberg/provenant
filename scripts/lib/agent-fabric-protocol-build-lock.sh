# Shared mutual exclusion for writers of agent-fabric-protocol/dist.
#
# `/bin/sh` on macOS has no portable `flock(1)`, so the lock is an atomically
# created directory beside the canonical dist directory. The holder records
# its PID only for stale-lock recovery: a dead holder is reclaimed, while PID
# reuse or an uninspectable owner can delay progress only until the bounded
# timeout. No naive PID-file creation is used as the exclusion primitive.

protocol_build_lock_failure() {
  echo "AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TIMEOUT: timed out waiting for exclusive repair of \"$_afpbl_dist_directory\"" >&2
  # A holder that died mid-reclaim leaves the lock directory behind, so name it:
  # it is the one thing an operator must remove to unwedge every waiter. Early
  # validation failures reach here before the path is resolved, hence the guard.
  [ -z "${_afpbl_lock_directory:-}" ] || echo "lock: $_afpbl_lock_directory" >&2
  echo "repair: $_afpbl_repair_command" >&2
  exit 78
}

protocol_build_lock_test_signal() {
  [ -n "${AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TEST_HOOK_DIRECTORY:-}" ] \
    || return 0
  : > "$AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TEST_HOOK_DIRECTORY/$$.$1.ready" \
    || protocol_build_lock_failure
}

protocol_build_lock_test_pause() {
  [ -n "${AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TEST_HOOK_DIRECTORY:-}" ] \
    || return 0
  # Solely a deterministic test seam for the stale-owner reclaim interleaving.
  protocol_build_lock_test_signal "$1"
  while [ ! -f "$AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TEST_HOOK_DIRECTORY/$$.$1.continue" ]; do
    _afpbl_test_now=$(date +%s) || protocol_build_lock_failure
    [ $((_afpbl_test_now - _afpbl_started)) -lt "$_afpbl_timeout" ] \
      || protocol_build_lock_failure
    sleep 1
  done
}

protocol_build_lock_release() {
  [ "${_afpbl_held:-false}" = true ] || return 0
  _afpbl_recorded_owner=$(cat "$_afpbl_lock_directory/owner" 2>/dev/null) \
    || _afpbl_recorded_owner=
  if [ "$_afpbl_recorded_owner" = "$$" ]; then
    rm -f "$_afpbl_lock_directory/owner"
    rmdir "$_afpbl_lock_directory" 2>/dev/null || :
  fi
  _afpbl_held=false
}

protocol_build_lock_acquire() {
  _afpbl_dist_requested=${1%/}
  _afpbl_repair_command=$2
  _afpbl_timeout=${AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TIMEOUT_SECONDS:-120}
  case "$_afpbl_timeout" in
    ""|*[!0-9]*|0)
      echo "AGENT_FABRIC_PROTOCOL_BUILD_LOCK_INVALID: AGENT_FABRIC_PROTOCOL_BUILD_LOCK_TIMEOUT_SECONDS must be a positive integer" >&2
      echo "repair: $_afpbl_repair_command" >&2
      exit 78
      ;;
  esac
  case "$_afpbl_dist_requested" in
    /*) ;;
    *)
      echo "AGENT_FABRIC_PROTOCOL_BUILD_LOCK_INVALID: protocol dist path must be absolute" >&2
      echo "repair: $_afpbl_repair_command" >&2
      exit 78
      ;;
  esac

  _afpbl_dist_parent=$(CDPATH= cd -- "${_afpbl_dist_requested%/*}" && pwd -P) \
    || protocol_build_lock_failure
  _afpbl_dist_name=${_afpbl_dist_requested##*/}
  _afpbl_dist_directory="$_afpbl_dist_parent/$_afpbl_dist_name"
  _afpbl_lock_directory="$_afpbl_dist_parent/.${_afpbl_dist_name}.agent-fabric-protocol-build.lock"
  _afpbl_started=$(date +%s) || protocol_build_lock_failure
  _afpbl_held=false

  while :; do
    if mkdir "$_afpbl_lock_directory" 2>/dev/null; then
      if ! printf '%s\n' "$$" > "$_afpbl_lock_directory/owner"; then
        rmdir "$_afpbl_lock_directory" 2>/dev/null || :
        protocol_build_lock_failure
      fi
      _afpbl_held=true
      trap 'protocol_build_lock_release' EXIT
      trap 'exit 129' HUP
      trap 'exit 130' INT
      trap 'exit 143' TERM
      return 0
    fi

    _afpbl_owner=$(cat "$_afpbl_lock_directory/owner" 2>/dev/null) \
      || _afpbl_owner=
    case "$_afpbl_owner" in
      ""|*[!0-9]*)
        # Give a just-created holder time to publish its owner before treating
        # an absent or partial record as a crashed acquisition.
        sleep 1
        _afpbl_owner_after=$(cat "$_afpbl_lock_directory/owner" 2>/dev/null) \
          || _afpbl_owner_after=
        if [ "$_afpbl_owner_after" = "$_afpbl_owner" ]; then
          rm -f "$_afpbl_lock_directory/owner"
          rmdir "$_afpbl_lock_directory" 2>/dev/null || :
        fi
        ;;
      *)
        if ! kill -0 "$_afpbl_owner" 2>/dev/null; then
          protocol_build_lock_test_pause "dead-owner-observed"
          if mkdir "$_afpbl_lock_directory/reclaim" 2>/dev/null; then
            _afpbl_owner_after=$(cat "$_afpbl_lock_directory/owner" 2>/dev/null) \
              || _afpbl_owner_after=
            if [ "$_afpbl_owner_after" = "$_afpbl_owner" ] \
              && ! kill -0 "$_afpbl_owner_after" 2>/dev/null
            then
              protocol_build_lock_test_pause "dead-owner-remove"
              rm -f "$_afpbl_lock_directory/owner"
              rmdir "$_afpbl_lock_directory/reclaim" 2>/dev/null || :
              rmdir "$_afpbl_lock_directory" 2>/dev/null || :
              protocol_build_lock_test_signal "dead-owner-removed"
            else
              rmdir "$_afpbl_lock_directory/reclaim" 2>/dev/null || :
            fi
          else
            protocol_build_lock_test_signal "dead-owner-reclaim-lost"
          fi
        fi
        ;;
    esac

    _afpbl_now=$(date +%s) || protocol_build_lock_failure
    [ $((_afpbl_now - _afpbl_started)) -lt "$_afpbl_timeout" ] \
      || protocol_build_lock_failure
    sleep 1
  done
}
