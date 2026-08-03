#!/usr/bin/env bash

# Public API: run_stdlib, run_yaml and run_test. Each call selects and probes
# exactly one interpreter before invoking it.

_run_harness_python() {
  local probe="$1"
  shift
  local product_root candidate primary_root common_dir
  product_root="${AGENT_FABRIC_PRODUCT_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
  candidate="${HARNESS_PYTHON:-$product_root/.venv/bin/python}"

  # A linked worktree is a checkout, not a built environment, and provisioning
  # one virtualenv per lane would be wasteful. When this checkout has no venv,
  # borrow the primary checkout's: the common Git directory names it.
  if test -z "${HARNESS_PYTHON:-}" && ! test -x "$candidate"; then
    common_dir="$(git -C "$product_root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"
    if test -n "$common_dir"; then
      primary_root="$(dirname "$common_dir")"
      if test -x "$primary_root/.venv/bin/python"; then
        candidate="$primary_root/.venv/bin/python"
      fi
    fi
  fi

  if ! test -f "$candidate" || ! test -x "$candidate"; then
    printf 'harness-python: unusable interpreter: %s\n' "$candidate" >&2
    printf 'repair: uv sync --project %s --locked --only-group test\n' "$product_root" >&2
    return 3
  fi
  if ! PYTHONNOUSERSITE=1 "$candidate" -c "$probe" >/dev/null 2>&1; then
    printf 'harness-python: interpreter probe failed: %s\n' "$candidate" >&2
    printf 'repair: uv sync --project %s --locked --only-group test\n' "$product_root" >&2
    return 3
  fi
  PYTHONNOUSERSITE=1 "$candidate" "$@"
}

# The floor is checked with an explicit exit rather than `assert`, because
# PYTHONOPTIMIZE in the caller's environment compiles an assert away and would
# let a stock 3.9 interpreter through the probe.
_VERSION_FLOOR='import sys; sys.version_info >= (3, 11) or sys.exit(1)'

run_stdlib() {
  _run_harness_python "$_VERSION_FLOOR" "$@"
}

run_yaml() {
  _run_harness_python "$_VERSION_FLOOR; import yaml" "$@"
}

run_test() {
  _run_harness_python "$_VERSION_FLOOR; import pytest, yaml" "$@"
}
