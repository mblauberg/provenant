#!/usr/bin/env bash
# Dispatch one prompt to a different-family CLI with conservative safety defaults.
#
# This script is a helper, not an authority. The caller still chooses an appropriate
# different-family verifier, checks data policy, and records failures in the run manifest.
# Pass --orchestrator-family when known so same-family verifier routes fail closed.
# It is an explicit degraded fallback or adapter preflight; normal answer-bearing
# external work uses Agent Fabric request/reply.
set -uo pipefail

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PUBLISH_HELPER="$SCRIPT_DIR/cf_dispatch_publish.py"
PRODUCT_ROOT="$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)"
# Keep routing, publication, and record formatting on the gate's locked interpreter.
DISPATCH_PYTHON="${HARNESS_PYTHON:-python3}"

usage() {
  cat <<'EOF'
Usage: cf_dispatch.sh --tool TOOL --orchestrator-family FAMILY --prompt TEXT [options]
       cf_dispatch.sh --chain "tool:model:effort ..." --orchestrator-family FAMILY --prompt TEXT [options]
       cf_dispatch.sh --doctor

Options:
  --tool TOOL                  One of claude, codex, cursor, kiro, copilot.
  --chain SPECS                Space-separated fallback chain.
  --orchestrator-family FAMILY Current orchestrator family; same-family routes fail closed.
  --alias ALIAS                Durable route alias: flagship, workhorse, scout (default: flagship).
  --role ROLE                  Route role (default: reviewer).
  --risk-tier TIER             Optional routine, substantial, crucial, or terminal risk tier.
  --reviewer-id ID             Stable worker/reviewer identity for receipt binding.
  --task-id ID                 Stable task identity for the dispatch receipt.
  --attempt-id ID              Stable attempt identity for the dispatch receipt.
  --receipt PATH               Dispatcher-owned JSON receipt path.
  --model MODEL                Optional model passed to adapter.
  --effort EFFORT              Optional effort passed to adapter.
  --evidence-root PATH         Lexical root containing all dispatcher evidence paths.
  --out PATH                   Human-readable answer path; defaults to mktemp.
  --terminal-artifact PATH     Dispatcher-owned JSON terminal artifact path; defaults to PATH.terminal.json.
  --prompt TEXT                Prompt text.
  --prompt-file PATH           Read prompt from file.
  --doctor                     Print local dispatch diagnostics and exit.
  -h, --help                   Show this help.

Gemini/Agy execution belongs to Agent Fabric, not this direct-CLI helper.
EOF
}

TOOL="" MODEL="" EFFORT="" EVIDENCE_ROOT="" OUT="" TERMINAL_ARTIFACT="" PROMPT="" PROMPT_FILE="" CHAIN="" ORCH_FAMILY="" MODEL_ALIAS="flagship" ROUTE_ROLE="reviewer" RISK_TIER="" REVIEWER_ID="" TASK_ID="" ATTEMPT_ID="" DISPATCH_RECEIPT="" DOCTOR=0
OUT_CREATED=false
need_value() {
  [ $# -ge 2 ] || { echo "missing value for $1" >&2; exit 2; }
}
while [ $# -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0;;
    --doctor) DOCTOR=1; shift;;
    --tool) need_value "$@"; TOOL="$2"; shift 2;;
    --model) need_value "$@"; MODEL="$2"; shift 2;;
    --effort) need_value "$@"; EFFORT="$2"; shift 2;;
    --evidence-root) need_value "$@"; EVIDENCE_ROOT="$2"; shift 2;;
    --out) need_value "$@"; OUT="$2"; shift 2;;
    --prompt) need_value "$@"; PROMPT="$2"; shift 2;;
    --prompt-file) need_value "$@"; PROMPT_FILE="$2"; shift 2;;
    --chain) need_value "$@"; CHAIN="$2"; shift 2;;
    --orchestrator-family) need_value "$@"; ORCH_FAMILY="$2"; shift 2;;
    --alias) need_value "$@"; MODEL_ALIAS="$2"; shift 2;;
    --role) need_value "$@"; ROUTE_ROLE="$2"; shift 2;;
    --risk-tier) need_value "$@"; RISK_TIER="$2"; shift 2;;
    --reviewer-id) need_value "$@"; REVIEWER_ID="$2"; shift 2;;
    --task-id) need_value "$@"; TASK_ID="$2"; shift 2;;
    --attempt-id) need_value "$@"; ATTEMPT_ID="$2"; shift 2;;
    --receipt) need_value "$@"; DISPATCH_RECEIPT="$2"; shift 2;;
    --terminal-artifact) need_value "$@"; TERMINAL_ARTIFACT="$2"; shift 2;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

append_cli_paths() {
  local dir="" home_dir=""
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

show_doctor() {
  local tool="" cmd=""
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
  for tool in claude codex cursor-agent kiro-cli copilot; do
    if cmd="$(command -v "$tool" 2>/dev/null)"; then
      printf '%s=%s\n' "$tool" "$cmd"
      case "$tool" in
        claude|codex) "$cmd" --version 2>/dev/null | sed "s/^/${tool}_version=/" | head -n 1;;
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

if [ -n "$PROMPT_FILE" ]; then
  [ -r "$PROMPT_FILE" ] || { echo "cannot read prompt file: $PROMPT_FILE" >&2; exit 2; }
  PROMPT="$(cat "$PROMPT_FILE")"
fi
[ -z "$PROMPT" ] && { echo "need --prompt or --prompt-file" >&2; exit 2; }
make_tmp() {
  local root="${TMPDIR:-/tmp}"
  [ -d "$root" ] || { echo "temporary directory does not exist: $root" >&2; return 1; }
  mktemp "$root/cf-dispatch.XXXXXX"
}
make_tmp_path() {
  local path=""
  path="$(make_tmp)" || return 1
  rm -f -- "$path"
  printf '%s\n' "$path"
}
make_tmp_dir() {
  local root="${TMPDIR:-/tmp}"
  [ -d "$root" ] || { echo "temporary directory does not exist: $root" >&2; return 1; }
  mktemp -d "$root/cf-dispatch-run.XXXXXX"
}
if [ -z "$OUT" ]; then
  OUT="$(make_tmp_path)"
  OUT_CREATED=true
fi
if [ -z "$TERMINAL_ARTIFACT" ]; then
  TERMINAL_ARTIFACT="${OUT}.terminal.json"
fi
# EVIDENCE_ROOT is caller-supplied only. Deriving it from the evidence paths
# would let the boundary certify itself, which is the defect this replaced.
PROMPT_TMP="$(make_tmp)"
printf '%s' "$PROMPT" >"$PROMPT_TMP"
trap 'rm -f "$PROMPT_TMP"' EXIT
trap 'rm -f "$PROMPT_TMP"; [ "$OUT_CREATED" = true ] && rm -f "$OUT"; exit 143' INT TERM HUP

strip_ansi() { sed $'s/\x1b\\[[0-9;?]*[A-Za-z]//g'; }
json_escape() {
  "$DISPATCH_PYTHON" -c 'import json,sys; print(json.dumps(sys.stdin.read())[1:-1])'
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
emit_record() {
  local tool="$1" model="$2" effort="$3" status="$4" rc="$5" path="$6" guarantee="$7"
  local family="${8:-}" endpoint="${9:-}" identity="${10:-}" effort_substitution="${11:-}"
  local requested_effort="${12:-}" effort_source="${13:-}" effort_capability_source="${14:-}" cross="" cert=""
  local substitution="${15:-}" requested_model="${16:-$model}" fallback_model="${17:-}"
  local catalog_model="${18:-}" model_selection="${19:-}"
  local risk_tier="${20:-$RISK_TIER}" policy_override="${21:-}" terminal_observed="${22:-false}" output_sha256="${23:-}" terminal_artifact_path="${24:-}" terminal_artifact_sha256="${25:-}" record="" receipt_tmp="" record_digest=""
  local adapter_resolution="${26:-}" adapter_executable="${27:-}" adapter_resolution_reason="${28:-}"
  local provider_assurance="${29:-}" certifying_answer_bearing_leg="${30:-false}"
  model="$(resolve_model "$tool" "$model")"
  [ -n "$endpoint" ] || endpoint="$(endpoint_provider "$tool")"
  [ -n "$identity" ] || identity="unresolved"
  cross="false"
  [ -n "$ORCH_FAMILY" ] && valid_family "$ORCH_FAMILY" && [ -n "$family" ] && [ "$ORCH_FAMILY" != "$family" ] && cross="true"
  cert="false"
  [ "${EVIDENCE_PATHS_DISTINCT:-false}" = "true" ] && [ "$status" = "ok" ] && [ "$cross" = "true" ] && [ "$adapter_resolution" = "verified-owner" ] && [ -n "$provider_assurance" ] && [ "$certifying_answer_bearing_leg" = "true" ] && { [ "$guarantee" = "enforced" ] || [ "$guarantee" = "oauth_safe_mode" ]; } && cert="true"
  record="$(printf '{"id":"%s","attempt_id":"%s","tool":"%s","adapter":"%s","adapter_gate":"direct-cli","model":"%s","requested_model":"%s","resolved_model":"%s","fallback_model":"%s","requested_effort":"%s","effort":"%s","effort_source":"%s","effort_capability_source":"%s","effort_substitution":"%s","substitution":"%s","status":"%s","exit":%s,"terminal_observed":%s,"output_path":"%s","output_sha256":"%s","terminal_artifact_path":"%s","terminal_artifact_sha256":"%s","read_only_guarantee":"%s","orchestrator_family":"%s","provider_family":"%s","model_family":"%s","endpoint_provider":"%s","identity_source":"%s","provider_assurance":"%s","catalog_model":"%s","model_selection":"%s","route_alias":"%s","reviewer_id":"%s","risk_tier":"%s","policy_override":"%s","adapter_resolution":"%s","adapter_executable":"%s","adapter_resolution_reason":"%s","cross_family":%s,"certification_eligible":%s}\n' \
    "$(printf '%s' "${TASK_ID:-$REVIEWER_ID}" | json_escape)" \
    "$(printf '%s' "${ATTEMPT_ID:-attempt-1}" | json_escape)" \
    "$(printf '%s' "$tool" | json_escape)" \
    "$(printf '%s' "$tool" | json_escape)" \
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
    "$rc" \
    "$terminal_observed" \
    "$(printf '%s' "$path" | json_escape)" \
    "$(printf '%s' "$output_sha256" | json_escape)" \
    "$(printf '%s' "$terminal_artifact_path" | json_escape)" \
    "$(printf '%s' "$terminal_artifact_sha256" | json_escape)" \
    "$(printf '%s' "$guarantee" | json_escape)" \
    "$(printf '%s' "$ORCH_FAMILY" | json_escape)" \
    "$(printf '%s' "$family" | json_escape)" \
    "$(printf '%s' "$family" | json_escape)" \
    "$(printf '%s' "$endpoint" | json_escape)" \
    "$(printf '%s' "$identity" | json_escape)" \
    "$(printf '%s' "$provider_assurance" | json_escape)" \
    "$(printf '%s' "$catalog_model" | json_escape)" \
    "$(printf '%s' "$model_selection" | json_escape)" \
    "$(printf '%s' "$MODEL_ALIAS" | json_escape)" \
    "$(printf '%s' "$REVIEWER_ID" | json_escape)" \
    "$(printf '%s' "$risk_tier" | json_escape)" \
    "$(printf '%s' "$policy_override" | json_escape)" \
    "$(printf '%s' "$adapter_resolution" | json_escape)" \
    "$(printf '%s' "$adapter_executable" | json_escape)" \
    "$(printf '%s' "$adapter_resolution_reason" | json_escape)" \
    "$cross" \
    "$cert" )"
  if [ -n "$DISPATCH_RECEIPT" ] && [ -n "$EVIDENCE_ROOT" ] && [ "$status" != "evidence_paths_not_distinct" ] && [ "$status" != "evidence_root_required" ]; then
    receipt_tmp="$(make_tmp)" || receipt_tmp=""
    if [ -z "$receipt_tmp" ] || ! printf '%s' "$record" >"$receipt_tmp" || ! "$DISPATCH_PYTHON" "$PUBLISH_HELPER" --root "$EVIDENCE_ROOT" publish "$DISPATCH_RECEIPT" "$receipt_tmp"; then
      [ -n "$receipt_tmp" ] && rm -f "$receipt_tmp"
      echo "cannot write dispatcher receipt: $DISPATCH_RECEIPT" >&2
      record="$(printf '%s' "$record" | "$DISPATCH_PYTHON" -c 'import json,sys; value=json.load(sys.stdin); value.update(status="receipt_write_error", exit=1, terminal_observed=False, read_only_guarantee="none", certification_eligible=False); print(json.dumps(value, separators=(",", ":")))')"
      printf '%s' "$record"
      return 1
    fi
    rm -f "$receipt_tmp"
    record_digest="$(printf '%s' "$record" | "$DISPATCH_PYTHON" -c 'import hashlib,sys; print("sha256:" + hashlib.sha256(sys.stdin.buffer.read()).hexdigest())')"
  fi
  if [ "$status" != "evidence_paths_not_distinct" ] && [ -n "$output_sha256" ] && [ -n "$terminal_artifact_sha256" ] && ! evidence_paths_valid "$output_sha256" "$terminal_artifact_sha256" "$record_digest"; then
    EVIDENCE_PATHS_DISTINCT=false
    record="$(printf '%s' "$record" | "$DISPATCH_PYTHON" -c 'import json,sys; value=json.load(sys.stdin); value.update(status="evidence_publication_invalid", exit=2, terminal_observed=False, read_only_guarantee="none", certification_eligible=False); print(json.dumps(value, separators=(",", ":")))')"
    printf '%s' "$record"
    return 1
  fi
  printf '%s' "$record"
}

write_terminal_artifact() {
  local path="$1" status="$2" rc="$3" payload="" outcome_id="" temporary=""
  [ -n "$path" ] || return 0
  outcome_id="${TASK_ID:-$REVIEWER_ID}"
  if [ "$status" = "ok" ] && [ "$rc" -eq 0 ]; then
    payload="$("$DISPATCH_PYTHON" - "$outcome_id" "${ATTEMPT_ID:-attempt-1}" <<'PY'
import json
import sys

print(json.dumps({
    "id": sys.argv[1],
    "attempt_id": sys.argv[2],
    "kind": "complete",
    "summary": "dispatcher observed a completed provider answer",
}, separators=(",", ":")))
PY
)"
  else
    payload="$("$DISPATCH_PYTHON" - "$outcome_id" "${ATTEMPT_ID:-attempt-1}" "$status" "$rc" <<'PY'
import json
import sys

print(json.dumps({
    "id": sys.argv[1],
    "attempt_id": sys.argv[2],
    "kind": "failed",
    "reason": f"dispatcher outcome {sys.argv[3]} (exit {sys.argv[4]})",
}, separators=(",", ":")))
PY
)"
  fi
  temporary="$(make_tmp)" || return 1
  if ! printf '%s' "$payload" >"$temporary" || ! "$DISPATCH_PYTHON" "$PUBLISH_HELPER" --root "$EVIDENCE_ROOT" publish "$path" "$temporary"; then
    rm -f "$temporary"
    return 1
  fi
  rm -f "$temporary"
}

ORCH_FAMILY="$(normalise_family "$ORCH_FAMILY")"
EVIDENCE_PATHS_DISTINCT=true

evidence_paths_distinct() {
  local -a paths=()
  paths=("$OUT" "$TERMINAL_ARTIFACT")
  [ -n "$DISPATCH_RECEIPT" ] && paths+=("$DISPATCH_RECEIPT")
  "$DISPATCH_PYTHON" "$PUBLISH_HELPER" --root "$EVIDENCE_ROOT" identity "${paths[@]}"
}

evidence_paths_valid() {
  local output_digest="$1" terminal_digest="$2" receipt_digest="${3:-}"
  local -a entries=()
  entries=("$OUT" "$output_digest" "$TERMINAL_ARTIFACT" "$terminal_digest")
  [ -n "$DISPATCH_RECEIPT" ] && entries+=("$DISPATCH_RECEIPT" "$receipt_digest")
  "$DISPATCH_PYTHON" "$PUBLISH_HELPER" --root "$EVIDENCE_ROOT" verify "${entries[@]}"
}

emit_evidence_path_rejection() {
  local record=""
  EVIDENCE_PATHS_DISTINCT=false
  echo "answer, dispatcher terminal, and receipt evidence paths must be distinct files; path, symlink, and hardlink aliases are rejected" >&2
  record="$(emit_record "${TOOL:-chain}" "" "" "evidence_paths_not_distinct" 2 "" "none" "" "" "" "" "" "" "" "" "" "" "" "" "$RISK_TIER" "evidence-path-alias" false "" "$TERMINAL_ARTIFACT" "")"
  printf '%s\n' "$record"
}

# Specific failure signatures only. Do not treat any mention of "quota" as a failure.
fail_sig='(Authentication required|Please sign in|Please( run)? login|not logged in|not authenticated|Unauthorized|insufficient_quota|quota exceeded|rate limit exceeded|usage limit reached)'
model_fail_sig='(model[^[:cntrl:]]*(unavailable|not available|not found|unsupported|does not exist)|unknown model|capacity|overloaded)'
adapter_command_for_tool() {
  case "$1" in
    claude) echo "claude";;
    codex) echo "codex";;
    cursor) echo "cursor-agent";;
    kiro) echo "kiro-cli";;
    copilot) echo "copilot";;
    *) return 1;;
  esac
}

fixed_provider_family_for_tool() {
  case "$1" in
    claude) echo "anthropic";;
    codex) echo "openai";;
    *) echo "";;
  esac
}

owner_failure_is_unavailable() {
  local diagnostic="$1"
  case "$diagnostic" in
    *AGENT_FABRIC_PREFLIGHT_INCOMPLETE*|*AGENT_FABRIC_PROTOCOL_BUILD_INCOMPLETE*|*AGENT_FABRIC_PROTOCOL_BUILD_STALE*|*cannot\ resolve\ the\ tsx\ loader*|*found\ no\ valid\ tsx\ loader*|*node:\ not\ found*|*No\ such\ file\ or\ directory*|*npm\ install\ attestation\ is\ missing*|*provider\ signature\ is\ invalid*|*NODE_MODULE_VERSION*|*ERR_DLOPEN_FAILED*|*different\ Node.js\ version*)
      return 0;;
    *) return 1;;
  esac
}

resolve_adapter_executable() {
  local tool="$1" diag="$2" adapter_id="$3"
  local command_name="" owner_root="" owner="" owner_output="" owner_rc=0
  adapter_resolution=""
  adapter_executable=""
  adapter_resolution_reason=""
  provider_assurance=""
  certifying_answer_bearing_leg="false"
  command_name="$(adapter_command_for_tool "$tool")" || {
    adapter_resolution="unavailable"
    adapter_resolution_reason="no adapter command mapping for $tool"
    echo "adapter executable resolution unavailable: $adapter_resolution_reason" >>"$diag"
    return 1
  }

  if [ -n "$adapter_id" ]; then
    owner_root="${AGENTS_HOME:-$PRODUCT_ROOT}"
    owner="$owner_root/scripts/agent-fabric"
    if [ -x "$owner" ]; then
      owner_output="$({ AGENTS_HOME="$owner_root" "$owner" adapter executable \
        --adapter "$adapter_id" \
        --product-root "$PRODUCT_ROOT" \
        --instance-root "${AGENT_FABRIC_INSTANCE_ROOT:-$PRODUCT_ROOT}" \
        --json; } 2>"$diag")"
      owner_rc=$?
      if [ "$owner_rc" -eq 0 ]; then
        local owner_executable="" owner_assurance="" owner_certifying=""
        owner_executable="$(printf '%s' "$owner_output" | "$DISPATCH_PYTHON" -c 'import json,sys
raw=sys.stdin.read()
try:
    value=json.loads(raw)
except (json.JSONDecodeError, TypeError):
    value={"executable":raw.strip()}
path=value.get("executable", "") if isinstance(value, dict) else ""
print(path if isinstance(path, str) else "")')"
        owner_assurance="$(printf '%s' "$owner_output" | "$DISPATCH_PYTHON" -c 'import json,sys
try:
    value=json.load(sys.stdin)
except (json.JSONDecodeError, TypeError):
    value={}
assurance=value.get("provider_assurance", "") if isinstance(value, dict) else ""
print(assurance if isinstance(assurance, str) else "")')"
        owner_certifying="$(printf '%s' "$owner_output" | "$DISPATCH_PYTHON" -c 'import json,sys
try:
    value=json.load(sys.stdin)
except (json.JSONDecodeError, TypeError):
    value={}
print("true" if isinstance(value, dict) and value.get("certifying_answer_bearing_leg") is True else "false")')"
        # The owner boolean is a status projection, not an authority input.
        # Derive the gate from the observed assurance so an inconsistent
        # owner response cannot certify an advisory route.
        case "$owner_assurance" in
          full-vendor-identity|lockfile-install-attestation) owner_certifying="true";;
          partial-signed-helpers|owner-controlled-install-root|*) owner_certifying="false";;
        esac
        if [ -n "$owner_executable" ] && [ "${owner_executable#*/}" != "$owner_executable" ] && [ -f "$owner_executable" ] && [ -x "$owner_executable" ]; then
          adapter_resolution="verified-owner"
          adapter_executable="$owner_executable"
          provider_assurance="$owner_assurance"
          certifying_answer_bearing_leg="$owner_certifying"
          return 0
        fi
        adapter_resolution="rejected"
        adapter_resolution_reason="adapter executable owner returned an invalid path for $adapter_id"
        echo "adapter executable resolution refused: $adapter_resolution_reason" >>"$diag"
        return 1
      fi
      if ! owner_failure_is_unavailable "$(cat "$diag" 2>/dev/null)"; then
        adapter_resolution="rejected"
        adapter_resolution_reason="adapter executable owner rejected $adapter_id"
        echo "adapter executable resolution refused: $adapter_resolution_reason" >>"$diag"
        return 1
      fi
      adapter_resolution_reason="adapter executable owner unavailable for $adapter_id"
    else
      adapter_resolution_reason="adapter executable owner not found: $owner"
    fi
  else
    adapter_resolution="unavailable"
    adapter_resolution_reason="route did not provide a compatibility adapter owner for $tool"
    echo "adapter executable resolution refused: $adapter_resolution_reason" >>"$diag"
    return 1
  fi

  if adapter_executable="$(command -v "$command_name" 2>/dev/null)" && [ -f "$adapter_executable" ] && [ -x "$adapter_executable" ]; then
    adapter_resolution="degraded-command-v"
    adapter_resolution_reason="DEGRADED: ${adapter_resolution_reason}; command -v fallback is not Fabric-verified"
    echo "WARNING: $adapter_resolution_reason: $adapter_executable" >>"$diag"
    return 0
  fi
  adapter_resolution="unavailable"
  adapter_resolution_reason="${adapter_resolution_reason}; command -v could not resolve $command_name"
  echo "adapter executable resolution unavailable: $adapter_resolution_reason" >>"$diag"
  return 1
}

resolve_routing() {
  # Resolve model routing via provenant if available, else via scripts/model_route.py from product root.
  # Returns JSON. If neither method is available, returns status="model_routing_unavailable".
  local tool="$1" alias="$2" role="$3" lead_family="$4" diag_file="$5"
  local model="$6" effort="$7" risk_tier="$8" capabilities_file="$9"
  local -a cmd=() route_args=()

  route_args=(--adapter "$tool" --alias "$alias" --role "$role" --lead-family "$lead_family" --require-distinct --adapter-gate direct-cli)
  [ -n "$model" ] && route_args+=(--model "$model")
  [ -n "$effort" ] && route_args+=(--effort "$effort")
  [ -n "$risk_tier" ] && route_args+=(--risk-tier "$risk_tier")
  [ -n "$capabilities_file" ] && [ -f "$capabilities_file" ] && route_args+=(--capabilities-file "$capabilities_file")

  # Try provenant first if available
  if command -v provenant >/dev/null 2>&1; then
    cmd=(provenant route resolve "${route_args[@]}")
    "${cmd[@]}" 2>>"$diag_file"
    return $?
  fi

  # Fall back to scripts/model_route.py from product root
  # Locate product root via git if possible, else try relative to this script
  local product_root=""
  if product_root="$(cd "$SCRIPT_DIR" && git rev-parse --show-toplevel 2>/dev/null)"; then
    if [ -f "$product_root/scripts/model_route.py" ]; then
      cmd=("$DISPATCH_PYTHON" "$product_root/scripts/model_route.py" "resolve" "${route_args[@]}")
      AGENT_FABRIC_PRODUCT_ROOT="$product_root" "${cmd[@]}" 2>>"$diag_file"
      return $?
    fi
  fi

  # Try relative path from script directory (should resolve to product root)
  if [ -f "$SCRIPT_DIR/../../../scripts/model_route.py" ]; then
    product_root="$(CDPATH= cd -- "$SCRIPT_DIR/../../.." && pwd)"
    cmd=("$DISPATCH_PYTHON" "$product_root/scripts/model_route.py" "resolve" "${route_args[@]}")
    AGENT_FABRIC_PRODUCT_ROOT="$product_root" "${cmd[@]}" 2>>"$diag_file"
    return $?
  fi

  # Unable to find routing capability; return typed status
  printf '{"status":"model_routing_unavailable","reason":"neither provenant nor scripts/model_route.py found"}\n'
  return 127
}

run_one() {  # $1 tool $2 model $3 effort -> writes answer and optional terminal artifact, echoes JSON, returns 0/1
  local tool="$1" model="$2" effort="$3"
  local tmpdir="" raw="" diag="" combined="" clean="" rc=0 status="" opath="" guarantee="" family="" endpoint="" identity=""
  local effort_substitution="" substitution="" requested_model="" requested_effort="" effort_source="" effort_capability_source=""
  local route_json="" route_rc=0 route_fields="" capabilities_file="" fallback_model="" primary_model="" catalog_model="" model_selection=""
  local policy_override="" route_risk_tier="" output_sha256="" terminal_artifact_sha256="" compatibility_adapter=""
  local adapter_resolution_failed=0 adapter_resolution="" adapter_executable="" adapter_resolution_reason=""
  local provider_assurance="" certifying_answer_bearing_leg=false terminal_observed=false
  model="$(resolve_model "$tool" "$model")"
  tmpdir="$(make_tmp_dir)"
  raw="$tmpdir/raw"
  diag="$tmpdir/diag"
  clean="$tmpdir/clean"
  combined="$tmpdir/combined"
  : >"$raw"
  : >"$diag"
  trap "rm -rf -- '$tmpdir'" EXIT
  trap "rm -rf -- '$tmpdir'; exit 143" INT TERM HUP
  family="$(fixed_provider_family_for_tool "$tool")"
  endpoint="$(endpoint_provider "$tool")"
  identity=""
  effort_substitution=""
  substitution=""
  catalog_model=""
  model_selection=""
  policy_override=""
  adapter_resolution=""
  adapter_executable=""
  adapter_resolution_reason=""
  provider_assurance=""
  certifying_answer_bearing_leg="false"
  terminal_observed="false"
  adapter_resolution_failed=0
  route_risk_tier="$RISK_TIER"
  requested_effort="$effort"
  effort_source=""
  effort_capability_source=""
  fallback_model=""
  requested_model="$model"
  capabilities_file=""
  primary_model=""
  if [ -n "$ORCH_FAMILY" ] && ! valid_family "$ORCH_FAMILY"; then
    guarantee="none"
    status="invalid_orchestrator_family"
    echo "invalid orchestrator family: $ORCH_FAMILY" >"$diag"
    rc=1
  elif [ -z "$ORCH_FAMILY" ]; then
    guarantee="none"
    status="orchestrator_family_required"
    echo "$tool disabled: pass --orchestrator-family so cross-family status can be proven" >"$diag"
    rc=1
  elif [ -z "$EVIDENCE_ROOT" ]; then
    guarantee="none"
    status="evidence_root_required"
    echo "$tool disabled: pass --evidence-root so the evidence boundary is stated, not inferred" >"$diag"
    rc=1
    record="$(emit_record "$tool" "$model" "$effort" "$status" "$rc" "" "$guarantee" "$family" "$endpoint" "$identity" "$effort_substitution" "$requested_effort" "$effort_source" "$effort_capability_source" "$substitution" "$requested_model" "$fallback_model" "$catalog_model" "$model_selection" "$route_risk_tier" "$policy_override" false "" "" "" "$adapter_resolution" "$adapter_executable" "$adapter_resolution_reason" "$provider_assurance" "$certifying_answer_bearing_leg")"
    cat "$diag" >&2
    printf '%s' "$record"
    return 1
  elif [ -n "$family" ] && [ "$family" = "$ORCH_FAMILY" ]; then
    guarantee="none"
    status="same_family_forbidden"
    echo "$tool disabled: same-family dispatch is forbidden" >"$diag"
    rc=1
  else
    if [ "$tool" = "codex" ]; then
      if ! resolve_adapter_executable "$tool" "$diag" "codex-app-server"; then
        adapter_resolution_failed=1
      fi
      capabilities_file="$tmpdir/codex-capabilities.json"
      if [ "$adapter_resolution_failed" -eq 0 ] && ! "$DISPATCH_PYTHON" "$SCRIPT_DIR/codex_capabilities.py" \
        --codex-bin "$adapter_executable" \
        --out "$capabilities_file" >>"$diag" 2>&1; then
        rm -f "$capabilities_file"
      fi
    fi
    route_json="$(resolve_routing "$tool" "$MODEL_ALIAS" "$ROUTE_ROLE" "$ORCH_FAMILY" "$diag" "$model" "$effort" "$RISK_TIER" "$capabilities_file")"
    route_rc=$?
    if route_fields="$(printf '%s' "$route_json" | "$DISPATCH_PYTHON" -c 'import json,sys; r=json.load(sys.stdin); print("|".join(str(r.get(k,"")) for k in ("status","resolved_model","model_family","endpoint_provider","identity_source","requested_effort","effort","effort_source","effort_capability_source","effort_substitution","substitution","fallback_model","catalog_model","model_selection","risk_tier","policy_override","compatibility_adapter")))' 2>>"$diag")"; then
      IFS='|' read -r status model family endpoint identity requested_effort effort effort_source effort_capability_source effort_substitution substitution fallback_model catalog_model model_selection route_risk_tier policy_override compatibility_adapter <<<"$route_fields"
      [ -n "$requested_model" ] || requested_model="$model"
      if [ "$adapter_resolution_failed" -eq 1 ]; then
        guarantee="none"
        printf '%s\n' "$route_json" >>"$diag"
        status="adapter_resolution_failed"
        rc=1
      elif [ "$route_rc" -ne 0 ]; then
        guarantee="none"
        printf '%s\n' "$route_json" >>"$diag"
        rc=1
      else
        status=""
        case "$tool" in
          claude|codex|cursor|kiro|copilot) ;;
          *)
            emit_record "$tool" "$model" "$effort" "unknown_tool" 1 "" "none" "$family" "$endpoint" "$identity" "$effort_substitution" "$requested_effort" "$effort_source" "$effort_capability_source"
            rm -f "$raw" "$diag"
            return 1
            ;;
        esac
        if [ "$tool" != "codex" ] && ! resolve_adapter_executable "$tool" "$diag" "$compatibility_adapter"; then
          status="adapter_resolution_failed"
          rc=1
          guarantee="none"
        else
          case "$tool" in
        claude)
          guarantee="enforced"
          local claude_verifier_system_prompt=""
          claude_verifier_system_prompt="You are a non-interactive cross-family verifier. You may use only Read, Grep, and Glob to inspect the requested workspace. Do not mutate files, use shell commands, call Task/tool/function abstractions, or launch subagents. Answer only the requested final verification text from the supplied prompt."
          if ! [ -x "$adapter_executable" ]; then
            status="tool_not_found"
            rc=127
          else
            CLAUDE_CODE_DISABLE_WORKFLOWS=1 "$adapter_executable" -p --bare --disable-slash-commands \
              --no-session-persistence --permission-mode plan --tools "Read,Grep,Glob" \
              --system-prompt "$claude_verifier_system_prompt" \
              ${model:+--model "$model"} ${effort:+--effort "$effort"} \
              <"$PROMPT_TMP" >"$raw" 2>"$diag"; rc=$?
          fi
          if [ "${status:-}" != "tool_not_found" ] && [ "$rc" -ne 0 ] && [ -n "$fallback_model" ] && cat "$raw" "$diag" | grep -Eqi "$model_fail_sig"; then
            primary_model="$model"
            : >"$raw"
            : >"$diag"
            model="$fallback_model"
            identity="runtime-provider-fallback"
            substitution="${substitution:+$substitution; }$primary_model unavailable; used $fallback_model"
            CLAUDE_CODE_DISABLE_WORKFLOWS=1 "$adapter_executable" -p --bare --disable-slash-commands \
              --no-session-persistence --permission-mode plan --tools "Read,Grep,Glob" \
              --system-prompt "$claude_verifier_system_prompt" \
              --model "$model" ${effort:+--effort "$effort"} \
              <"$PROMPT_TMP" >"$raw" 2>"$diag"; rc=$?
          fi
          if [ "${status:-}" != "tool_not_found" ] && [ "$rc" -ne 0 ] && cat "$raw" "$diag" | grep -Eqi "$fail_sig"; then
            if CLAUDE_CODE_DISABLE_WORKFLOWS=1 "$adapter_executable" auth status 2>/dev/null | grep -Eq '"loggedIn"[[:space:]]*:[[:space:]]*true'; then
              : >"$raw"
              : >"$diag"
              guarantee="oauth_safe_mode"
              CLAUDE_CODE_DISABLE_WORKFLOWS=1 "$adapter_executable" -p --safe-mode --no-session-persistence --permission-mode plan \
                --disable-slash-commands --tools "Read,Grep,Glob" \
                --system-prompt "$claude_verifier_system_prompt" \
                ${model:+--model "$model"} ${effort:+--effort "$effort"} \
              <"$PROMPT_TMP" >"$raw" 2>"$diag"; rc=$?
            fi
          fi
          if [ "$rc" -ne 0 ] && [ -n "$fallback_model" ] && [ "$model" = "$requested_model" ] && cat "$raw" "$diag" | grep -Eqi "$model_fail_sig"; then
            primary_model="$model"
            : >"$raw"
            : >"$diag"
            model="$fallback_model"
            identity="runtime-provider-fallback"
            substitution="${substitution:+$substitution; }$primary_model unavailable; used $fallback_model"
            if [ "$guarantee" = "oauth_safe_mode" ]; then
              CLAUDE_CODE_DISABLE_WORKFLOWS=1 "$adapter_executable" -p --safe-mode --no-session-persistence --permission-mode plan \
                --disable-slash-commands --tools "Read,Grep,Glob" --system-prompt "$claude_verifier_system_prompt" \
                --model "$model" ${effort:+--effort "$effort"} <"$PROMPT_TMP" >"$raw" 2>"$diag"; rc=$?
            else
              CLAUDE_CODE_DISABLE_WORKFLOWS=1 "$adapter_executable" -p --bare --disable-slash-commands \
                --no-session-persistence --permission-mode plan --tools "Read,Grep,Glob" --system-prompt "$claude_verifier_system_prompt" \
                --model "$model" ${effort:+--effort "$effort"} <"$PROMPT_TMP" >"$raw" 2>"$diag"; rc=$?
            fi
          fi ;;
        codex)
          guarantee="enforced"
          # Preserve the existing read-only invocation contract (`codex exec -s read-only`)
          # while replacing only its command-name resolution with the verified path.
          if ! [ -x "$adapter_executable" ]; then
            status="tool_not_found"
            rc=127
          else
            "$adapter_executable" exec -s read-only --ignore-user-config --ignore-rules --ephemeral ${model:+-m "$model"} \
              ${effort:+-c model_reasoning_effort="$effort"} \
              - <"$PROMPT_TMP" >"$raw" 2>"$diag"; rc=$?
          fi ;;
        cursor)
          guarantee="enforced"
          if ! [ -x "$adapter_executable" ]; then
            status="tool_not_found"
            rc=127
          else
            "$adapter_executable" -p --trust --mode ask --sandbox enabled --output-format text \
              ${model:+--model "$model"} "$(cat "$PROMPT_TMP")" </dev/null >"$raw" 2>"$diag"; rc=$?
          fi ;;
        kiro)
          guarantee="none"
          if [ "${CF_DISPATCH_ENABLE_KIRO:-0}" != "1" ]; then
            status="unsafe_by_default"
            echo "kiro disabled: no hard read-only mode verified in current local help" >"$diag"
            rc=1
          else
            guarantee="best_effort"
            if ! [ -x "$adapter_executable" ]; then
              status="tool_not_found"
              rc=127
            else
              "$adapter_executable" chat --no-interactive ${model:+--model "$model"} ${effort:+--effort "$effort"} \
                "$(cat "$PROMPT_TMP")" </dev/null >"$raw" 2>"$diag"; rc=$?
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
            if ! [ -x "$adapter_executable" ]; then
              status="tool_not_found"
              rc=127
            else
              "$adapter_executable" -p "$PROMPT" --mode plan --silent --disable-builtin-mcps \
                --available-tools='' --disallow-temp-dir ${model:+--model "$model"} ${effort:+--effort "$effort"} \
                </dev/null >"$raw" 2>"$diag"; rc=$?
            fi
          fi ;;
        *) emit_record "$tool" "$model" "$effort" "unknown_tool" 1 "" "none" "$family" "$endpoint" "$identity" "$effort_substitution" "$requested_effort" "$effort_source" "$effort_capability_source"; rm -f "$raw" "$diag"; return 1;;
          esac
        fi
      fi
    else
      guarantee="none"
      status="routing_record_invalid"
      echo "model routing returned no valid JSON record" >>"$diag"
      rc=1
    fi
  fi

  strip_ansi <"$raw" >"$clean"
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
    if "$DISPATCH_PYTHON" "$PUBLISH_HELPER" --root "$EVIDENCE_ROOT" publish "$OUT" "$clean"; then
      opath="$OUT"
    else
      status="output_write_error"
      rc=1
      guarantee="none"
      opath=""
    fi
  else
    if "$DISPATCH_PYTHON" "$PUBLISH_HELPER" --root "$EVIDENCE_ROOT" publish "$OUT" "$combined"; then
      opath="$OUT"
    else
      status="output_write_error"
      rc=1
      guarantee="none"
      opath=""
    fi
  fi
  output_sha256=""
  if [ -n "$opath" ] && [ -f "$opath" ]; then
    if ! output_sha256="$("$DISPATCH_PYTHON" "$PUBLISH_HELPER" --root "$EVIDENCE_ROOT" digest "$opath")"; then
      status="output_validation_error"
      rc=1
      guarantee="none"
      opath=""
    fi
  fi
  terminal_artifact_sha256=""
  if [ -n "$TERMINAL_ARTIFACT" ]; then
    if write_terminal_artifact "$TERMINAL_ARTIFACT" "$status" "$rc"; then
      if ! terminal_artifact_sha256="$("$DISPATCH_PYTHON" "$PUBLISH_HELPER" --root "$EVIDENCE_ROOT" digest "$TERMINAL_ARTIFACT")"; then
        status="terminal_artifact_validation_error"
        rc=1
        guarantee="none"
      else
        terminal_observed="true"
      fi
    else
      status="terminal_artifact_write_error"
      rc=1
      guarantee="none"
    fi
  fi
  if ! evidence_paths_distinct; then
    EVIDENCE_PATHS_DISTINCT=false
    status="evidence_paths_not_distinct"
    rc=2
    guarantee="none"
    terminal_observed="false"
    opath=""
  fi
  if ! emit_record "$tool" "$model" "$effort" "$status" "$rc" "$opath" "$guarantee" "$family" "$endpoint" "$identity" "$effort_substitution" "$requested_effort" "$effort_source" "$effort_capability_source" "$substitution" "$requested_model" "$fallback_model" "$catalog_model" "$model_selection" "$route_risk_tier" "$policy_override" "$terminal_observed" "$output_sha256" "$TERMINAL_ARTIFACT" "$terminal_artifact_sha256" "$adapter_resolution" "$adapter_executable" "$adapter_resolution_reason" "$provider_assurance" "$certifying_answer_bearing_leg"; then
    return 1
  fi
  [ "$status" = "ok" ]
}

if [ -n "$CHAIN" ]; then
  chain_base_out="$OUT"
  chain_base_terminal="$TERMINAL_ARTIFACT"
  chain_base_receipt="$DISPATCH_RECEIPT"
  chain_summary_path="${chain_base_receipt:-${chain_base_out}.chain.json}"
  chain_attempts_tmp="$(make_tmp)" || exit 1
  : >"$chain_attempts_tmp"
  chain_attempt_index=0
  chain_attempt_base="${ATTEMPT_ID:-attempt-1}"
  for spec in $CHAIN; do
    chain_attempt_index=$((chain_attempt_index + 1))
    t="${spec%%:*}"
    rest="${spec#*:}"
    m="${rest%%:*}"
    e="${rest#*:}"
    [ "$rest" = "$spec" ] && { m=""; e=""; }
    [ "$e" = "$m" ] && e=""
    OUT="${chain_base_out}.attempt-${chain_attempt_index}"
    TERMINAL_ARTIFACT="${chain_base_terminal}.attempt-${chain_attempt_index}"
    if [ -n "$chain_base_receipt" ]; then
      DISPATCH_RECEIPT="${chain_base_receipt}.attempt-${chain_attempt_index}"
    else
      DISPATCH_RECEIPT="${chain_base_out}.route.attempt-${chain_attempt_index}.json"
    fi
    ATTEMPT_ID="${chain_attempt_base}.attempt-${chain_attempt_index}"
    rec="$(run_one "$t" "$m" "$e")"; rc=$?
    echo "$rec" >&2
    printf '{"record":%s,"receipt_path":"%s"}\n' \
      "$rec" "$(printf '%s' "$DISPATCH_RECEIPT" | json_escape)" >>"$chain_attempts_tmp"
    if [ $rc -eq 0 ]; then
      chain_record_tmp="$(make_tmp)" || exit 1
      if ! "$DISPATCH_PYTHON" "$SCRIPT_DIR/cf_dispatch_chain.py" "$chain_attempts_tmp" "$rec" "$chain_summary_path" >"$chain_record_tmp"; then
        rm -f "$chain_record_tmp" "$chain_attempts_tmp"
        exit 1
      fi
      if ! "$DISPATCH_PYTHON" "$PUBLISH_HELPER" --root "$EVIDENCE_ROOT" publish "$chain_summary_path" "$chain_record_tmp"; then
        echo "cannot write immutable chain summary: $chain_summary_path" >&2
        rm -f "$chain_record_tmp" "$chain_attempts_tmp"
        exit 1
      fi
      cat "$chain_record_tmp"
      rm -f "$chain_record_tmp" "$chain_attempts_tmp"
      exit 0
    fi
  done
  chain_record_tmp="$(make_tmp)" || exit 1
  if ! "$DISPATCH_PYTHON" "$SCRIPT_DIR/cf_dispatch_chain.py" "$chain_attempts_tmp" "" "$chain_summary_path" >"$chain_record_tmp"; then
    rm -f "$chain_record_tmp" "$chain_attempts_tmp"
    exit 1
  fi
  if ! "$DISPATCH_PYTHON" "$PUBLISH_HELPER" --root "$EVIDENCE_ROOT" publish "$chain_summary_path" "$chain_record_tmp"; then
    echo "cannot write immutable chain summary: $chain_summary_path" >&2
    rm -f "$chain_record_tmp" "$chain_attempts_tmp"
    exit 1
  fi
  cat "$chain_record_tmp"
  rm -f "$chain_record_tmp" "$chain_attempts_tmp"
  exit 1
else
  [ -z "$TOOL" ] && { echo "need --tool or --chain" >&2; exit 2; }
  rec="$(run_one "$TOOL" "$MODEL" "$EFFORT")"; rc=$?
  echo "$rec"
  exit $rc
fi
