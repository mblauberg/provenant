#!/usr/bin/env bash

# Interpreter ladder for model-route's Python dependency.
# scripts/check-harness keeps its own ladder because it probes pytest and yaml.

exec_model_route_python() {
  local helper_root product_root python3_path
  helper_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  product_root="${AGENT_FABRIC_PRODUCT_ROOT:-$helper_root}"
  [[ "$product_root" == /* ]] || {
    echo "model-route: product root must be absolute: $product_root" >&2
    return 3
  }

  if [[ -x "${HARNESS_PYTHON:-}" ]]; then
    exec "$HARNESS_PYTHON" "$@"
  fi
  if [[ -x "$product_root/.venv/bin/python" ]]; then
    exec "$product_root/.venv/bin/python" "$@"
  fi
  if python3_path="$(command -v python3 2>/dev/null)" &&
     "$python3_path" -c 'import yaml' >/dev/null 2>&1; then
    exec "$python3_path" "$@"
  fi
  if command -v uv >/dev/null 2>&1; then
    exec uv run --project "$helper_root" --frozen --only-group test python "$@"
  fi

  echo "model-route: no Python with PyYAML found; install uv or set HARNESS_PYTHON" >&2
  return 3
}
