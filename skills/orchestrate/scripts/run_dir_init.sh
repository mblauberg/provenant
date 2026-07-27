#!/usr/bin/env bash
# Scaffold a run directory used as shared memory for a layered multi-agent run.
# Usage: run_dir_init.sh [run-dir] [--force]   (default dir: ./.agent-run/<UTC-timestamp>)
set -euo pipefail

ROOT=""
FORCE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --force) FORCE=1; shift;;
    -*) echo "unknown arg: $1" >&2; exit 2;;
    *)
      [ -n "$ROOT" ] && { echo "unexpected extra arg: $1" >&2; exit 2; }
      ROOT="$1"; shift;;
  esac
done

ROOT="${ROOT:-./.agent-run/$(date -u +%Y%m%dT%H%M%SZ)}"
if [ -d "$ROOT" ] && [ -n "$(ls -A "$ROOT" 2>/dev/null)" ] && [ "$FORCE" != "1" ]; then
  echo "refusing: $ROOT is non-empty (pass --force to reuse it)" >&2; exit 1
fi
mkdir -p "$ROOT"/{findings,crossfamily,traces}
CREATED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

if [ ! -e "$ROOT/MANIFEST.md" ] || [ "$FORCE" != "1" ]; then
cat > "$ROOT/MANIFEST.md" <<'EOF'
# Run manifest

One row per artifact. status: draft | verified | superseded | retired

| id | path | topic | produced_by | date | status | retention | supersedes |
|----|------|-------|-------------|------|--------|-----------|------------|
EOF
fi

if [ ! -e "$ROOT/RUN_RECEIPT.json" ] || [ "$FORCE" != "1" ]; then
cat > "$ROOT/RUN_RECEIPT.json" <<EOF
{
  "schema_version": 1,
  "status": "active",
  "created_at": "$CREATED_AT",
  "closed_at": null,
  "owner": "lead",
  "task": "",
  "retention_policy": "capsule-plus-referenced-evidence",
  "terminal_reason": null,
  "review_plan": {
    "risk_tier": "routine",
    "chair_family": "",
    "concurrency_ceiling": 1,
    "panels": [],
    "reviews": []
  },
  "pair": {
    "mode": "solo",
    "chair_family": "",
    "chair_id": "",
    "peer_family": "",
    "peer_id": "",
    "status": "not-running",
    "degradation_reason": "",
    "lease_path": "LEASE.json",
    "lease_generation": 0,
    "checkpoint_generation": 0,
    "current_stage": "",
    "in_flight": [],
    "assignment_artifacts": [],
    "stage_ledger": [],
    "handoff_generation": null
  },
  "owned_panes": [],
  "closed_panes": [],
  "handed_off_panes": [],
  "unclassified_paths": [],
  "pruned_paths": []
}
EOF
fi

if [ ! -e "$ROOT/traces/README.md" ] || [ "$FORCE" != "1" ]; then
cat > "$ROOT/traces/README.md" <<'EOF'
# Traces

Record worker dispatches, tool/CLI versions, failovers, objective checks, and verifier disagreement.
EOF
fi

if [ ! -e "$ROOT/decisions.md" ] || [ "$FORCE" != "1" ]; then
cat > "$ROOT/decisions.md" <<'EOF'
# Decisions

**State of play:** (one line — reload point for the next session)

## Resolved
## Unresolved (for human decision)
EOF
fi

if [ ! -e "$ROOT/SYNTHESIS.md" ] || [ "$FORCE" != "1" ]; then
: > "$ROOT/SYNTHESIS.md"
fi

if [ ! -e "$ROOT/FINAL_GATE.md" ] || [ "$FORCE" != "1" ]; then
cat > "$ROOT/FINAL_GATE.md" <<'EOF'
# Final gate

Mark each item PASS / FAIL / N/A before final response.

| gate | status | evidence |
|---|---|---|
| Worker artifacts listed in MANIFEST.md |  |  |
| Run terminalisation inputs and retention policy verified |  |  |
| Duplicate findings merged or superseded |  |  |
| Contradictions resolved or recorded unresolved |  |  |
| P0/P1 findings triaged or explicitly deferred |  |  |
| Objective checks run with command/source locators |  |  |
| Cross-family verifier record has status=ok, cross_family=true, and read_only_guarantee=enforced/oauth_safe_mode, or is marked scout only |  |  |
| CROSS-FAMILY-NOT-RUN reasons recorded when cross-family verification was unavailable |  |  |
| Advisory cross-family findings triaged and either verified or rejected |  |  |
| Document update wave run or explicitly N/A |  |  |
| Updated docs verified against current source/artifacts |  |  |
| High-stakes/low-oracle work has two family passes or CROSS-FAMILY-NOT-RUN reasons |  |  |
| No unauthorised shared-state writes |  |  |
| Run-owned panes/resources closed or explicitly handed off |  |  |
| Human-authority gates listed |  |  |
| Final claims have source/test/file anchors |  |  |
| Context hygiene classified: durable outputs retained; owned ephemeral payload archived/removed |  |  |
EOF
fi

echo "$ROOT"
