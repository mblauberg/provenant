#!/usr/bin/env bash

# Shared interpreter ladder for installer and model-route callers.
# Keep this order aligned with scripts/check-harness:
# HARNESS_PYTHON, the product .venv, compatible python3, then uv.

model_route_product_root() {
  local helper_root
  helper_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  printf '%s\n' "${AGENT_FABRIC_PRODUCT_ROOT:-$helper_root}"
}

resolve_model_route_python() {
  local helper_root product_root python3_path resolved
  helper_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
  product_root="$(model_route_product_root)"
  [[ "$product_root" == /* ]] || {
    echo "model-route: product root must be absolute: $product_root" >&2
    return 3
  }

  if [[ -x "${HARNESS_PYTHON:-}" ]]; then
    printf '%s\n' "$HARNESS_PYTHON"
    return 0
  fi
  if [[ -x "$product_root/.venv/bin/python" ]]; then
    printf '%s\n' "$product_root/.venv/bin/python"
    return 0
  fi
  if python3_path="$(command -v python3 2>/dev/null)" &&
     "$python3_path" -c 'import yaml, tomllib' >/dev/null 2>&1; then
    printf '%s\n' "$python3_path"
    return 0
  fi
  if command -v uv >/dev/null 2>&1; then
    resolved="$(uv run --project "$helper_root" --frozen --only-group test python \
      -c 'import sys; print(sys.executable)')" || return 3
    [[ -x "$resolved" ]] || {
      echo "model-route: uv did not resolve an executable Python" >&2
      return 3
    }
    printf '%s\n' "$resolved"
    return 0
  fi

  echo "model-route: no Python with PyYAML and tomllib found; install uv or set HARNESS_PYTHON" >&2
  return 3
}

exec_model_route_python() {
  local python
  python="$(resolve_model_route_python)" || return
  exec "$python" "$@"
}
