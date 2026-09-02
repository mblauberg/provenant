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
  --tool TOOL                  One of claude, codex, cursor, agy, kiro, copilot.
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
  --add-dir PATH               Additional agy read directory; repeatable.
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
ALIAS_EXPLICIT=0
OUT_CREATED=false
ACTIVE_RUN_TMPDIR=""
INSTALLED_OUTPUT_DIGEST=""
INSTALLED_OUTPUT_DEVICE=""
INSTALLED_OUTPUT_INODE=""
AGY_ADD_DIRS=()
ACCESS_MODE="read_only"
WORKTREE=""
WORKTREE_GIT_COMMON=""
need_value() {
  [ $# -ge 2 ] || { echo "missing value for $1" >&2; exit 2; }
}
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0;;
    --doctor) DOCTOR=1; shift;;
    --tool) need_value "$@"; TOOL="$2"; shift 2;;
    --task-class) need_value "$@"; TASK_CLASS="$2"; shift 2;;
    --model) need_value "$@"; MODEL="$2"; shift 2;;
    --effort) need_value "$@"; EFFORT="$2"; shift 2;;
    --add-dir) need_value "$@"; AGY_ADD_DIRS+=("$2"); shift 2;;
    --access-mode) need_value "$@"; ACCESS_MODE="$2"; shift 2;;
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
      claude|codex) ;;
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

CLAUDE_MODE_FLAGS=(--permission-mode plan --tools "Read,Grep,Glob")
# A fixed identifier for the provider codex is told to use when a dispatch names
# an endpoint profile. The profile name is carried as the provider's display
# name instead, so a profile name containing characters that a TOML dotted path
# would have to quote cannot reshape the override.
CODEX_ENDPOINT_PROVIDER_ID="provenant_endpoint"
CLAUDE_SYSTEM_PROMPT="You are a non-interactive independent verifier. You may use only Read, Grep, and Glob to inspect the requested workspace. Fabric MCP tools are not exposed to this direct verifier invocation. Do not mutate files, use shell commands, call Task/tool/function abstractions, or launch subagents. Return only the file-backed verification result requested by the supplied prompt; the caller owns any Fabric correlation."
# A writer lane has to be able to run its own tests and commit its own work, so
# the write tools are named on the permission allow-list rather than left to the
# permission mode. `--permission-mode acceptEdits` accepts edits; `--allowedTools`
# is what pre-approves Bash in `-p` mode, where a permission prompt is a denial.
CLAUDE_WRITER_TOOLS="Bash,Edit,Write,MultiEdit,NotebookEdit,Read,Grep,Glob"
if [ "$ACCESS_MODE" = "worktree_write" ]; then
  CLAUDE_MODE_FLAGS=(--permission-mode acceptEdits --add-dir "$WORKTREE" --allowedTools "$CLAUDE_WRITER_TOOLS")
  CLAUDE_SYSTEM_PROMPT="You are a non-interactive worker running inside the Git worktree at $WORKTREE, which you own exclusively for this run. Write, run commands and commit only inside that worktree. Do not touch any other checkout, do not push, and do not create or remove worktrees or branches outside it. Fabric MCP tools are not exposed to this direct invocation, and the caller owns any Fabric correlation. Return the file-backed result requested by the supplied prompt."
fi

# The provider inherits the writer worktree as its working directory. Only the
# provider call moves; the dispatcher keeps its own cwd for output custody.
claude_provider() {
  if [ -n "$WORKTREE" ]; then
    ( cd "$WORKTREE" && CLAUDE_CODE_DISABLE_WORKFLOWS=1 claude "$@" <"$PROMPT_TMP" >"$raw" 2>"$diag" )
  else
    CLAUDE_CODE_DISABLE_WORKFLOWS=1 claude "$@" <"$PROMPT_TMP" >"$raw" 2>"$diag"
  fi
}

# When the caller does not name an alias, derive it from the role rather than
# defaulting everything to flagship. A bare dispatch is ordinary work and must not
# silently land on the most expensive model in a family; flagship is for work whose
# role says it is critical. The flagship roles are the keys of
# families.*.role_effort_defaults in config/model-routing.json; keep this list in
# step with that file.
# An explicit special-model override pins the alias too: every
# risk_tier_overrides entry in that file
# declares alias "flagship", and the resolver rejects any other alias with
# risk_tier_alias_mismatch. Keep both lists in step with the config. The rule only
# governs the case where the caller named neither an alias nor a model.
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
  for tool in claude codex cursor-agent agy kiro-cli copilot; do
    if cmd="$(command -v "$tool" 2>/dev/null)"; then
      printf '%s=%s\n' "$tool" "$cmd"
      case "$tool" in
        claude|codex|agy) "$cmd" --version 2>/dev/null | sed "s/^/${tool}_version=/" | head -n 1;;
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
PROMPT_ARG=""
IFS= read -r -d '' PROMPT_ARG <"$PROMPT_TMP" || true

strip_ansi() { sed $'s/\x1b\\[[0-9;?]*[A-Za-z]//g'; }
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
    *) echo "";;
  esac
}
endpoint_provider() {
  case "$1" in
    claude) echo "anthropic";;
    codex) echo "openai";;
    cursor) echo "cursor";;
    kiro) echo "aws";;
    copilot) echo "github";;
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
  [ -n "$ORCH_FAMILY" ] && valid_family "$ORCH_FAMILY" && [ -n "$family" ] && [ "$ORCH_FAMILY" != "$family" ] && cross="true"
  cert="false"
  [ "$INTENT" = "assurance" ] && [ "$status" = "ok" ] && [ -n "$output_digest" ] && [ "$cross" = "true" ] && { [ "$guarantee" = "enforced" ] || [ "$guarantee" = "oauth_safe_mode" ]; } && cert="true"
  printf '{"tool":"%s","adapter":"%s","adapter_gate":"direct-cli","execution_intent":"%s","model":"%s","requested_model":"%s","resolved_model":"%s","fallback_model":"%s","requested_effort":"%s","effort":"%s","effort_source":"%s","effort_capability_source":"%s","effort_substitution":"%s","substitution":"%s","status":"%s","reason":"%s","exit":%s,"output_path":"%s","output_digest":"%s","read_only_guarantee":"%s","access_mode":"%s","worktree":"%s","orchestrator_family":"%s","provider_family":"%s","model_family":"%s","endpoint_provider":"%s","identity_source":"%s","catalog_model":"%s","model_selection":"%s","route_alias":"%s","reviewer_id":"%s","risk_tier":"%s","model_override_tier":"%s","policy_override":"%s","cross_family":%s,"certification_eligible":%s}\n' \
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
    "$(printf '%s' "$ACCESS_MODE" | json_escape)" \
    "$(printf '%s' "$WORKTREE" | json_escape)" \
    "$(printf '%s' "$ORCH_FAMILY" | json_escape)" \
    "$(printf '%s' "$family" | json_escape)" \
    "$(printf '%s' "$family" | json_escape)" \
    "$(printf '%s' "$endpoint_provider" | json_escape)" \
    "$(printf '%s' "$identity" | json_escape)" \
    "$(printf '%s' "$catalog_model" | json_escape)" \
    "$(printf '%s' "$model_selection" | json_escape)" \
    "$(printf '%s' "$MODEL_ALIAS" | json_escape)" \
    "$(printf '%s' "$REVIEWER_ID" | json_escape)" \
    "$(printf '%s' "$risk_tier" | json_escape)" \
    "$(printf '%s' "$model_override_tier" | json_escape)" \
    "$(printf '%s' "$policy_override" | json_escape)" \
    "$cross" \
    "$cert"
  [ "$status" = "ok" ]
}

ORCH_FAMILY="$(normalise_family "$ORCH_FAMILY")"

# Specific failure signatures only. Do not treat any mention of "quota" as a failure.
fail_sig='(Authentication required|Please sign in|Please( run)? login|not logged in|not authenticated|Unauthorized|insufficient_quota|quota exceeded|rate limit exceeded|usage limit reached)'
model_fail_sig='(model[^[:cntrl:]]*(unavailable|not available|not found|unsupported|does not exist)|unknown model|capacity|overloaded)'
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
  # are the ones under test. A caller who has already set the variable knows better
  # than this derivation, so never override an explicit value.
  if [ -n "${AGENT_FABRIC_PRODUCT_ROOT:-}" ]; then
    product_root="$AGENT_FABRIC_PRODUCT_ROOT"
  else
    product_root="$(cd "$SCRIPT_DIR" && git rev-parse --show-toplevel 2>/dev/null || true)"
  fi

  route_args=(--adapter "$tool" --role "$role" --lead-family "$lead_family")
  if [ -n "$task_class" ]; then
    route_args+=(--task-class "$task_class")
  else
    route_args+=(--alias "$alias")
  fi
  [ "$INTENT" = "assurance" ] && route_args+=(--require-distinct)
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
      AGENT_FABRIC_PRODUCT_ROOT="$product_root" "${cmd[@]}" 2>>"$diag_file"
    else
      "${cmd[@]}" 2>>"$diag_file"
    fi
    return $?
  fi

  # Fall back to scripts/model_route.py from product root
  # Locate product root via git if possible, else try relative to this script
  if [ -n "$product_root" ]; then
    if [ -f "$product_root/scripts/model_route.py" ]; then
      cmd=(python3 "$product_root/scripts/model_route.py" "resolve" "${route_args[@]}")
      AGENT_FABRIC_PRODUCT_ROOT="$product_root" "${cmd[@]}" 2>>"$diag_file"
      return $?
    fi
  fi

  # Try relative path from script directory (should resolve to product root)
  if [ -f "$SCRIPT_DIR/../../../scripts/model_route.py" ]; then
    product_root="$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)"
    cmd=(python3 "$product_root/scripts/model_route.py" "resolve" "${route_args[@]}")
    AGENT_FABRIC_PRODUCT_ROOT="$product_root" "${cmd[@]}" 2>>"$diag_file"
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
  model="$(resolve_model "$tool" "$model")"
  raw="$tmpdir/raw"
  diag="$tmpdir/diag"
  clean="$tmpdir/clean"
  combined="$tmpdir/combined"
  : >"$raw"
  : >"$diag"
  : >"$clean"
  trap "rm -rf -- '$tmpdir'" EXIT
  trap "rm -rf -- '$tmpdir'; exit 143" INT TERM HUP
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
        status=""
        case "$tool" in
        claude)
          guarantee="enforced"
          [ "$ACCESS_MODE" = "worktree_write" ] && guarantee="none"
          # `run_one` is always invoked in a command-substitution subshell, so the
          # endpoint credentials below reach the `claude` child and die with it.
          # The token is read from the named variable here and passed in the
          # environment, never on a command line and never into a record. Every
          # `claude_provider` call in both access modes inherits them.
          if [ -n "$endpoint_base_url" ] && [ -n "$endpoint_token_env" ]; then
            export ANTHROPIC_BASE_URL="$endpoint_base_url"
            ANTHROPIC_AUTH_TOKEN="$(printenv "$endpoint_token_env" || true)"
            export ANTHROPIC_AUTH_TOKEN
          fi
          if ! require_cmd claude "$diag"; then
            status="tool_not_found"
            rc=127
          else
            claude_provider -p --bare --disable-slash-commands \
              --no-session-persistence "${CLAUDE_MODE_FLAGS[@]}" \
              --system-prompt "$CLAUDE_SYSTEM_PROMPT" \
              ${model:+--model "$model"} ${effort:+--effort "$effort"}; rc=$?
          fi
          if [ "${status:-}" != "tool_not_found" ] && [ "$rc" -ne 0 ] && [ -n "$fallback_model" ] && cat "$raw" "$diag" | grep -Eqi "$model_fail_sig"; then
            primary_model="$model"
            : >"$raw"
            : >"$diag"
            model="$fallback_model"
            identity="runtime-provider-fallback"
            substitution="${substitution:+$substitution; }$primary_model unavailable; used $fallback_model"
            claude_provider -p --bare --disable-slash-commands \
              --no-session-persistence "${CLAUDE_MODE_FLAGS[@]}" \
              --system-prompt "$CLAUDE_SYSTEM_PROMPT" \
              --model "$model" ${effort:+--effort "$effort"}; rc=$?
          fi
          if [ "${status:-}" != "tool_not_found" ] && [ "$rc" -ne 0 ] && cat "$raw" "$diag" | grep -Eqi "$fail_sig"; then
            if CLAUDE_CODE_DISABLE_WORKFLOWS=1 claude auth status 2>/dev/null | grep -Eq '"loggedIn"[[:space:]]*:[[:space:]]*true'; then
              : >"$raw"
              : >"$diag"
              guarantee="oauth_safe_mode"
              [ "$ACCESS_MODE" = "worktree_write" ] && guarantee="none"
              claude_provider -p --safe-mode --no-session-persistence \
                --disable-slash-commands "${CLAUDE_MODE_FLAGS[@]}" \
                --system-prompt "$CLAUDE_SYSTEM_PROMPT" \
                ${model:+--model "$model"} ${effort:+--effort "$effort"}; rc=$?
            fi
          fi
          if [ "${status:-}" != "tool_not_found" ] && [ "$rc" -ne 0 ] && [ -n "$fallback_model" ] && [ "$model" = "$requested_model" ] && cat "$raw" "$diag" | grep -Eqi "$model_fail_sig"; then
            primary_model="$model"
            : >"$raw"
            : >"$diag"
            model="$fallback_model"
            identity="runtime-provider-fallback"
            substitution="${substitution:+$substitution; }$primary_model unavailable; used $fallback_model"
            if [ "$guarantee" = "oauth_safe_mode" ]; then
              claude_provider -p --safe-mode --no-session-persistence \
                --disable-slash-commands "${CLAUDE_MODE_FLAGS[@]}" --system-prompt "$CLAUDE_SYSTEM_PROMPT" \
                --model "$model" ${effort:+--effort "$effort"}; rc=$?
            else
              claude_provider -p --bare --disable-slash-commands \
                --no-session-persistence "${CLAUDE_MODE_FLAGS[@]}" --system-prompt "$CLAUDE_SYSTEM_PROMPT" \
                --model "$model" ${effort:+--effort "$effort"}; rc=$?
            fi
          fi ;;
        codex)
          guarantee="enforced"
          # `--ignore-user-config` stays on every codex route: a dispatched run
          # must never inherit the user's own `~/.codex/config.toml`. A named
          # endpoint profile therefore supplies its provider inline instead, as
          # `-c` overrides, which codex honours with the flag set. The token is
          # named, not passed: `env_key` tells codex which variable to read, so
          # the credential never reaches an argument vector or a record.
          local -a codex_provider_flags=()
          if [ -n "$endpoint_base_url" ] && [ -n "$endpoint_token_env" ]; then
            codex_provider_flags=(
              -c "model_providers.${CODEX_ENDPOINT_PROVIDER_ID}.name=$endpoint_profile"
              -c "model_providers.${CODEX_ENDPOINT_PROVIDER_ID}.base_url=$endpoint_base_url"
              -c "model_providers.${CODEX_ENDPOINT_PROVIDER_ID}.env_key=$endpoint_token_env"
            )
            [ -n "$endpoint_wire_api" ] && codex_provider_flags+=(
              -c "model_providers.${CODEX_ENDPOINT_PROVIDER_ID}.wire_api=$endpoint_wire_api"
            )
            codex_provider_flags+=(-c "model_provider=${CODEX_ENDPOINT_PROVIDER_ID}")
          fi
          if ! require_cmd codex "$diag"; then
            status="tool_not_found"
            rc=127
          elif [ "$ACCESS_MODE" = "worktree_write" ]; then
            # A linked worktree keeps its Git metadata outside the worktree root,
            # so the sandbox needs the common Git directory as a writable root or
            # the worker cannot commit what it just wrote.
            guarantee="none"
            codex exec -s workspace-write --cd "$WORKTREE" --ignore-user-config --ignore-rules \
              --ephemeral -c service_tier="default" \
              ${WORKTREE_GIT_COMMON:+-c sandbox_workspace_write.writable_roots="[\"$WORKTREE_GIT_COMMON\"]"} \
              ${codex_provider_flags[@]+"${codex_provider_flags[@]}"} \
              ${model:+-m "$model"} ${effort:+-c model_reasoning_effort="$effort"} \
              - <"$PROMPT_TMP" >"$raw" 2>"$diag"; rc=$?
          else
            codex exec -s read-only --ignore-user-config --ignore-rules --ephemeral -c service_tier="default" \
              ${codex_provider_flags[@]+"${codex_provider_flags[@]}"} ${model:+-m "$model"} \
              ${effort:+-c model_reasoning_effort="$effort"} \
              - <"$PROMPT_TMP" >"$raw" 2>"$diag"; rc=$?
          fi ;;
        cursor)
          guarantee="enforced"
          if ! require_cmd cursor-agent "$diag"; then
            status="tool_not_found"
            rc=127
          else
            cursor-agent -p --trust --mode ask --sandbox enabled --output-format text \
              ${model:+--model "$model"} "$PROMPT_ARG" </dev/null >"$raw" 2>"$diag"; rc=$?
          fi ;;
        agy)
          # agy --sandbox does not enforce read-only writes. On agy 1.1.10 a
          # write probe under these dispatcher flags returned SUCCESS and
          # created the file; --mode plan did the same, so only the prompt
          # discourages mutation.
          guarantee="prompt_only"
          if ! require_cmd agy "$diag"; then
            status="tool_not_found"
            rc=127
          else
            local -a agy_cmd
            agy_cmd=(agy --output-format json --disable-slash-commands --sandbox)
            if [ -n "${CF_DISPATCH_AGY_TIMEOUT:-}" ]; then
              agy_cmd+=(--print-timeout "${CF_DISPATCH_AGY_TIMEOUT}")
            else
              agy_cmd+=(--print-timeout 900s)
            fi
            [ -n "$model" ] && agy_cmd+=(--model "$model")
            [ -n "$effort" ] && agy_cmd+=(--effort "$effort")
            for agy_dir in "${AGY_ADD_DIRS[@]:-}"; do
              [ -n "$agy_dir" ] || continue
              if [ "${agy_dir#/}" = "$agy_dir" ]; then
                agy_dir="$(CDPATH= cd -- "$agy_dir" 2>/dev/null && pwd -P)" || {
                  status="error"
                  echo "agy add-dir is not a readable directory" >"$diag"
                  rc=1
                  break
                }
              fi
              case "$agy_dir" in
                *--dangerously-skip-permissions*)
                  status="unsafe_by_default"
                  echo "agy refused: --dangerously-skip-permissions is not allowed on the read-only route" >"$diag"
                  rc=1
                  break
                  ;;
              esac
              agy_cmd+=(--add-dir "$agy_dir")
            done
            if [ -z "${status:-}" ]; then
              # agy has no file-backed prompt input. `--print` requires a value:
              # with none it exits 2 on "flag needs an argument", and `--print -`
              # is worse than useless, because agy treats the dash as the literal
              # prompt, ignores stdin and answers it -- exit 0, plausible prose,
              # wrong question. So the prompt goes in as one argv value.
              #
              # That puts it under the kernel's argument limits, and the binding
              # one is per-string, not total. Linux caps a single argv element at
              # MAX_ARG_STRLEN, 32 pages = 128 KiB, and refuses the exec with
              # E2BIG; darwin has no per-string cap and allows 1 MiB in total, so
              # a prompt that works on a developer's Mac can fail on a Linux
              # runner. Take the smaller limit on both, with room for the flags.
              agy_prompt_bytes=$(wc -c <"$PROMPT_TMP")
              if [ "$agy_prompt_bytes" -gt 126976 ]; then
                status="error"
                echo "agy prompt is ${agy_prompt_bytes} bytes, over the 124 KiB single-argument ceiling; pass the material with --add-dir instead" >"$diag"
                rc=1
              else
                agy_cmd+=(--print "$PROMPT_ARG")
                "${agy_cmd[@]}" >"$raw" 2>"$diag"; rc=$?
              fi
            fi
          fi
          if [ "${status:-}" != "tool_not_found" ] && [ "${status:-}" != "unsafe_by_default" ] && [ "${status:-}" != "error" ]; then
            agy_status="$(python3 - "$raw" "$diag" "$clean" "$rc" <<'PY'
import json
import re
import sys
from pathlib import Path

raw_path, diag_path, clean_path = map(Path, sys.argv[1:4])
exit_code = int(sys.argv[4])
try:
    stdout = raw_path.read_bytes().decode("utf-8")
    stdout_valid = True
except UnicodeDecodeError:
    stdout = ""
    stdout_valid = False
stderr = diag_path.read_text(encoding="utf-8", errors="replace")
denial = re.compile(r'no output produced.*?a tool required the "[^"]+" permission', re.I | re.S)
auth = re.compile(r"unauthenticated|not logged in|sign in|quota|rate.?limit|401|403", re.I)
envelope = None

def reject_duplicate_members(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError("duplicate JSON member: %s" % key)
        value[key] = item
    return value

if denial.search(stderr):
    status = "permission_denied"
    response = ""
elif not stdout_valid:
    status = "invalid_envelope"
    response = ""
else:
    try:
        candidate = json.loads(stdout, object_pairs_hook=reject_duplicate_members)
        if isinstance(candidate, dict):
            envelope = candidate
    except (json.JSONDecodeError, ValueError):
        envelope = None
    if envelope is None:
        if auth.search(stderr) or auth.search(stdout):
            status = "auth_or_quota_error"
        elif exit_code == 0:
            status = "empty_output"
        else:
            status = "error"
        response = ""
    else:
        provider_status = envelope.get("status")
        response = envelope.get("response")
        error_value = envelope.get("error")
        error = "" if error_value is None else str(error_value).strip()
        if not isinstance(provider_status, str) or not isinstance(response, str):
            status = "invalid_envelope"
            response = ""
        elif provider_status.upper() == "SUCCESS" and error:
            status = "auth_or_quota_error" if auth.search(error) else "error"
            response = ""
        elif provider_status.upper() == "SUCCESS" and exit_code != 0:
            status = "error"
            response = ""
        elif provider_status.upper() == "SUCCESS" and response.strip():
            status = "ok"
        elif provider_status.upper() == "SUCCESS":
            status = "empty_output"
            response = ""
        else:
            if "timeout" in error.lower():
                status = "timeout"
            elif auth.search(error) or auth.search(stderr) or auth.search(stdout):
                status = "auth_or_quota_error"
            else:
                status = "error"
            response = ""

clean_path.write_text(response if status == "ok" else "", encoding="utf-8")

# agy reports its failures inside the stdout JSON envelope, not on stderr, so
# without this the diagnostic is discarded and the caller is left with an empty
# output file. The reason is worth keeping: an exhausted quota that resets in an
# hour and a broken credential both classify as auth_or_quota_error, and they
# want opposite responses. The response body itself is never written back on a
# non-ok status, so a failed run still cannot be mistaken for a review.
if status != "ok":
    notes = ["agy dispatch failed: status=%s exit=%d" % (status, exit_code)]
    detail = error if envelope else ""
    if detail:
        notes.append("provider error: %s" % detail)
    elif (
        envelope
        and isinstance(provider_status, str)
        and provider_status.upper() != "SUCCESS"
    ):
        notes.append(
            "provider error: provider returned a non-success status without an error message"
        )
    elif envelope is None and not stderr.strip() and stdout.strip():
        notes.append("unparsed agy stdout: %s" % stdout.strip()[:2000])
    with diag_path.open("a", encoding="utf-8") as handle:
        handle.write("\n".join(notes) + "\n")

print(status)
PY
            )"
            status="$agy_status"
            if [ "$status" != "ok" ] && [ "$rc" -eq 0 ]; then
              rc=1
            fi
          fi
          ;;
        kiro)
          guarantee="none"
          if [ "${CF_DISPATCH_ENABLE_KIRO:-0}" != "1" ]; then
            status="unsafe_by_default"
            echo "kiro disabled: no hard read-only mode verified in current local help" >"$diag"
            rc=1
          else
            guarantee="best_effort"
            if ! require_cmd kiro-cli "$diag"; then
              status="tool_not_found"
              rc=127
            else
              kiro-cli chat --no-interactive ${model:+--model "$model"} ${effort:+--effort "$effort"} \
                "$PROMPT_ARG" </dev/null >"$raw" 2>"$diag"; rc=$?
            fi
          fi ;;
        copilot)
          guarantee="none"
          if [ "${CF_DISPATCH_ENABLE_COPILOT:-0}" != "1" ]; then
            status="unsafe_by_default"
            echo "copilot disabled: non-interactive mode may require broad tool permissions" >"$diag"
            rc=1
          else
            guarantee="prompt_only"
            if ! require_cmd copilot "$diag"; then
              status="tool_not_found"
              rc=127
            else
              copilot -p "$PROMPT_ARG" --mode plan --silent --disable-builtin-mcps \
                --available-tools='' --disallow-temp-dir ${model:+--model "$model"} ${effort:+--effort "$effort"} \
                </dev/null >"$raw" 2>"$diag"; rc=$?
            fi
          fi ;;
        *) emit_record "$tool" "$model" "$effort" "unknown_tool" 1 "" "none" "$family" "$endpoint" "$identity" "$effort_substitution" "$requested_effort" "$effort_source" "$effort_capability_source"; rm -f "$raw" "$diag"; return 1;;
        esac
      fi
    else
      guarantee="none"
      status="routing_record_invalid"
      echo "model routing returned no valid JSON record" >>"$diag"
      rc=1
    fi
  fi

  if [ "$tool" != "agy" ]; then
    strip_ansi <"$raw" >"$clean"
  fi
  cat "$clean" "$diag" >"$combined"
  if [ -n "${status:-}" ] && [ "$rc" -ne 0 ]; then
    :
  elif [ "$rc" -eq 0 ] && ! grep -q '[^[:space:]]' "$clean"; then
    status="empty_output"
    rc=1
    guarantee="none"
  elif [ "$rc" -ne 0 ] && grep -Eqi "$fail_sig" "$combined"; then
    status="auth_or_quota_error"
  elif [ "$rc" -ne 0 ]; then
    status="error"
  else
    status="ok"
  fi
  [ "$status" = "tool_not_found" ] && guarantee="none"

  if [ "$status" = "ok" ]; then
    if install_output "$clean" "$OUT"; then
      opath="$OUT"
    else
      status="output_write_error"
      rc=1
      guarantee="none"
      opath=""
    fi
  else
    if install_output "$combined" "$OUT"; then
      opath="$OUT"
    else
      status="output_write_error"
      rc=1
      guarantee="none"
      opath=""
    fi
  fi
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
  done
  [ "$OUT_CREATED" = true ] && rm -f "$OUT"
  emit_record "chain" "" "" "all_failed" 1 "" "none"
  exit 1
else
  [ -z "$TOOL" ] && { echo "need --tool or --chain" >&2; exit 2; }
  ACTIVE_RUN_TMPDIR="$(make_tmp_dir)" || exit 1
  rec="$(run_one "$TOOL" "$MODEL" "$EFFORT" "$ACTIVE_RUN_TMPDIR")"; rc=$?
  rm -rf -- "$ACTIVE_RUN_TMPDIR"
  ACTIVE_RUN_TMPDIR=""
  echo "$rec"
  exit $rc
fi
