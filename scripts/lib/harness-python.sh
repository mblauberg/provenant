#!/usr/bin/env bash

# Public API: run_stdlib, run_yaml and run_test. Each call selects and probes
# exactly one interpreter before invoking it.

_run_harness_python() {
  local probe="$1"
  shift
  local product_root candidate
  product_root="${AGENT_FABRIC_PRODUCT_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
  candidate="${HARNESS_PYTHON:-$product_root/.venv/bin/python}"

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

run_stdlib() {
  _run_harness_python 'import sys; assert sys.version_info >= (3, 11)' "$@"
}

run_yaml() {
  _run_harness_python 'import sys; assert sys.version_info >= (3, 11); import yaml' "$@"
}

run_test() {
  _run_harness_python 'import sys; assert sys.version_info >= (3, 11); import pytest, yaml' "$@"
}
