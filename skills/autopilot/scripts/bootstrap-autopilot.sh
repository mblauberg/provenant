#!/bin/sh
# bootstrap-autopilot.sh — scaffold a fresh autopilot mission state directory.
# ============================================================================
# Layer 2 of the autopilot discipline (the state contract).
# See:  references/state-contract.md  ·  references/recovery-and-cadence.md
#       SKILL.md "Bootstrap"
#
# This script is DOMAIN-AGNOSTIC. It carries NO knowledge of any specific
# mission, constraint, gate, or product. Every domain-specific fact is a
# NAMED CONFIG KNOB ({{DOMAIN}}, {{MISSION}}, {{LOCKED_CONSTRAINTS}},
# {{BUILD_CEILING}}, {{ESCALATION_GATES}}, {{RUNAWAY_CAPS}}, ...) that the
# user fills ONCE in the CONFIG KNOBS block at the top of the mission's
# GOAL.md. The same machinery then drives any domain with zero edits to this
# script.
#
# WHAT IT DOES (two phases, same command, idempotent):
#
#   Phase A — SCAFFOLD (first run on an empty/new mission dir):
#     * create the mission directory under .agent-run/runs/<mission-run-dir>/ by
#       DEFAULT (a session-owned run location, matching how deliver/implement/
#       orchestrate stores its own .agent-run/runs/ artifacts) — NEVER inside
#       this skill's own directory.
#     * copy the skill's templates/ into the mission as the working state
#       files (README.md, GOAL.md, STATE.md, QUEUE.md, HANDOFF.md). If a
#       template is missing from the skill, a built-in DEFAULT is written
#       instead, so the script is self-sufficient today and forward-
#       compatible when templates change.
#     * leave {{KNOBS}} UNSUBSTITUTED on purpose — the user now fills them.
#     * print "next steps": fill the CONFIG KNOBS block in GOAL.md, re-run me.
#
#   Phase B — SUBSTITUTE (re-run after the knob block is filled):
#     * read the fenced CONFIG KNOBS block from GOAL.md
#     * do literal {{KNOB}} -> value substitution into README.md and STATE.md
#     * report any {{...}} placeholders still left behind (the canary that a
#       knob was missed) before declaring success.
#
# IDEMPOTENT / NON-CLOBBERING: an existing mission file is NEVER overwritten
# in the scaffold phase — it is left as-is and a warning is printed.
# Re-running only re-applies substitution to placeholders still present. So
# you can run this as many times as you like; it converges, it does not
# destroy.
#
# DELEGATION: this script scaffolds ONLY the thin conductor state (GOAL,
# STATE, QUEUE, HANDOFF, README). It never scaffolds a decision/ADR archive,
# a dashboard, or model-routing config — those are owned by implement/
# deliver/orchestrate respectively (see references/state-contract.md).
#
# INSTALL:  chmod +x scripts/bootstrap-autopilot.sh
# ============================================================================

set -eu

PROG="bootstrap-autopilot.sh"

# The skill root is the parent of this script's scripts/ dir. Resolve it so
# we can find templates/ regardless of the caller's cwd.
SCRIPT_PATH="$0"
case "$SCRIPT_PATH" in
  /*) : ;;                                   # already absolute
  *)  SCRIPT_PATH="$(pwd)/$SCRIPT_PATH" ;;   # make absolute from cwd
esac
SCRIPT_DIR="$(cd "$(dirname "$SCRIPT_PATH")" && pwd)"
SKILL_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
SKILL_TEMPLATES_DIR="$SKILL_DIR/templates"

# ---- defaults --------------------------------------------------------------
MISSION_ID=""
REPO_ROOT_ARG=""         # optional --repo-root override (advanced/testing only);
                          # the mission dir is ALWAYS <root>/.agent-run/runs/<mission-run-dir> —
                          # there is no way to point the mission dir anywhere else.
DOMAIN_ARG=""            # optional domain name; seeds {{DOMAIN}} knob
FORCE=false              # --force: re-copy templates over existing files (still warns)
DRY_RUN=false            # --dry-run: print actions, change nothing
QUIET=false

err()  { printf '%s: %s\n' "$PROG" "$*" >&2; }
warn() { printf '%s: WARNING: %s\n' "$PROG" "$*" >&2; }
note() { [ "$QUIET" = true ] || printf '%s\n' "$*"; }
die()  { err "$*"; exit 2; }

# ---- usage -----------------------------------------------------------------
usage() {
  cat <<EOF
$PROG — scaffold a fresh autopilot mission state directory under
.agent-run/runs/<mission-run-dir>/, then substitute the domain CONFIG KNOBS.
Idempotent and non-clobbering. Never writes inside the skill directory.

USAGE:
  $PROG <MISSION_ID> [DOMAIN]
  $PROG --id <MISSION_ID> [--repo-root <PATH>] [--domain "<text>"] [--force] [--dry-run] [-q] [-h]

ARGUMENTS:
  MISSION_ID        (required) Short identifier for this mission. Must be a
                     path-safe slug: letters, digits, '.', '_', '-' only; no
                     '/', no leading '.', no '..'. Target directory is ALWAYS
                     <repo-root>/.agent-run/runs/<mission-run-dir>/ — no flag can point
                     the mission dir anywhere else.
  DOMAIN             (optional) One-line domain name; seeds the {{DOMAIN}}
                     knob in GOAL.md on first scaffold.

OPTIONS:
  --id ID           Alternative to the positional MISSION_ID.
  --repo-root PATH  Override the detected repository/session root (advanced
                     use only — mainly for tests). The mission dir is still
                     always <PATH>/.agent-run/runs/<mission-run-dir>/; this only moves
                     where that anchor sits, it cannot escape it.
  --domain TEXT     Alternative to the positional DOMAIN.
  --force           Re-copy templates over existing mission files (still
                     warns; use ONLY to refresh from updated skill templates
                     — it will overwrite your edits to those files).
  --dry-run         Print what WOULD happen; create/modify nothing.
  -q, --quiet       Suppress the informational receipt (warnings still print).
  -h, --help        Show this help.

TWO-PHASE WORKFLOW (same command, run it twice):
  1) $PROG my-mission "my domain"        # scaffolds .agent-run/runs/<mission-run-dir>/
  2) edit .agent-run/runs/<mission-run-dir>/GOAL.md  # fill CONFIG KNOBS (only
                                          #   file you must edit: {{MISSION}},
                                          #   {{LOCKED_CONSTRAINTS}},
                                          #   {{BUILD_CEILING}},
                                          #   {{ESCALATION_GATES}})
  3) $PROG my-mission                    # substitutes knobs + reports any
                                          #   you left unfilled

WHAT IT CREATES:
  .agent-run/runs/<mission-run-dir>/
    README.md   single human entry point
    GOAL.md     human-owned: mission + STATUS:RUN/STOP gate + knobs
    STATE.md    heartbeat / recover-after-compaction anchor (empty)
    QUEUE.md    durable work queue + item-lease ledger (empty)
    HANDOFF.md  capstone synthesis stub

  Nothing is ever written inside this skill's own directory, and nothing is
  ever written outside <repo-root>/.agent-run/runs/<mission-run-dir>/ — that directory
  is the ONLY write location this script has. The repo root is the enclosing
  git repository's toplevel (matching how skills/session's context_audit.py
  and skills/orchestrate's run_dir_init.sh anchor their own .agent-run/
  trees), never the raw invocation cwd, so running this script from inside
  skills/autopilot/ still resolves to the repo root. Delegated implement/
  deliver waves may later add RUN.json to the same mission dir — this
  script does not create or manage that file.

EXIT CODES:
   0  success (scaffolded, or substituted with no placeholders left).
   2  usage / argument error.
   3  scaffolded OK, but the mission is INCOMPLETE — the CONFIG KNOBS block
      has not been filled yet (still has {{KNOBS}}). Expected after the
      FIRST run; fill GOAL.md and re-run. (Phase A normal-finish.)
   4  substitution ran but UNSUBSTITUTED {{...}} placeholders remain (a knob
      was missed, or a typo'd knob name). The offending placeholders + files
      are listed on stderr. Fix the knob block and re-run.

ENVIRONMENT NOTES (warnings, never fatal):
  * Worktree isolation for build spikes needs a Git repo plus direct human
    authority. Authorised checkouts use primary-root/.worktrees through the
    global scripts/worktree helper. If Git is absent, keep worktree mode off.
  * The loop runs natively on a Claude Code operator (Workflow()/resume; /loop
    + Stop hook), or via the external driver on a Codex operator (see
    references/codex-operator.md). With neither CLI detected, $PROG warns;
    the mission still works for manual/agent dispatch.
EOF
}

# ---- arg parsing -----------------------------------------------------------
while [ $# -gt 0 ]; do
  case "$1" in
    --id)         [ $# -ge 2 ] || die "missing value for --id";         MISSION_ID="$2"; shift 2 ;;
    --repo-root)  [ $# -ge 2 ] || die "missing value for --repo-root";  REPO_ROOT_ARG="$2"; shift 2 ;;
    --domain)     [ $# -ge 2 ] || die "missing value for --domain";     DOMAIN_ARG="$2"; shift 2 ;;
    --force)      FORCE=true; shift ;;
    --dry-run)    DRY_RUN=true; shift ;;
    -q|--quiet)   QUIET=true; shift ;;
    -h|--help)    usage; exit 0 ;;
    --)           shift; break ;;
    -*)           die "unknown option: $1 (try --help)" ;;
    *)
      if [ -z "$MISSION_ID" ]; then
        MISSION_ID="$1"
      elif [ -z "$DOMAIN_ARG" ]; then
        DOMAIN_ARG="$1"
      else
        die "unexpected extra argument: $1 (try --help)"
      fi
      shift ;;
  esac
done

[ -n "$MISSION_ID" ] || die "a mission id is required (try --help)"

# ---- validate the mission id as a path-safe slug ---------------------------
# The mission directory under .agent-run/runs/ is the owned write location.
# A mission id is concatenated directly into that path, so it must never be
# able to add a path separator or a traversal segment.
case "$MISSION_ID" in
  */*) die "mission id must not contain '/': $MISSION_ID" ;;
  .*)  die "mission id must not start with '.': $MISSION_ID" ;;
  *..*) die "mission id must not contain '..': $MISSION_ID" ;;
esac
case "$MISSION_ID" in
  *[!A-Za-z0-9._-]*)
    die "mission id must be a path-safe slug (letters, digits, '.', '_', '-' only): $MISSION_ID" ;;
esac

# ---- resolve the repository/session root -----------------------------------
# Derived deterministically from the enclosing git repository's toplevel
# (matching how skills/session's context_audit.py and skills/orchestrate's
# run_dir_init.sh anchor their own .agent-run/ trees), NEVER from the raw
# invocation cwd -- so running this script from inside skills/autopilot/
# still resolves to the repo root, not a nested skills/autopilot/.agent-run/.
if [ -n "$REPO_ROOT_ARG" ]; then
  [ -d "$REPO_ROOT_ARG" ] || die "--repo-root does not exist or is not a directory: $REPO_ROOT_ARG"
  REPO_ROOT="$(cd "$REPO_ROOT_ARG" && pwd -P)"
elif GIT_TOPLEVEL="$(git rev-parse --show-toplevel 2>/dev/null)"; then
  REPO_ROOT="$(cd "$GIT_TOPLEVEL" && pwd -P)"
else
  REPO_ROOT="$(pwd -P)"
  warn "no git repo detected: anchoring .agent-run/ to the current directory ($REPO_ROOT)."
fi
if COMMON_DIR="$(git -C "$REPO_ROOT" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"; then
  if [ "$(basename "$COMMON_DIR")" = ".git" ]; then
    REPO_ROOT="$(cd "$(dirname "$COMMON_DIR")" && pwd -P)"
  fi
fi

# ---- build the mission dir under .agent-run/runs/ ONLY ----------------------
AGENT_RUN_ROOT="$REPO_ROOT/.agent-run"
[ -L "$AGENT_RUN_ROOT" ] && die "refusing: $AGENT_RUN_ROOT is a symlink"
[ -e "$AGENT_RUN_ROOT" ] && [ ! -d "$AGENT_RUN_ROOT" ] && die "refusing: $AGENT_RUN_ROOT exists and is not a directory"
RUNS_ROOT="$AGENT_RUN_ROOT/runs"
[ -L "$RUNS_ROOT" ] && die "refusing: $RUNS_ROOT is a symlink"
[ -e "$RUNS_ROOT" ] && [ ! -d "$RUNS_ROOT" ] && die "refusing: $RUNS_ROOT exists and is not a directory"
MISSION_SLUG="$(printf '%s' "$MISSION_ID" | tr '[:upper:]' '[:lower:]' | tr '._' '--' | cut -c1-32)"
MISSION_DIR=""
# One-release legacy fallback: an existing mission stays at its original path.
if [ -f "$AGENT_RUN_ROOT/$MISSION_ID/GOAL.md" ]; then
  MISSION_DIR="$AGENT_RUN_ROOT/$MISSION_ID"
else
  for candidate in "$RUNS_ROOT"/*-mission-"$MISSION_SLUG"-??????; do
    [ -f "$candidate/GOAL.md" ] || continue
    [ -f "$candidate/.mission-id" ] || continue
    candidate_id=""
    IFS= read -r candidate_id < "$candidate/.mission-id" || true
    [ "$candidate_id" = "$MISSION_ID" ] || continue
    [ -z "$MISSION_DIR" ] || die "multiple missions match $MISSION_ID; select one by path"
    MISSION_DIR="$candidate"
  done
fi
if [ -z "$MISSION_DIR" ]; then
  MISSION_SUFFIX="$(od -An -N3 -tx1 /dev/urandom | tr -d ' \n')"
  MISSION_DIR="$RUNS_ROOT/$(date -u +%Y%m%d-%H%M)-mission-$MISSION_SLUG-$MISSION_SUFFIX"
fi
[ -L "$MISSION_DIR" ] && die "refusing: mission path is a symlink: $MISSION_DIR"
SCRATCH_ROOT="$AGENT_RUN_ROOT/scratch"
if [ "$DRY_RUN" != true ]; then
  mkdir -p "$SCRATCH_ROOT"
  if git -C "$REPO_ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    COMMON_DIR="$(git -C "$REPO_ROOT" rev-parse --path-format=absolute --git-common-dir)"
    EXCLUDE_FILE="$COMMON_DIR/info/exclude"
    mkdir -p "$(dirname "$EXCLUDE_FILE")"
    grep -Fxq '/.agent-run/' "$EXCLUDE_FILE" 2>/dev/null || printf '\n/.agent-run/\n' >> "$EXCLUDE_FILE"
  fi
fi

# ---- dry-run executor ------------------------------------------------------
# Every filesystem mutation routes through these so --dry-run is honoured in
# exactly one place.
do_mkdir() {
  if [ "$DRY_RUN" = true ]; then note "  [dry-run] mkdir -p $1"; else mkdir -p "$1"; fi
}
# do_write FILE  (content on stdin). Honours non-clobber + --force + --dry-run.
do_write() {
  _f="$1"
  if [ -e "$_f" ] && [ "$FORCE" != true ]; then
    warn "exists, kept (not clobbered): $_f"
    cat >/dev/null              # drain stdin
    return 0
  fi
  if [ -e "$_f" ] && [ "$FORCE" = true ]; then
    warn "--force: overwriting existing file: $_f"
  fi
  if [ "$DRY_RUN" = true ]; then
    cat >/dev/null
    note "  [dry-run] write $_f"
  else
    cat > "$_f"
    note "  wrote $_f"
  fi
}
# do_copy SRC DST. Honours non-clobber + --force + --dry-run.
do_copy() {
  _s="$1"; _d="$2"
  if [ -e "$_d" ] && [ "$FORCE" != true ]; then
    warn "exists, kept (not clobbered): $_d"
    return 0
  fi
  if [ "$DRY_RUN" = true ]; then
    note "  [dry-run] copy $_s -> $_d"
  else
    cp "$_s" "$_d"
    note "  copied $(basename "$_s") -> $_d"
  fi
}

# ---- resolve / create the mission root -------------------------------------
if [ -e "$MISSION_DIR" ] && [ ! -d "$MISSION_DIR" ]; then
  die "mission path exists but is not a directory: $MISSION_DIR"
fi
if [ ! -d "$MISSION_DIR" ]; then
  note "Creating new mission directory: $MISSION_DIR"
  do_mkdir "$MISSION_DIR"
fi
if [ -d "$MISSION_DIR" ]; then
  MISSION="$(cd "$MISSION_DIR" && pwd -P)"
  # Boundary check: reject any path escape (e.g. a symlink swapped in after
  # the earlier -L check, or an unexpected resolution) — the physically
  # resolved mission dir must land EXACTLY on its chosen canonical or legacy path.
  if [ "$MISSION" != "$AGENT_RUN_ROOT/$MISSION_ID" ]; then
    if [ "$MISSION" != "$MISSION_DIR" ]; then
      die "refusing: resolved mission path escaped the .agent-run/ boundary: $MISSION"
    fi
  fi
else
  # DRY-RUN with nothing created yet: report the path that WOULD be used.
  MISSION="$MISSION_DIR"
fi
printf '%s\n' "$MISSION_ID" | do_write "$MISSION/.mission-id"

# Detect whether this is a first scaffold or a re-run (GOAL.md present already).
RERUN=false
[ -f "$MISSION/GOAL.md" ] && RERUN=true

note ""
note "=============================================================="
note "  autopilot bootstrap"
note "  mission:    $MISSION"
note "  skill:      $SKILL_DIR"
note "  phase:      $([ "$RERUN" = true ] && echo 'B (substitute — GOAL.md present)' || echo 'A (scaffold — fresh mission)')"
[ "$DRY_RUN" = true ] && note "  mode:       DRY-RUN (no changes will be made)"
note "=============================================================="

# ============================================================================
# PHASE A — SCAFFOLD (only the missing pieces; never clobber)
# ============================================================================

# --- helper: install a state file from the skill template if present, else
#     fall back to a built-in default written via the named generator fn. ----
install_file() {
  _rel="$1"; _tpl="$2"; _fn="$3"
  _dst="$MISSION/$_rel"
  if [ -f "$SKILL_TEMPLATES_DIR/$_tpl" ]; then
    do_copy "$SKILL_TEMPLATES_DIR/$_tpl" "$_dst"
  else
    "$_fn" | do_write "$_dst"
  fi
}

note ""
note "[1/3] mission state files (template if present, else built-in default)"

# ---------------------------------------------------------------------------
# Built-in default generators. Each emits a domain-AGNOSTIC file carrying
# {{KNOBS}} where domain facts belong. GOAL.md is intentionally the ONLY file
# the user edits; README.md and STATE.md are substituted from it.
# ---------------------------------------------------------------------------

gen_goal() {
  if [ -n "$DOMAIN_ARG" ]; then _dom="$DOMAIN_ARG"; else _dom="{{DOMAIN}}"; fi
  cat <<EOF
# GOAL — the north star + the RUN/STOP gate
<!-- HUMAN-OWNED. The conductor reads this at the START of every iteration
     and obeys it. It must NEVER author content into this file. The ONLY
     clean exit is a human setting STATUS: STOP. -->

\`\`\`config-knobs
DOMAIN            = $_dom
MISSION           = {{MISSION}}
LOCKED_CONSTRAINTS = {{LOCKED_CONSTRAINTS}}
BUILD_CEILING     = {{BUILD_CEILING}}
ESCALATION_GATES  = {{ESCALATION_GATES}}
RUNAWAY_CAPS      = {{RUNAWAY_CAPS}}
\`\`\`

## Mission

{{MISSION}}

Never self-close the mission. An empty queue triggers one bounded
re-enumeration pass; if still dry, write an idle checkpoint and pause until
a material resume trigger. Only human STOP closes the mission.

## Active directives
<!-- Human-editable steering block. Empty = follow default traversal. -->

## STATUS gate

STATUS: RUN
<!-- audit note: who/why/when this last flipped. RUN at scaffold. -->
PREV: (none)
EOF
}

gen_state() {
  cat <<'EOF'
# STATE — conductor heartbeat
Updated: <YYYY-MM-DDTHH:MM:SSZ>
<!-- Rewritten IN FULL every iteration; the recover-after-compaction anchor. -->

## Heartbeat

- **Run status:** RUNNING — phase: BOOTSTRAP (no iterations yet).
- **Conductor lease:** unclaimed — generation 0.
- **In flight:** (none)
- **Next up:** Iteration 1 — enumerate the initial work-units into QUEUE.md.
- **Blockers:** (none)
- **Resume protocol:** read GOAL.md, then this file, then QUEUE.md head.

## Hot note window (newest-first, max five)

- **Note (iter0):** Mission scaffolded by bootstrap-autopilot.sh.
EOF
}

gen_queue() {
  cat <<'EOF'
# QUEUE — durable work queue + item-lease ledger

## Tier 0 — foundational one-way-doors

| id | status | depends-on | lease-owner | lease-expiry | notes |
|----|--------|------------|-------------|---------------|-------|

## Tier 1+

| id | status | depends-on | lease-owner | lease-expiry | notes |
|----|--------|------------|-------------|---------------|-------|

## Count summary

- pending: 0 · leased: 0 · done: 0 · blocked: 0 · deferred: 0
- total items: 0
EOF
}

gen_handoff() {
  cat <<'EOF'
# HANDOFF — capstone synthesis (stub until the mission produces artifacts)

## TERMINAL PICKUP

The mission has been scaffolded but has not run yet. Start by filling the
CONFIG KNOBS block in GOAL.md, then run iteration 1 (enumerate the initial
work frontier into QUEUE.md).
EOF
}

gen_readme() {
  cat <<'EOF'
# Autopilot mission: {{DOMAIN}} — start here

> Single human entry point. Never hard-codes a status snapshot — read
> STATE.md and QUEUE.md for live state.

## What this mission is

> **Mission:** {{MISSION}}

The mission stays open until human STOP. See GOAL.md, STATE.md, QUEUE.md.
EOF
}

install_file "GOAL.md"     "GOAL.template.md"     gen_goal
install_file "STATE.md"    "STATE.template.md"    gen_state
install_file "QUEUE.md"    "QUEUE.template.md"    gen_queue
install_file "HANDOFF.md"  "HANDOFF.template.md"  gen_handoff
install_file "README.md"   "README.template.md"   gen_readme

# --- seed the DOMAIN knob into the freshly-installed GOAL.md -----------------
if [ -n "$DOMAIN_ARG" ] && [ -f "$MISSION/GOAL.md" ] && [ "$DRY_RUN" != true ]; then
  if grep -qE '^[[:space:]]*DOMAIN[[:space:]]*=[[:space:]]*\{\{DOMAIN\}\}[[:space:]]*$' "$MISSION/GOAL.md"; then
    esc_dom="$(printf '%s' "$DOMAIN_ARG" | sed -e 's/[&\\|]/\\&/g')"
    tmpg="$(mktemp "$SCRATCH_ROOT/bootstrap-autopilot.goal.XXXXXX")"
    sed -e "s|^\([[:space:]]*DOMAIN[[:space:]]*=[[:space:]]*\){{DOMAIN}}[[:space:]]*\$|\1$esc_dom|" \
      "$MISSION/GOAL.md" > "$tmpg" && cat "$tmpg" > "$MISSION/GOAL.md" && rm -f "$tmpg"
    note "  seeded DOMAIN knob in GOAL.md = $DOMAIN_ARG"
  fi
fi

# ============================================================================
# PHASE B — SUBSTITUTE the CONFIG KNOBS from GOAL.md into README.md/STATE.md
# ============================================================================
note ""
note "[2/3] substitute CONFIG KNOBS from GOAL.md"

# GOAL.md is excluded — it is the knob SOURCE and the human's file.
SUBST_TARGETS="README.md STATE.md HANDOFF.md"

GOAL_FILE="$MISSION/GOAL.md"
KNOBS_FILLED=true

if [ ! -f "$GOAL_FILE" ] && [ "$DRY_RUN" = true ]; then
  note "  [dry-run] GOAL.md not on disk; skipping substitution preview."
elif [ ! -f "$GOAL_FILE" ]; then
  warn "GOAL.md missing — cannot substitute. (Unexpected after scaffold.)"
  KNOBS_FILLED=false
else
  KNOB_PAIRS="$(
    awk '
      /^```config-knobs[[:space:]]*$/ { inblk=1; next }
      /^```[[:space:]]*$/             { if (inblk) { inblk=0 } ; next }
      inblk {
        line=$0
        sub(/^[[:space:]]+/, "", line)
        if (line ~ /^#/ || line == "") next
        eq=index(line, "=")
        if (eq == 0) next
        key=substr(line, 1, eq-1)
        val=substr(line, eq+1)
        sub(/[[:space:]]+$/, "", key); sub(/^[[:space:]]+/, "", key)
        sub(/^[[:space:]]+/, "", val); sub(/[[:space:]]+$/, "", val)
        if (key == "") next
        printf "%s\t%s\n", key, val
      }
    ' "$GOAL_FILE"
  )"

  if [ -z "$KNOB_PAIRS" ]; then
    warn "no CONFIG KNOBS block found in GOAL.md (expected a \`\`\`config-knobs fence)."
    KNOBS_FILLED=false
  fi

  if [ "$DRY_RUN" = true ]; then SED_PROG=/dev/null; else SED_PROG="$(mktemp "$SCRATCH_ROOT/bootstrap-autopilot.sed.XXXXXX")"; fi
  : > "$SED_PROG"
  printf '%s\n' "$KNOB_PAIRS" | while IFS="$(printf '\t')" read -r k v; do
    [ -n "$k" ] || continue
    case "$v" in
      ""|"{{$k}}"|*"{{"*"}}"*) : ;;   # unfilled — do NOT emit a substitution
      *)
        esc_v="$(printf '%s' "$v" | sed -e 's/[&\\|]/\\&/g')"
        printf 's|{{%s}}|%s|g\n' "$k" "$esc_v" >> "$SED_PROG"
        ;;
    esac
  done

  # Default for the one knob with a documented fixed default not carried in
  # every GOAL block.
  gv="$(printf '%s\n' "$KNOB_PAIRS" | awk -F'\t' '$1=="RUNAWAY_CAPS"{print $2; exit}')"
  case "$gv" in
    ""|"{{RUNAWAY_CAPS}}"|*"{{"*"}}"*)
      printf 's|{{RUNAWAY_CAPS}}|max ~4 concurrent jobs; max ~3 active forks; fork-depth <=2; ~3 runs/unit before escalation; bounded-retry <=2; long-wake ~3600s|g\n' >> "$SED_PROG"
      ;;
  esac

  REQUIRED="DOMAIN MISSION LOCKED_CONSTRAINTS BUILD_CEILING ESCALATION_GATES"
  UNFILLED_REQUIRED=""
  for rk in $REQUIRED; do
    rv="$(printf '%s\n' "$KNOB_PAIRS" | awk -F'\t' -v k="$rk" '$1==k{print $2; exit}')"
    case "$rv" in
      ""|"{{$rk}}"|*"{{"*"}}"*) UNFILLED_REQUIRED="$UNFILLED_REQUIRED $rk"; KNOBS_FILLED=false ;;
    esac
  done
  [ -n "$UNFILLED_REQUIRED" ] && warn "required CONFIG KNOBS not yet filled in GOAL.md:$UNFILLED_REQUIRED"

  if [ -s "$SED_PROG" ]; then
    for t in $SUBST_TARGETS; do
      tf="$MISSION/$t"
      [ -f "$tf" ] || continue
      if [ "$DRY_RUN" = true ]; then
        note "  [dry-run] would substitute knobs in $t"
      else
        tmp="$(mktemp "$SCRATCH_ROOT/bootstrap-autopilot.$(basename "$t").XXXXXX")"
        sed -f "$SED_PROG" "$tf" > "$tmp" && cat "$tmp" > "$tf" && rm -f "$tmp"
        note "  substituted knobs in $t"
      fi
    done
  else
    note "  (no filled knobs to substitute yet)"
  fi
  [ "$DRY_RUN" = true ] || rm -f "$SED_PROG"
fi

# ---- placeholder canary: report any {{...}} still left behind ---------------
note ""
note "[3/3] scanning for unsubstituted {{...}} placeholders"
if [ "$DRY_RUN" = true ]; then LEFTOVER_FILE=/dev/null; else LEFTOVER_FILE="$(mktemp "$SCRATCH_ROOT/bootstrap-autopilot.leftover.XXXXXX")"; fi
: > "$LEFTOVER_FILE"
SCAN_TARGETS="README.md STATE.md HANDOFF.md QUEUE.md"
for t in $SCAN_TARGETS; do
  tf="$MISSION/$t"
  [ -f "$tf" ] || continue
  grep -oE '\{\{[A-Za-z0-9_]+\}\}' "$tf" 2>/dev/null | sort -u | while read -r ph; do
    printf '%s\t%s\n' "$t" "$ph" >> "$LEFTOVER_FILE"
  done || true
done

LEFTOVER_COUNT=0
if [ -s "$LEFTOVER_FILE" ]; then
  LEFTOVER_COUNT="$(wc -l < "$LEFTOVER_FILE" | tr -d ' ')"
fi

# ---- environment warnings (never fatal) ------------------------------------
note ""
note "Environment checks (warnings only — a mission without these still works):"

if command -v git >/dev/null 2>&1 && git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  note "  ok   git repo detected (authorised worktree spikes use primary-root/.worktrees via scripts/worktree)."
else
  warn "no git repo detected: worktree isolation for build spikes is OFF."
fi

HAVE_CLAUDE=false; HAVE_CODEX=false
command -v claude >/dev/null 2>&1 && HAVE_CLAUDE=true
command -v codex  >/dev/null 2>&1 && HAVE_CODEX=true
if [ "$HAVE_CLAUDE" = true ]; then
  note "  ok   claude CLI detected — reference operator (Workflow()/loop/Stop hook)."
fi
if [ "$HAVE_CODEX" = true ]; then
  note "  ok   codex CLI detected — external driver + Ultra/native multi-agent (references/codex-operator.md)."
fi
if [ "$HAVE_CLAUDE" = false ] && [ "$HAVE_CODEX" = false ]; then
  warn "no operator CLI detected (claude or codex). Manual/agent dispatch still works."
fi
note "  info bounded waves + external model-family dispatch route through orchestrate."

# ============================================================================
# RECEIPT + exit code
# ============================================================================
note ""
note "=============================================================="
note "  RECEIPT"
note "=============================================================="
note "  mission root:    $MISSION"
note "  state files:     GOAL STATE QUEUE HANDOFF README"
note "  human entry:     README.md"
note "  knob source:     $MISSION/GOAL.md  (the ONLY file you edit)"
note "  delegated:       decision records -> implement/deliver RUN.json in this dir"
note "                   bounded waves + model routing -> orchestrate"

if [ "$DRY_RUN" = true ]; then
  note "  mode:            DRY-RUN — nothing was changed."
  note "=============================================================="
  [ "$DRY_RUN" = true ] || rm -f "$LEFTOVER_FILE"
  exit 0
fi

if [ "$KNOBS_FILLED" != true ]; then
  note ""
  note "  STATUS: SCAFFOLDED — CONFIG KNOBS not filled yet."
  note "  NEXT:"
  note "    1) Open  $MISSION/GOAL.md  and fill the \`\`\`config-knobs block."
  note "    2) Re-run:  $PROG $MISSION_ID"
  note "=============================================================="
  rm -f "$LEFTOVER_FILE"
  exit 3
fi

if [ "$LEFTOVER_COUNT" -gt 0 ]; then
  err ""
  err "UNSUBSTITUTED placeholders remain ($LEFTOVER_COUNT) — a knob was missed or mistyped:"
  sort -u "$LEFTOVER_FILE" | while IFS="$(printf '\t')" read -r f p; do
    err "    $p   in   $f"
  done
  err "Fix the CONFIG KNOBS block in GOAL.md, then re-run."
  rm -f "$LEFTOVER_FILE"
  exit 4
fi

note ""
note "  STATUS: READY — knobs substituted, no placeholders remain."
note "  NEXT: read $MISSION/README.md, launch one detected operator, steer via"
note "  GOAL.md 'Active directives', stop via STATUS: STOP."
note "=============================================================="
rm -f "$LEFTOVER_FILE"
exit 0
