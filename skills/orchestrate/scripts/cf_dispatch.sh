#!/usr/bin/env bash
# Dispatch one prompt to a configured provider CLI with conservative safety defaults.
#
# This script is a helper, not an authority. The caller still chooses an appropriate
# different-family verifier, checks data policy, and records failures in the run manifest.
# Assurance remains the default. Ordinary execution can explicitly allow a same-family route.
# When Fabric is used, the caller records correlation; this helper owns only the
# provider call and its direct receipt.
set -uo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

# The provider must inherit the same repository discovered by the validator,
# never a repository redirected through the caller's Git environment.
unset GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_CEILING_DIRECTORIES GIT_COMMON_DIR \
  GIT_CONFIG GIT_CONFIG_COUNT GIT_CONFIG_PARAMETERS GIT_DIR \
  GIT_DISCOVERY_ACROSS_FILESYSTEM GIT_GRAFT_FILE GIT_IMPLICIT_WORK_TREE \
  GIT_INDEX_FILE GIT_NAMESPACE GIT_NO_REPLACE_OBJECTS GIT_OBJECT_DIRECTORY \
  GIT_PREFIX GIT_QUARANTINE_PATH GIT_REPLACE_REF_BASE GIT_SHALLOW_FILE \
  GIT_WORK_TREE

usage() {
  cat <<'EOF'
Usage: cf_dispatch.sh --tool TOOL --orchestrator-family FAMILY --prompt TEXT [options]
       cf_dispatch.sh --chain "tool:model:effort ..." --orchestrator-family FAMILY --prompt TEXT [options]
       cf_dispatch.sh --doctor

Options:
  --tool TOOL                  One of claude, codex, cursor, agy, kiro, copilot, opencode.
  --task-class CLASS           Route task class through model_route.py.
  --chain SPECS                Space-separated fallback chain.
  --orchestrator-family FAMILY Labels the chair family; assurance requires separation.
  --intent INTENT              assurance (default) or ordinary execution.
  --alias ALIAS                Durable route alias: flagship, workhorse, scout.
                               Defaults from --role: flagship for lead,
                               orchestrator and critical-review, workhorse otherwise.
  --role ROLE                  Route role (default: reviewer).
  --risk-tier TIER             Lifecycle/receipt risk metadata; never selects a model.
  --model-override-tier TIER   Explicit special-model override tier.
  --reviewer-id ID             Stable worker/reviewer identity for receipt binding.
  --model MODEL                Optional model passed to adapter.
  --effort EFFORT              Optional effort passed to adapter.
  --timeout-seconds N          Supervisor wall clock deadline, all adapters.
  --add-dir PATH               Additional provider directory; repeatable.
  --plan-only                  Print resolved provider argv and controls as JSON.
  --sandbox MODE               read-only, workspace-write, or full.
  --network BOOL               true or false; unsupported controls are warned.
  --resume-session ID          Resume a retained provider session.
  --no-preface                 Omit the route attribution preface.
  --access-mode MODE           read_only (default) or worktree_write.
  --worktree PATH              Git worktree root the writer owns exclusively.
                               Required by, and only valid with, worktree_write.
  --out PATH                   Clean output path; defaults to mktemp.
  --prompt TEXT                Prompt text.
  --prompt-file PATH           Read prompt from file.
  --doctor                     Print local dispatch diagnostics and exit.
  -h, --help                   Show this help.

When Fabric is used, the caller records any Fabric correlation.
EOF
}

TOOL="" MODEL="" EFFORT="" OUT="" PROMPT="" PROMPT_FILE="" CHAIN="" ORCH_FAMILY="" MODEL_ALIAS="" TASK_CLASS="" ROUTE_ROLE="reviewer" RISK_TIER="" MODEL_OVERRIDE_TIER="" REVIEWER_ID="" INTENT="assurance" DOCTOR=0
PLAN_ONLY=0
SANDBOX="" NETWORK="" RESUME_SESSION="" PROVIDER_CWD=""
PREFACE=1
FALLBACK=""
ALIAS_EXPLICIT=0
OUT_CREATED=false
ACTIVE_RUN_TMPDIR=""
INSTALLED_OUTPUT_DIGEST=""
INSTALLED_OUTPUT_DEVICE=""
INSTALLED_OUTPUT_INODE=""
AGY_ADD_DIRS=()
AGY_SANDBOX_JSON=null
# Effective outbound network of the provider sandbox, where the arm controls it:
# true or false for codex, null where the adapter does not expose the switch.
PROVIDER_NETWORK_JSON=null
ACCESS_MODE="read_only"
TIMEOUT_SECONDS=""
WORKTREE=""
WORKTREE_GIT_COMMON=""
need_value() {
  [ $# -ge 2 ] || { echo "missing value for $1" >&2; exit 2; }
}
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0;;
    --doctor) DOCTOR=1; shift;;
    --plan-only) PLAN_ONLY=1; shift;;
    --fallback) need_value "$@"; FALLBACK="$2"; shift 2;;
    --cwd) need_value "$@"; PROVIDER_CWD="$2"; shift 2;;
    --sandbox) need_value "$@"; SANDBOX="$2"; shift 2;;
    --network) need_value "$@"; NETWORK="$2"; shift 2;;
    --resume-session) need_value "$@"; RESUME_SESSION="$2"; shift 2;;
    --no-preface) PREFACE=0; shift;;
    --tool) need_value "$@"; TOOL="$2"; shift 2;;
    --task-class) need_value "$@"; TASK_CLASS="$2"; shift 2;;
    --model) need_value "$@"; MODEL="$2"; shift 2;;
    --effort) need_value "$@"; EFFORT="$2"; shift 2;;
    --add-dir) need_value "$@"; AGY_ADD_DIRS+=("$2"); shift 2;;
    --access-mode) need_value "$@"; ACCESS_MODE="$2"; shift 2;;
    --timeout-seconds) need_value "$@"; TIMEOUT_SECONDS="$2"; shift 2;;
    --worktree) need_value "$@"; WORKTREE="$2"; shift 2;;
    --out) need_value "$@"; OUT="$2"; shift 2;;
    --prompt) need_value "$@"; PROMPT="$2"; shift 2;;
    --prompt-file) need_value "$@"; PROMPT_FILE="$2"; shift 2;;
    --chain) need_value "$@"; CHAIN="$2"; shift 2;;
    --orchestrator-family) need_value "$@"; ORCH_FAMILY="$2"; shift 2;;
    --intent|--execution-intent) need_value "$@"; INTENT="$2"; shift 2;;
    --alias) need_value "$@"; MODEL_ALIAS="$2"; ALIAS_EXPLICIT=1; shift 2;;
    --role) need_value "$@"; ROUTE_ROLE="$2"; shift 2;;
    --risk-tier) need_value "$@"; RISK_TIER="$2"; shift 2;;
    --model-override-tier) need_value "$@"; MODEL_OVERRIDE_TIER="$2"; shift 2;;
    --reviewer-id) need_value "$@"; REVIEWER_ID="$2"; shift 2;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

case "$INTENT" in
  assurance|ordinary) ;;
  *) echo "invalid intent: $INTENT" >&2; exit 2;;
esac

# The caller's deadline, in whole seconds. Only the arms whose CLI accepts a
# headless timeout consume it; the rest stay bounded by the calling owner.
case "$TIMEOUT_SECONDS" in
  "") ;;
  0*|*[!0-9]*) echo "invalid timeout-seconds: $TIMEOUT_SECONDS" >&2; exit 2;;
esac
if [ -n "${CF_DISPATCH_IDLE_SECONDS:-}" ] &&
   { [[ ! "$CF_DISPATCH_IDLE_SECONDS" =~ ^[0-9]+$ ]] || [[ ! "$CF_DISPATCH_IDLE_SECONDS" =~ [1-9] ]]; }; then
  echo "CF_DISPATCH_IDLE_SECONDS must be a positive integer" >&2
  exit 2
fi

# The adapters with an executing arm in run_one below. This is the only
# adapter list the shell keeps: dispatch state (implemented / dormant /
# unsupported) is owned by `dispatch_registry` in
# config/adapter-compatibility.yaml, and runtime/fabric/tests/adapter-registry.test.ts
# binds this list to the registry's implemented set, so the two cannot drift.
# There are no dormant adapters today; a future dormant adapter must be refused
# here (absent from this list) until its arm and safety boundary exist.
DISPATCH_IMPLEMENTED_ADAPTERS="agy claude codex copilot cursor kiro opencode"
# An adapter with no arm is an input error, refused here rather than after a
# temporary directory, prompt staging and route resolution have been paid for.
known_adapter() {
  local candidate="$1" known
  for known in $DISPATCH_IMPLEMENTED_ADAPTERS; do
    [ "$candidate" = "$known" ] && return 0
  done
  return 1
}
# Unquoted on purpose: --chain is a space-separated list of tool:model:effort
# specs, and an empty TOOL or CHAIN contributes no word at all.
for candidate in ${TOOL:-} ${CHAIN:-}; do
  known_adapter "${candidate%%:*}" || {
    echo "unimplemented adapter: ${candidate%%:*} (known: $DISPATCH_IMPLEMENTED_ADAPTERS)" >&2
    exit 2
  }
done

# Access mode is the only writable route. It stays off by default, is refused for
# assurance work, and demands a Git worktree root the caller has already given the
# worker exclusively; the dispatch owner holds the one-writer lease over that path.
case "$ACCESS_MODE" in
  read_only)
    if [ -n "$WORKTREE" ]; then
      echo "--worktree requires --access-mode worktree_write" >&2; exit 2
    fi
    ;;
  worktree_write)
    if [ -z "$WORKTREE" ]; then
      echo "--access-mode worktree_write requires --worktree" >&2; exit 2
    fi
    if [ "$INTENT" != "ordinary" ]; then
      echo "--access-mode worktree_write requires --intent ordinary" >&2; exit 2
    fi
    case "$TOOL" in
      claude|codex|opencode|cursor|agy|kiro|copilot) ;;
      *) echo "--access-mode worktree_write is unsupported for adapter: ${TOOL:-<chain>}" >&2; exit 2;;
    esac
    if ! command -v git >/dev/null 2>&1; then
      echo "--access-mode worktree_write requires git" >&2; exit 2
    fi
    if ! WORKTREE="$(CDPATH= cd -- "$WORKTREE" 2>/dev/null && pwd -P)"; then
      echo "--worktree is not a readable directory" >&2; exit 2
    fi
    worktree_top="$(git -C "$WORKTREE" rev-parse --path-format=absolute --show-toplevel 2>/dev/null)" || worktree_top=""
    if [ -z "$worktree_top" ] || [ "$(CDPATH= cd -- "$worktree_top" 2>/dev/null && pwd -P)" != "$WORKTREE" ]; then
      echo "--worktree must be the root of a Git worktree: $WORKTREE" >&2; exit 2
    fi
    WORKTREE_GIT_COMMON="$(git -C "$WORKTREE" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" || WORKTREE_GIT_COMMON=""
    ;;
  *) echo "invalid access mode: $ACCESS_MODE" >&2; exit 2;;
esac

if [ "$ALIAS_EXPLICIT" -eq 0 ] && [ -z "$TASK_CLASS" ]; then
  if [ -n "$MODEL" ] || [ -n "$MODEL_OVERRIDE_TIER" ]; then
    # An explicitly named model has already made the cost decision, so leave the
    # alias at flagship rather than narrowing the candidate list under it.
    MODEL_ALIAS="flagship"
  else
    case "$ROUTE_ROLE" in
      lead|orchestrator|critical-review) MODEL_ALIAS="flagship";;
      *) MODEL_ALIAS="workhorse";;
    esac
  fi
fi

append_cli_paths() {
  local dir home_dir
  home_dir="${HOME:-}"
  for dir in /opt/homebrew/bin /usr/local/bin ${home_dir:+"$home_dir/.local/bin"} ${home_dir:+"$home_dir/bin"}; do
    [ -d "$dir" ] || continue
    case ":$PATH:" in
      *":$dir:"*) ;;
      *) PATH="$PATH:$dir";;
    esac
  done
  export PATH
}
append_cli_paths

if [ -n "${CF_DISPATCH_AGY_ADD_DIR:-}" ]; then
  while IFS= read -r agy_dir; do
    [ -n "$agy_dir" ] && AGY_ADD_DIRS+=("$agy_dir")
  done < <(printf '%s\n' "$CF_DISPATCH_AGY_ADD_DIR" | tr ':' '\n')
fi

show_doctor() {
  local tool cmd
  printf 'cf_dispatch doctor\n'
  printf 'pwd=%s\n' "$(pwd)"
  printf 'PATH=%s\n' "$PATH"
  if git_root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
    printf 'git_root=%s\n' "$git_root"
    printf 'git_status_short_count=%s\n' "$(git status --short 2>/dev/null | wc -l | tr -d ' ')"
  else
    printf 'git_root=NONE\n'
  fi
  printf 'CF_DISPATCH_ENABLE_KIRO=%s\n' "${CF_DISPATCH_ENABLE_KIRO:-0}"
  printf 'CF_DISPATCH_ENABLE_COPILOT=%s\n' "${CF_DISPATCH_ENABLE_COPILOT:-0}"
  printf 'CF_DISPATCH_AGY_ADD_DIR=%s\n' "${CF_DISPATCH_AGY_ADD_DIR:-}"
  for tool in claude codex cursor-agent agy kiro-cli copilot opencode; do
    if cmd="$(command -v "$tool" 2>/dev/null)"; then
      printf '%s=%s\n' "$tool" "$cmd"
      case "$tool" in
        claude|codex|agy|opencode) "$cmd" --version 2>/dev/null | sed "s/^/${tool}_version=/" | head -n 1;;
      esac
    else
      printf '%s=NOT_FOUND\n' "$tool"
    fi
  done
}

if [ "$DOCTOR" = "1" ]; then
  show_doctor
  exit 0
fi

WORKTREE_POLICY="$SCRIPT_DIR/../../../scripts/worktree.py"
[ -f "$WORKTREE_POLICY" ] || {
  echo "worktree context validator is unavailable: $WORKTREE_POLICY" >&2
  exit 2
}
python3 "$WORKTREE_POLICY" validate-context --repo "$(pwd -P)" --allow-non-git \
  >/dev/null || exit 2

if [ -n "$PROMPT_FILE" ]; then
  [ -r "$PROMPT_FILE" ] || { echo "cannot read prompt file: $PROMPT_FILE" >&2; exit 2; }
elif [ -z "$PROMPT" ]; then
  echo "need --prompt or --prompt-file" >&2
  exit 2
fi
make_tmp() {
  local root="${TMPDIR:-/tmp}"
  [ -d "$root" ] || { echo "temporary directory does not exist: $root" >&2; return 1; }
  mktemp "$root/cf-dispatch.XXXXXX"
}
make_tmp_dir() {
  local root="${TMPDIR:-/tmp}"
  [ -d "$root" ] || { echo "temporary directory does not exist: $root" >&2; return 1; }
  mktemp -d "$root/cf-dispatch-run.XXXXXX"
}
if [ -z "$OUT" ]; then
  OUT="$(make_tmp)"
  OUT_CREATED=true
fi
PROMPT_TMP="$(make_tmp)"
if [ -n "$PROMPT_FILE" ]; then
  if ! cp -- "$PROMPT_FILE" "$PROMPT_TMP"; then
    echo "cannot retain prompt file: $PROMPT_FILE" >&2
    [ "$OUT_CREATED" = true ] && rm -f "$OUT"
    exit 2
  fi
else
  printf '%s' "$PROMPT" >"$PROMPT_TMP"
fi
cleanup_dispatch() {
  rm -f "$PROMPT_TMP"
  [ -n "$ACTIVE_RUN_TMPDIR" ] && rm -rf -- "$ACTIVE_RUN_TMPDIR"
}
abort_dispatch() {
  cleanup_dispatch
  [ -n "$OUT" ] && rm -f -- "$OUT.raw.jsonl"
  [ "$OUT_CREATED" = true ] && rm -f "$OUT"
  exit 143
}
trap cleanup_dispatch EXIT
trap abort_dispatch INT TERM HUP
[ -s "$PROMPT_TMP" ] || {
  echo "need --prompt or --prompt-file" >&2
  [ "$OUT_CREATED" = true ] && rm -f "$OUT"
  exit 2
}
if ! python3 - "$PROMPT_TMP" <<'PY'
import sys
from pathlib import Path

raise SystemExit(1 if b"\0" in Path(sys.argv[1]).read_bytes() else 0)
PY
then
  echo "prompt contains unsupported NUL bytes" >&2
  [ "$OUT_CREATED" = true ] && rm -f "$OUT"
  exit 2
fi
json_escape() {
  python3 -c 'import json,sys; print(json.dumps(sys.stdin.read())[1:-1])'
}
normalise_family() {
  case "$1" in
    claude) echo "anthropic";;
    codex) echo "openai";;
    *) echo "$1";;
  esac
}
valid_family() {
  case "$1" in
    anthropic|openai) return 0;;
    *) return 1;;
  esac
}
# Upstream families that may appear on a receipt but must not set cross_family /
# certification_eligible (broker collapse / open-weight bucket).
assurance_family() {
  case "$1" in
    ""|generic-open|open-weight) return 1;;
    *) return 0;;
  esac
}
resolve_model() {
  local tool="$1" model="$2"
  if [ -n "$model" ]; then
    echo "$model"
    return
  fi
  case "$tool" in
    cursor) echo "${CF_DISPATCH_CURSOR_MODEL:-}";;
    kiro) echo "${CF_DISPATCH_KIRO_MODEL:-}";;
    copilot) echo "${CF_DISPATCH_COPILOT_MODEL:-}";;
    opencode) echo "${CF_DISPATCH_OPENCODE_MODEL:-}";;
    *) echo "";;
  esac
}
endpoint_provider() {
  # Mirror of the routing catalogue's per-adapter endpoint_provider, drift-bound
  # by tests/test_adapter_identity_maps.py.
  case "$1" in
    claude) echo "anthropic";;
    codex) echo "openai";;
    cursor) echo "cursor";;
    agy) echo "agy";;
    kiro) echo "aws";;
    copilot) echo "github";;
    opencode) echo "opencode";;
    *) echo "";;
  esac
}
install_output() {
  local source="$1" destination="$2" installed_identity extra
  INSTALLED_OUTPUT_DIGEST=""
  INSTALLED_OUTPUT_DEVICE=""
  INSTALLED_OUTPUT_INODE=""
  installed_identity="$("$SCRIPT_DIR/output_custody.py" install \
    --source "$source" --destination "$destination")" || return 1
  read -r INSTALLED_OUTPUT_DIGEST INSTALLED_OUTPUT_DEVICE INSTALLED_OUTPUT_INODE extra \
    <<<"$installed_identity"
  [[ "$INSTALLED_OUTPUT_DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] \
    && [[ "$INSTALLED_OUTPUT_DEVICE" =~ ^[0-9]+$ ]] \
    && [[ "$INSTALLED_OUTPUT_INODE" =~ ^[0-9]+$ ]] \
    && [ -z "${extra:-}" ]
}
verify_installed_output() {
  local destination="$1"
  [ -n "$INSTALLED_OUTPUT_DIGEST" ] \
    && "$SCRIPT_DIR/output_custody.py" verify \
      --destination "$destination" \
      --digest "$INSTALLED_OUTPUT_DIGEST" \
      --device "$INSTALLED_OUTPUT_DEVICE" \
      --inode "$INSTALLED_OUTPUT_INODE"
}
emit_record() {
  local tool="$1" model="$2" effort="$3" status="$4" rc="$5" path="$6" guarantee="$7"
  local family="${8:-}" endpoint_provider="${9:-}" identity="${10:-}" effort_substitution="${11:-}"
  local requested_effort="${12:-}" effort_source="${13:-}" effort_capability_source="${14:-}" cross cert
  local substitution="${15:-}" requested_model="${16:-$model}" fallback_model="${17:-}"
  local catalog_model="${18:-}" model_selection="${19:-}"
  local risk_tier="${20:-$RISK_TIER}" policy_override="${21:-}"
  local model_override_tier="${22:-$MODEL_OVERRIDE_TIER}"
  local reason="${23:-}"
  local receipt_alias="$MODEL_ALIAS"
  if [ -n "$model" ] && [ "$ALIAS_EXPLICIT" -eq 0 ]; then receipt_alias=""; fi
  local output_digest=""
  model="$(resolve_model "$tool" "$model")"
  [ -n "$endpoint_provider" ] || endpoint_provider="$(endpoint_provider "$tool")"
  [ -n "$identity" ] || identity="unresolved"
  if [ -n "$path" ]; then
    if verify_installed_output "$path"; then
      output_digest="$INSTALLED_OUTPUT_DIGEST"
    else
      path=""
      guarantee="none"
    fi
  fi
  if [ "$status" = "ok" ] && [ -z "$output_digest" ]; then
    status="output_identity_invalid"
    rc=1
    path=""
    guarantee="none"
  fi
  cross="false"
  [ -n "$ORCH_FAMILY" ] && valid_family "$ORCH_FAMILY" && assurance_family "$family" && [ -n "$family" ] && [ "$ORCH_FAMILY" != "$family" ] && cross="true"
  cert="false"
  [ "$INTENT" = "assurance" ] && [ "$status" = "ok" ] && [ -n "$output_digest" ] && [ "$cross" = "true" ] && { [ "$guarantee" = "enforced" ] || [ "$guarantee" = "oauth_safe_mode" ]; } && cert="true"
  printf '{"tool":"%s","adapter":"%s","adapter_gate":"direct-cli","execution_intent":"%s","model":"%s","requested_model":"%s","resolved_model":"%s","fallback_model":"%s","requested_effort":"%s","effort":"%s","effort_source":"%s","effort_capability_source":"%s","effort_substitution":"%s","substitution":"%s","status":"%s","reason":"%s","exit":%s,"output_path":"%s","output_digest":"%s","read_only_guarantee":"%s","provider_sandbox":%s,"provider_network":%s,"access_mode":"%s","worktree":"%s","orchestrator_family":"%s","provider_family":"%s","model_family":"%s","endpoint_provider":"%s","identity_source":"%s","catalog_model":"%s","model_selection":"%s","route_alias":"%s","reviewer_id":"%s","risk_tier":"%s","model_override_tier":"%s","policy_override":"%s","cross_family":%s,"certification_eligible":%s}\n' \
    "$(printf '%s' "$tool" | json_escape)" \
    "$(printf '%s' "$tool" | json_escape)" \
    "$(printf '%s' "$INTENT" | json_escape)" \
    "$(printf '%s' "$model" | json_escape)" \
    "$(printf '%s' "$requested_model" | json_escape)" \
    "$(printf '%s' "$model" | json_escape)" \
    "$(printf '%s' "$fallback_model" | json_escape)" \
    "$(printf '%s' "$requested_effort" | json_escape)" \
    "$(printf '%s' "$effort" | json_escape)" \
    "$(printf '%s' "$effort_source" | json_escape)" \
    "$(printf '%s' "$effort_capability_source" | json_escape)" \
    "$(printf '%s' "$effort_substitution" | json_escape)" \
    "$(printf '%s' "$substitution" | json_escape)" \
    "$(printf '%s' "$status" | json_escape)" \
    "$(printf '%s' "$reason" | json_escape)" \
    "$rc" \
    "$(printf '%s' "$path" | json_escape)" \
    "$(printf '%s' "$output_digest" | json_escape)" \
    "$(printf '%s' "$guarantee" | json_escape)" \
    "$AGY_SANDBOX_JSON" \
    "$PROVIDER_NETWORK_JSON" \
    "$(printf '%s' "$ACCESS_MODE" | json_escape)" \
    "$(printf '%s' "$WORKTREE" | json_escape)" \
    "$(printf '%s' "$ORCH_FAMILY" | json_escape)" \
    "$(printf '%s' "$family" | json_escape)" \
    "$(printf '%s' "$family" | json_escape)" \
    "$(printf '%s' "$endpoint_provider" | json_escape)" \
    "$(printf '%s' "$identity" | json_escape)" \
    "$(printf '%s' "$catalog_model" | json_escape)" \
    "$(printf '%s' "$model_selection" | json_escape)" \
    "$(printf '%s' "$receipt_alias" | json_escape)" \
    "$(printf '%s' "$REVIEWER_ID" | json_escape)" \
    "$(printf '%s' "$risk_tier" | json_escape)" \
    "$(printf '%s' "$model_override_tier" | json_escape)" \
    "$(printf '%s' "$policy_override" | json_escape)" \
    "$cross" \
    "$cert"
  [ "$status" = "ok" ]
}

ORCH_FAMILY="$(normalise_family "$ORCH_FAMILY")"

# The single product-root derivation in this script (#754). It mirrors the
# precedence in scripts/lib/roots.py, which a shell script cannot import: an
# explicit AGENT_FABRIC_PRODUCT_ROOT wins, because a caller who set it knows
# better than any derivation; then the repository this script physically lives
# in, so a linked worktree tests its own config; then the checkout layout above
# skills/<skill>/scripts.
resolve_product_root() {
  local derived
  if [ -n "${AGENT_FABRIC_PRODUCT_ROOT:-}" ]; then
    printf '%s\n' "$AGENT_FABRIC_PRODUCT_ROOT"
    return 0
  fi
  derived="$(cd "$SCRIPT_DIR" && git rev-parse --show-toplevel 2>/dev/null || true)"
  if [ -n "$derived" ]; then
    printf '%s\n' "$derived"
    return 0
  fi
  if [ -d "$SCRIPT_DIR/../../.." ]; then
    printf '%s\n' "$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)"
    return 0
  fi
  return 1
}

require_cmd() {
  local cmd="$1" diag="$2"
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "$cmd not found. PATH=$PATH" >"$diag"
    return 1
  fi
}

resolve_routing() {
  # Resolve model routing via provenant if available, else via scripts/model_route.py from product root.
  # Returns JSON. If neither method is available, returns status="model_routing_unavailable".
  local tool="$1" alias="$2" role="$3" lead_family="$4" diag_file="$5"
  local model="$6" effort="$7" model_override_tier="$8" capabilities_file="$9"
  local task_class="${10:-}"
  local product_root=""
  local -a cmd route_args

  # The installed `provenant` resolves config from wherever it was installed from,
  # which is not this checkout when the dispatcher runs inside a linked worktree.
  # Pin it to the tree this script actually lives in, so a worktree's config edits
  # are the ones under test.
  product_root="$(resolve_product_root || true)"
  local instance_root="${AGENT_FABRIC_INSTANCE_ROOT:-${HOME}/.agents}"
  # Catalogue fallback belongs to the router invocation, never the provider seat.
  if [ ! -e "$instance_root/config/model-routing.json" ] && [ ! -L "$instance_root/config/model-routing.json" ]; then
    instance_root="$product_root"
  fi

  route_args=(--adapter "$tool" --role "$role" --lead-family "$lead_family")
  if [ -n "$task_class" ]; then
    route_args+=(--task-class "$task_class")
  else
    route_args+=(--alias "$alias")
  fi
  # A defaulted alias beside a named model is ours, not the caller's; an env
  # hint (not a flag) keeps an older installed router working.
  local alias_implied=0
  [ "${ALIAS_EXPLICIT:-0}" -eq 0 ] && alias_implied=1
  [ "$INTENT" = "assurance" ] && route_args+=(--require-distinct)
  [ -n "$FALLBACK" ] && route_args+=(--fallback "$FALLBACK")
  [ -n "$model" ] && route_args+=(--model "$model")
  [ -n "$effort" ] && route_args+=(--effort "$effort")
  [ -n "$model_override_tier" ] && route_args+=(--model-override-tier "$model_override_tier")
  [ -n "$capabilities_file" ] && [ -f "$capabilities_file" ] && route_args+=(--capabilities-file "$capabilities_file")
  # Endpoint profiles are named in the routing catalogue, never configured here:
  # the caller names one, and the router decides whether it is usable.
  [ -n "${CF_DISPATCH_ENDPOINT:-}" ] && route_args+=(--endpoint "$CF_DISPATCH_ENDPOINT")

  # Try provenant first if available
  if command -v provenant >/dev/null 2>&1; then
    cmd=(provenant route resolve "${route_args[@]}")
    if [ -n "$product_root" ]; then
      FABRIC_ALIAS_IMPLIED="$alias_implied" AGENT_FABRIC_PRODUCT_ROOT="$product_root" AGENT_FABRIC_INSTANCE_ROOT="$instance_root" "${cmd[@]}" 2>>"$diag_file"
    else
      FABRIC_ALIAS_IMPLIED="$alias_implied" "${cmd[@]}" 2>>"$diag_file"
    fi
    return $?
  fi

  # Fall back to scripts/model_route.py under the one resolved product root.
  if [ -n "$product_root" ] && [ -f "$product_root/scripts/model_route.py" ]; then
    cmd=(python3 "$product_root/scripts/model_route.py" "resolve" "${route_args[@]}")
    FABRIC_ALIAS_IMPLIED="$alias_implied" AGENT_FABRIC_PRODUCT_ROOT="$product_root" AGENT_FABRIC_INSTANCE_ROOT="$instance_root" "${cmd[@]}" 2>>"$diag_file"
    return $?
  fi

  # Unable to find routing capability; return typed status
  printf '{"status":"model_routing_unavailable","reason":"neither provenant nor scripts/model_route.py found"}\n'
  return 127
}

parse_route_json() {
  local route_json="$1" route_dir="$2" route_path fields_path key value
  route_path="$route_dir/route.json"
  fields_path="$route_dir/route-fields"
  printf '%s' "$route_json" >"$route_path"
  if ! python3 - "$route_path" "$fields_path" <<'PY'
import json
import sys
from pathlib import Path

route_path, fields_path = map(Path, sys.argv[1:3])

def reject_duplicate_members(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate route member: {key}")
        value[key] = item
    return value

route = json.loads(
    route_path.read_text(encoding="utf-8"),
    object_pairs_hook=reject_duplicate_members,
)
if not isinstance(route, dict):
    raise ValueError("route must be a JSON object")
if route.get("status") == "ok":
    for key in (
        "resolved_model", "model_family", "endpoint_provider", "identity_source",
    ):
        value = route.get(key)
        if not isinstance(value, str) or not value.strip():
            raise ValueError(f"successful route requires non-empty {key}")
keys = (
    "status", "resolved_model", "model_family", "endpoint_provider",
    "identity_source", "requested_effort", "effort", "effort_source",
    "effort_capability_source", "effort_substitution", "substitution",
    "fallback_model", "catalog_model", "model_selection",
    "model_override_tier", "policy_override", "alias", "reason",
    "endpoint_profile", "endpoint_base_url", "endpoint_token_env",
    "endpoint_wire_api",
)
with fields_path.open("wb") as handle:
    for key in keys:
        value = route.get(key, "")
        if value is None:
            value = ""
        if not isinstance(value, str) or "\0" in value:
            raise ValueError(f"route field {key} must be a NUL-free string")
        handle.write(key.encode("ascii") + b"\0")
        handle.write(value.encode("utf-8") + b"\0")
PY
  then
    return 1
  fi
  while IFS= read -r -d '' key && IFS= read -r -d '' value; do
    case "$key" in
      status) status="$value";;
      resolved_model) model="$value";;
      model_family) family="$value";;
      endpoint_provider) endpoint="$value";;
      identity_source) identity="$value";;
      requested_effort) requested_effort="$value";;
      effort) effort="$value";;
      effort_source) effort_source="$value";;
      effort_capability_source) effort_capability_source="$value";;
      effort_substitution) effort_substitution="$value";;
      substitution) substitution="$value";;
      fallback_model) fallback_model="$value";;
      catalog_model) catalog_model="$value";;
      model_selection) model_selection="$value";;
      model_override_tier) route_model_override_tier="$value";;
      policy_override) policy_override="$value";;
      alias) route_alias="$value";;
      reason) route_reason="$value";;
      endpoint_profile) endpoint_profile="$value";;
      endpoint_base_url) endpoint_base_url="$value";;
      endpoint_token_env) endpoint_token_env="$value";;
      endpoint_wire_api) endpoint_wire_api="$value";;
      *) return 1;;
    esac
  done <"$fields_path"
  [ -n "$status" ]
}

agy_has_unsafe_arg() {
  case "$1 $2 ${CF_DISPATCH_AGY_ADD_DIR:-}" in
    *--dangerously-skip-permissions*) return 0;;
    *) return 1;;
  esac
}

run_one() {  # $1 tool $2 model $3 effort $4 private tempdir -> JSON, returns 0/1
  local tool="$1" model="$2" effort="$3" route_effort_input="$3" tmpdir="$4" raw diag combined clean rc status opath guarantee family endpoint identity effort_substitution substitution requested_model requested_effort effort_source effort_capability_source route_json route_rc capabilities_file fallback_model primary_model catalog_model model_selection policy_override route_risk_tier route_model_override_tier route_alias route_reason endpoint_profile endpoint_base_url endpoint_token_env endpoint_wire_api agy_status agy_dir agy_prompt_bytes
  local model_pin="$2"
  model="$(resolve_model "$tool" "$model")"
  raw="$tmpdir/raw"
  diag="$tmpdir/diag"
  clean="$tmpdir/clean"
  combined="$tmpdir/combined"
  : >"$raw"
  : >"$diag"
  : >"$clean"
  if [ -n "$CHAIN" ]; then
    trap "rm -rf -- '$tmpdir'" EXIT
    trap "rm -rf -- '$tmpdir'; exit 143" INT TERM HUP
  fi
  family=""
  endpoint=""
  identity=""
  effort_substitution=""
  substitution=""
  catalog_model=""
  model_selection=""
  policy_override=""
  route_risk_tier="$RISK_TIER"
  route_model_override_tier="$MODEL_OVERRIDE_TIER"
  route_reason=""
  endpoint_profile=""
  endpoint_base_url=""
  endpoint_token_env=""
  endpoint_wire_api=""
  requested_effort="$effort"
  effort_source=""
  effort_capability_source=""
  fallback_model=""
  requested_model="$model"
  capabilities_file=""
  primary_model=""
  if [ "$tool" = "agy" ] && agy_has_unsafe_arg "$model" "$effort"; then
    guarantee="none"
    status="unsafe_by_default"
    echo "agy refused: --dangerously-skip-permissions is not allowed on the read-only route" >"$diag"
    rc=1
  elif [ "$tool" = "codex" ] && [ "${CF_DISPATCH_CODEX_NETWORK-1}" != "0" ] && [ "${CF_DISPATCH_CODEX_NETWORK-1}" != "1" ]; then
    guarantee="none"
    status="invalid_configuration"
    echo "CF_DISPATCH_CODEX_NETWORK must be 0 or 1" >"$diag"
    rc=1
  elif [ "$tool" = "agy" ] && [ "${CF_DISPATCH_AGY_SANDBOX-0}" != "0" ] && [ "${CF_DISPATCH_AGY_SANDBOX-0}" != "1" ]; then
    guarantee="none"
    status="invalid_configuration"
    echo "CF_DISPATCH_AGY_SANDBOX must be 0 or 1" >"$diag"
    rc=1
  elif [ -n "$ORCH_FAMILY" ] && ! valid_family "$ORCH_FAMILY"; then
    guarantee="none"
    status="invalid_orchestrator_family"
    echo "invalid orchestrator family: $ORCH_FAMILY" >"$diag"
    rc=1
  elif [ "$INTENT" = "assurance" ] && [ -z "$ORCH_FAMILY" ]; then
    guarantee="none"
    status="orchestrator_family_required"
    echo "$tool disabled: pass --orchestrator-family so cross-family status can be proven" >"$diag"
    rc=1
  else
    # Resolve configuration before any provider-backed capability probe. This
    # makes disabled or malformed adapter policy a non-executing rejection.
    route_json="$(resolve_routing "$tool" "$MODEL_ALIAS" "$ROUTE_ROLE" "$ORCH_FAMILY" "$diag" "$requested_model" "$route_effort_input" "$MODEL_OVERRIDE_TIER" "" "$TASK_CLASS")"
    route_rc=$?
    if parse_route_json "$route_json" "$tmpdir" 2>>"$diag"; then
      case "$tool:$status" in
        codex:capability_discovery_failed)
          capabilities_file="$tmpdir/codex-capabilities.json"
          if "$SCRIPT_DIR/capabilities.py" codex \
            --out "$capabilities_file" >>"$diag" 2>&1; then
            route_json="$(resolve_routing "$tool" "$MODEL_ALIAS" "$ROUTE_ROLE" "$ORCH_FAMILY" "$diag" "$requested_model" "$route_effort_input" "$MODEL_OVERRIDE_TIER" "$capabilities_file" "$TASK_CLASS")"
            route_rc=$?
            if ! parse_route_json "$route_json" "$tmpdir" 2>>"$diag"; then
              status="routing_record_invalid"
              route_rc=1
            fi
          else
            rm -f "$capabilities_file"
          fi
          ;;
        agy:ok|agy:model_required_for_broker)
          capabilities_file="$tmpdir/agy-capabilities.json"
          if "$SCRIPT_DIR/capabilities.py" agy \
            --out "$capabilities_file" >>"$diag" 2>&1; then
            route_json="$(resolve_routing "$tool" "$MODEL_ALIAS" "$ROUTE_ROLE" "$ORCH_FAMILY" "$diag" "$requested_model" "$route_effort_input" "$MODEL_OVERRIDE_TIER" "$capabilities_file" "$TASK_CLASS")"
            route_rc=$?
            if ! parse_route_json "$route_json" "$tmpdir" 2>>"$diag"; then
              status="routing_record_invalid"
              route_rc=1
            fi
          else
            rm -f "$capabilities_file"
          fi
          ;;
      esac
      if [ "$tool" = "claude" ] && [ -n "$TASK_CLASS" ] \
        && [ "$status" = "task_class_capability_unverified" ] \
        && [ -n "$model" ] && [ -n "$requested_effort" ]; then
        capabilities_file="$tmpdir/claude-capabilities.json"
        if "$SCRIPT_DIR/capabilities.py" claude --out "$capabilities_file" \
          --alias "$model" --effort "$requested_effort" >>"$diag" 2>&1; then
          route_json="$(resolve_routing "$tool" "$MODEL_ALIAS" "$ROUTE_ROLE" "$ORCH_FAMILY" "$diag" "$requested_model" "$route_effort_input" "$MODEL_OVERRIDE_TIER" "$capabilities_file" "$TASK_CLASS")"
          route_rc=$?
          if ! parse_route_json "$route_json" "$tmpdir" 2>>"$diag"; then
            status="routing_record_invalid"
            route_rc=1
          fi
        else
          rm -f "$capabilities_file"
        fi
      fi
      [ -n "$route_alias" ] && MODEL_ALIAS="$route_alias"
      [ -n "$requested_model" ] || requested_model="$model"
      if [ "$route_rc" -ne 0 ] || [ "$status" != "ok" ]; then
        guarantee="none"
        printf '%s\n' "$route_json" >>"$diag"
        rc=1
      else
        local provider_cli="$tool"
        [ "$tool" = cursor ] && provider_cli=cursor-agent
        [ "$tool" = kiro ] && provider_cli=kiro-cli
        if ! require_cmd "$provider_cli" "$diag"; then
          install_output "$diag" "$OUT" || true
          emit_record "$tool" "$model" "$effort" "tool_missing" 127 "$OUT" "none" "$family" "$endpoint" "$identity" "$effort_substitution" "$requested_effort" "$effort_source" "$effort_capability_source" "$substitution" "$requested_model" "$fallback_model"
          return 1
        fi
        local -a supervisor=(python3 "$SCRIPT_DIR/provider_exec.py" --route-file "$tmpdir/route.json"
          --adapter "$tool" --prompt-file "$PROMPT_TMP" --out "$OUT" --mode "$ACCESS_MODE"
          --workspace-root "$(pwd -P)"
          --intent "$INTENT" --orchestrator-family "$ORCH_FAMILY" --reviewer-id "$REVIEWER_ID"
          --risk-tier "$RISK_TIER" --model-override-tier "$MODEL_OVERRIDE_TIER"
          --requested-model "$model_pin" --requested-effort "$route_effort_input")
        [ "$PLAN_ONLY" = 1 ] && supervisor+=(--plan-only)
        [ "$PREFACE" = 0 ] && supervisor+=(--no-preface)
        [ -n "$WORKTREE" ] && supervisor+=(--worktree "$WORKTREE")
        [ -n "$PROVIDER_CWD" ] && supervisor+=(--cwd "$PROVIDER_CWD")
        [ -n "$SANDBOX" ] && supervisor+=(--sandbox "$SANDBOX")
        [ -n "$NETWORK" ] && supervisor+=(--network "$NETWORK")
        [ -n "$RESUME_SESSION" ] && supervisor+=(--resume-session "$RESUME_SESSION")
        [ -n "$TIMEOUT_SECONDS" ] && supervisor+=(--timeout-seconds "$TIMEOUT_SECONDS")
        for agy_dir in "${AGY_ADD_DIRS[@]:-}"; do
          [ -n "$agy_dir" ] && supervisor+=(--add-dir "$agy_dir")
        done
        if [ -z "$CHAIN" ]; then
          exec "${supervisor[@]}" --cleanup-dir "$tmpdir" --cleanup-prompt
        fi
        "${supervisor[@]}"
        return $?

      fi
    else
      guarantee="none"
      status="routing_record_invalid"
      echo "model routing returned no valid JSON record" >>"$diag"
      rc=1
    fi
  fi

  cat "$diag" >"$clean"
  opath=""
  if install_output "$clean" "$OUT"; then opath="$OUT"; fi
  emit_record "$tool" "$model" "$effort" "$status" "$rc" "$opath" "$guarantee" "$family" "$endpoint" "$identity" "$effort_substitution" "$requested_effort" "$effort_source" "$effort_capability_source" "$substitution" "$requested_model" "$fallback_model" "$catalog_model" "$model_selection" "$route_risk_tier" "$policy_override" "$route_model_override_tier" "$route_reason"
}

if [ -n "$CHAIN" ]; then
  for spec in $CHAIN; do
    t="${spec%%:*}"
    rest="${spec#*:}"
    m="${rest%%:*}"
    e="${rest#*:}"
    [ "$rest" = "$spec" ] && { m=""; e=""; }
    [ "$e" = "$m" ] && e=""
    ACTIVE_RUN_TMPDIR="$(make_tmp_dir)" || exit 1
    rec="$(run_one "$t" "$m" "$e" "$ACTIVE_RUN_TMPDIR")"; rc=$?
    rm -rf -- "$ACTIVE_RUN_TMPDIR"
    ACTIVE_RUN_TMPDIR=""
    echo "$rec" >&2
    if [ $rc -eq 0 ]; then echo "$rec"; exit 0; fi
    rm -f -- "$OUT.raw.jsonl"
  done
  rm -f -- "$OUT.raw.jsonl"
  [ "$OUT_CREATED" = true ] && rm -f "$OUT"
  emit_record "chain" "" "" "all_failed" 1 "" "none"
  exit 1
else
  [ -z "$TOOL" ] && { echo "need --tool or --chain" >&2; exit 2; }
  ACTIVE_RUN_TMPDIR="$(make_tmp_dir)" || exit 1
  run_one "$TOOL" "$MODEL" "$EFFORT" "$ACTIVE_RUN_TMPDIR"
  exit $?
fi
