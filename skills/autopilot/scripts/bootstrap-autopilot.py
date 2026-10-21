#!/usr/bin/env python3
"""Scaffold an autopilot mission and substitute its CONFIG KNOBS."""

from __future__ import annotations

import datetime as dt
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


PROG = "bootstrap-autopilot.py"
REQUIRED_KNOBS = ("DOMAIN", "MISSION", "LOCKED_CONSTRAINTS", "BUILD_CEILING", "ESCALATION_GATES")
SUBST_TARGETS = ("README.md", "STATE.md", "HANDOFF.md")
SCAN_TARGETS = ("README.md", "STATE.md", "HANDOFF.md", "QUEUE.md")
DEFAULT_RUNAWAY_CAPS = (
    "max ~4 concurrent jobs; max ~3 active forks; fork-depth <=2; ~3 runs/unit "
    "before escalation; bounded-retry <=2; long-wake ~3600s"
)


def usage() -> str:
    return f'''{PROG} — scaffold a fresh autopilot mission state directory under
.agent-run/runs/<mission-run-dir>/, then substitute the domain CONFIG KNOBS.
Idempotent and non-clobbering. Never writes inside the skill directory.

USAGE:
  {PROG} <MISSION_ID> [DOMAIN]
  {PROG} --id <MISSION_ID> [--repo-root <PATH>] [--domain "<text>"] [--force] [--dry-run] [-q] [-h]

ARGUMENTS:
  MISSION_ID        (required) Short identifier for this mission. Must be a
                     path-safe slug: letters, digits, '.', '_', '-' only; no
                     '/', no leading '.', no '..'. Target directory is ALWAYS
                     <repo-root>/.agent-run/runs/<mission-run-dir>/ — no flag can point
                     the mission dir anywhere else.
  DOMAIN             (optional) One-line domain name; seeds the {{{{DOMAIN}}}}
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
  1) {PROG} my-mission "my domain"        # scaffolds .agent-run/runs/<mission-run-dir>/
  2) edit .agent-run/runs/<mission-run-dir>/GOAL.md  # fill CONFIG KNOBS (only
                                          #   file you must edit: {{{{MISSION}}}},
                                          #   {{{{LOCKED_CONSTRAINTS}}}},
                                          #   {{{{BUILD_CEILING}}}},
                                          #   {{{{ESCALATION_GATES}}}})
  3) {PROG} my-mission                    # substitutes knobs + reports any
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
      has not been filled yet (still has {{{{KNOBS}}}}). Expected after the
      FIRST run; fill GOAL.md and re-run. (Phase A normal-finish.)
   4  substitution ran but UNSUBSTITUTED {{{{...}}}} placeholders remain (a knob
      was missed, or a typo'd knob name). The offending placeholders + files
      are listed on stderr. Fix the knob block and re-run.

ENVIRONMENT NOTES (warnings, never fatal):
  * Worktree isolation for build spikes needs a Git repo plus direct human
    authority. Authorised checkouts use primary-root/.worktrees through the
    global scripts/worktree helper. If Git is absent, keep worktree mode off.
  * The loop runs natively on a Claude Code operator (Workflow()/resume; /loop
    + Stop hook), or via the external driver on a Codex operator (see
    references/codex-operator.md). With neither CLI detected, {PROG} warns;
    the mission still works for manual/agent dispatch.
'''


def git_output(args: list[str], cwd: Path | None = None) -> str | None:
    try:
        result = subprocess.run(
            ["git", *args], cwd=cwd, check=False, capture_output=True, text=True
        )
    except OSError:
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def parse_args(argv: list[str]) -> tuple[dict[str, object], list[str]]:
    values: dict[str, object] = {
        "mission_id": "", "repo_root": "", "domain": "", "force": False,
        "dry_run": False, "quiet": False,
    }
    positionals: list[str] = []
    index = 0
    while index < len(argv):
        arg = argv[index]
        if arg == "--":
            break
        elif arg in ("--id", "--repo-root", "--domain"):
            if index + 1 >= len(argv):
                label = {"--id": "--id", "--repo-root": "--repo-root", "--domain": "--domain"}[arg]
                die(f"missing value for {label}")
            key = {"--id": "mission_id", "--repo-root": "repo_root", "--domain": "domain"}[arg]
            values[key] = argv[index + 1]
            index += 2
        elif arg == "--force":
            values["force"] = True
            index += 1
        elif arg == "--dry-run":
            values["dry_run"] = True
            index += 1
        elif arg in ("-q", "--quiet"):
            values["quiet"] = True
            index += 1
        elif arg in ("-h", "--help"):
            print(usage(), end="")
            raise SystemExit(0)
        elif arg.startswith("-"):
            die(f"unknown option: {arg} (try --help)")
        else:
            positionals.append(arg)
            index += 1
    if values["mission_id"]:
        if positionals and not values["domain"]:
            values["domain"] = positionals.pop(0)
        if positionals:
            die(f"unexpected extra argument: {positionals[0]} (try --help)")
    else:
        if positionals:
            values["mission_id"] = positionals.pop(0)
        if positionals and not values["domain"]:
            values["domain"] = positionals.pop(0)
        if positionals:
            die(f"unexpected extra argument: {positionals[0]} (try --help)")
    return values, positionals


def err(message: str = "") -> None:
    print(f"{PROG}: {message}" if message else f"{PROG}:", file=sys.stderr)


def warn(message: str, quiet: bool = False) -> None:
    print(f"{PROG}: WARNING: {message}", file=sys.stderr)


def die(message: str) -> None:
    err(message)
    raise SystemExit(2)


def safe_read(path: Path) -> str | None:
    try:
        return path.read_text()
    except OSError:
        return None


def atomic_write(path: Path, content: str) -> bool:
    temporary = path.with_name(f"{path.name}.tmp.{os.getpid()}")
    try:
        temporary.write_text(content)
        os.replace(temporary, path)
        return True
    except OSError:
        temporary.unlink(missing_ok=True)
        return False


def fallback_content(name: str, domain: str) -> str:
    if name == "GOAL.md":
        knob_domain = domain if domain else "{{DOMAIN}}"
        return f'''# GOAL — the north star + the RUN/STOP gate
<!-- HUMAN-OWNED. The conductor reads this at the START of every iteration
     and obeys it. It must NEVER author content into this file. The ONLY
     clean exit is a human setting STATUS: STOP. -->

```config-knobs
DOMAIN            = {knob_domain}
MISSION           = {{{{MISSION}}}}
LOCKED_CONSTRAINTS = {{{{LOCKED_CONSTRAINTS}}}}
BUILD_CEILING     = {{{{BUILD_CEILING}}}}
ESCALATION_GATES  = {{{{ESCALATION_GATES}}}}
RUNAWAY_CAPS      = {{{{RUNAWAY_CAPS}}}}
```

## Mission

{{{{MISSION}}}}

Never self-close the mission. An empty queue triggers one bounded
re-enumeration pass; if still dry, write an idle checkpoint and pause until
a material resume trigger. Only human STOP closes the mission.

## Active directives
<!-- Human-editable steering block. Empty = follow default traversal. -->

## STATUS gate

STATUS: RUN
<!-- audit note: who/why/when this last flipped. RUN at scaffold. -->
PREV: (none)
'''
    defaults = {
        "STATE.md": "# STATE — conductor heartbeat\nUpdated: <YYYY-MM-DDTHH:MM:SSZ>\n\n## Heartbeat\n\n- **Run status:** RUNNING — phase: BOOTSTRAP (no iterations yet).\n- **Conductor lease:** unclaimed — generation 0.\n- **In flight:** (none)\n- **Next up:** Iteration 1 — enumerate the initial work-units into QUEUE.md.\n- **Blockers:** (none)\n- **Resume protocol:** read GOAL.md, then this file, then QUEUE.md head.\n\n## Hot note window (newest-first, max five)\n\n- **Note (iter0):** Mission scaffolded by bootstrap-autopilot.py.\n",
        "QUEUE.md": "# QUEUE — durable work queue + item-lease ledger\n\n## Tier 0 — foundational one-way-doors\n\n| id | status | depends-on | lease-owner | lease-expiry | notes |\n|----|--------|------------|-------------|---------------|-------|\n\n## Tier 1+\n\n| id | status | depends-on | lease-owner | lease-expiry | notes |\n|----|--------|------------|-------------|---------------|-------|\n\n## Count summary\n\n- pending: 0 · leased: 0 · done: 0 · blocked: 0 · deferred: 0\n- total items: 0\n",
        "HANDOFF.md": "# HANDOFF — capstone synthesis (stub until the mission produces artifacts)\n\n## TERMINAL PICKUP\n\nThe mission has been scaffolded but has not run yet. Start by filling the\nCONFIG KNOBS block in GOAL.md, then run iteration 1 (enumerate the initial\nwork frontier into QUEUE.md).\n",
        "README.md": "# Autopilot mission: {{DOMAIN}} — start here\n\n> Single human entry point. Never hard-codes a status snapshot — read\n> STATE.md and QUEUE.md for live state.\n\n## What this mission is\n\n> **Mission:** {{MISSION}}\n\nThe mission stays open until human STOP. See GOAL.md, STATE.md, QUEUE.md.\n",
    }
    return defaults[name]


def parse_knobs(goal: str) -> dict[str, str]:
    in_block = False
    result: dict[str, str] = {}
    for line in goal.splitlines():
        if re.fullmatch(r"```config-knobs\s*", line):
            in_block = True
            continue
        if re.fullmatch(r"```\s*", line):
            in_block = False
            continue
        if not in_block:
            continue
        line = line.lstrip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key, value = key.strip(), value.strip()
        if key:
            result[key] = value
    return result


def main(argv: list[str] | None = None) -> int:
    args, _ = parse_args(sys.argv[1:] if argv is None else argv)
    mission_id = str(args["mission_id"])
    repo_root_arg = str(args["repo_root"])
    domain = str(args["domain"])
    force, dry_run, quiet = bool(args["force"]), bool(args["dry_run"]), bool(args["quiet"])
    note = (lambda text="": print(text)) if not quiet else (lambda text="": None)
    warning = lambda text: warn(text, quiet)

    if not mission_id:
        die("a mission id is required (try --help)")
    if "/" in mission_id:
        die(f"mission id must not contain '/': {mission_id}")
    if mission_id.startswith("."):
        die(f"mission id must not start with '.': {mission_id}")
    if ".." in mission_id:
        die(f"mission id must not contain '..': {mission_id}")
    if not re.fullmatch(r"[A-Za-z0-9._-]+", mission_id):
        die(f"mission id must be a path-safe slug (letters, digits, '.', '_', '-' only): {mission_id}")

    if repo_root_arg:
        supplied_root = Path(repo_root_arg)
        if not supplied_root.is_dir():
            die(f"--repo-root does not exist or is not a directory: {repo_root_arg}")
        repo_root = supplied_root.resolve()
    else:
        git_root = git_output(["rev-parse", "--show-toplevel"])
        if git_root:
            repo_root = Path(git_root).resolve()
        else:
            repo_root = Path.cwd().resolve()
            warning(f"no git repo detected: anchoring .agent-run/ to the current directory ({repo_root}).")
    common_dir = git_output(["-C", str(repo_root), "rev-parse", "--path-format=absolute", "--git-common-dir"])
    if common_dir and Path(common_dir).name == ".git":
        repo_root = Path(common_dir).parent.resolve()

    agent_run = repo_root / ".agent-run"
    runs_root = agent_run / "runs"
    for path in (agent_run, runs_root):
        if path.is_symlink():
            die(f"refusing: {path} is a symlink")
        if path.exists() and not path.is_dir():
            die(f"refusing: {path} exists and is not a directory")
    slug = re.sub(r"[._]", "-", mission_id.lower())[:32]
    mission_dir: Path | None = None
    try:
        candidates = sorted(runs_root.glob(f"*-mission-{slug}-??????"))
    except OSError:
        candidates = []
    for candidate in candidates:
        if not (candidate / "GOAL.md").is_file() or not (candidate / ".mission-id").is_file():
            continue
        existing = (safe_read(candidate / ".mission-id") or "").splitlines()
        if existing and existing[0] == mission_id:
            if mission_dir is not None:
                die(f"multiple missions match {mission_id}; select one by path")
            mission_dir = candidate
    if mission_dir is None:
        stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d-%H%M")
        mission_dir = runs_root / f"{stamp}-mission-{slug}-{os.urandom(3).hex()}"
    if mission_dir.is_symlink():
        die(f"refusing: mission path is a symlink: {mission_dir}")

    scratch_root = agent_run / "scratch"
    if not dry_run:
        scratch_root.mkdir(parents=True, exist_ok=True)
        inside_worktree = git_output(["-C", str(repo_root), "rev-parse", "--is-inside-work-tree"]) == "true"
        if inside_worktree:
            common = git_output(["-C", str(repo_root), "rev-parse", "--path-format=absolute", "--git-common-dir"])
            if common:
                exclude = Path(common) / "info" / "exclude"
                exclude.parent.mkdir(parents=True, exist_ok=True)
                current = safe_read(exclude) or ""
                if "/.agent-run/" not in current.splitlines():
                    with exclude.open("a") as stream:
                        stream.write("\n/.agent-run/\n")

    if mission_dir.exists() and not mission_dir.is_dir():
        die(f"mission path exists but is not a directory: {mission_dir}")
    if not mission_dir.is_dir():
        note(f"Creating new mission directory: {mission_dir}")
        if not dry_run:
            mission_dir.mkdir(parents=True, exist_ok=True)
    mission = mission_dir.resolve() if mission_dir.is_dir() else mission_dir
    try:
        mission.relative_to(runs_root.resolve())
    except ValueError:
        die(f"refusing: resolved mission path escaped the .agent-run/ boundary: {mission}")
    marker = mission / ".mission-id"
    if marker.is_file():
        existing_id = (safe_read(marker) or "").splitlines()
        if not existing_id or existing_id[0] != mission_id:
            die(f"mission identity mismatch: {mission}")
    else:
        if dry_run:
            note(f"  [dry-run] write {marker}")
        else:
            marker.write_text(mission_id + "\n")
            note(f"  wrote {marker}")

    rerun = (mission / "GOAL.md").is_file()
    note("")
    note("==============================================================")
    note("  autopilot bootstrap")
    note(f"  mission:    {mission}")
    note(f"  skill:      {Path(__file__).resolve().parent.parent}")
    note(f"  phase:      {'B (substitute — GOAL.md present)' if rerun else 'A (scaffold — fresh mission)'}")
    if dry_run:
        note("  mode:       DRY-RUN (no changes will be made)")
    note("==============================================================")
    note("")
    note("[1/3] mission state files (template if present, else built-in default)")

    templates = Path(__file__).resolve().parent.parent / "templates"
    for filename in ("GOAL.md", "STATE.md", "QUEUE.md", "HANDOFF.md", "README.md"):
        template_name = filename.replace(".md", ".template.md")
        source = templates / template_name
        destination = mission / filename
        if source.is_file():
            if destination.exists() and not force:
                warning(f"exists, kept (not clobbered): {destination}")
            elif dry_run:
                note(f"  [dry-run] copy {source} -> {destination}")
            else:
                if destination.exists() and force:
                    warning(f"--force: overwriting existing file: {destination}")
                shutil.copy2(source, destination)
                note(f"  copied {source.name} -> {destination}")
        else:
            if destination.exists() and not force:
                warning(f"exists, kept (not clobbered): {destination}")
            elif dry_run:
                note(f"  [dry-run] write {destination}")
            else:
                if destination.exists() and force:
                    warning(f"--force: overwriting existing file: {destination}")
                destination.write_text(fallback_content(filename, domain))
                note(f"  wrote {destination}")

    goal_path = mission / "GOAL.md"
    if domain and goal_path.is_file() and not dry_run:
        contents = goal_path.read_text()
        pattern = re.compile(r"^([ \t]*DOMAIN[ \t]*=[ \t]*)\{\{DOMAIN\}\}[ \t]*$", re.MULTILINE)
        if pattern.search(contents):
            seeded = pattern.sub(lambda match: match.group(1) + domain, contents)
            goal_path.write_text(seeded)
            note(f"  seeded DOMAIN knob in GOAL.md = {domain}")

    note("")
    note("[2/3] substitute CONFIG KNOBS from GOAL.md")
    knobs_filled = True
    goal = safe_read(goal_path)
    substitutions: dict[str, str] = {}
    if goal is None and dry_run:
        note("  [dry-run] GOAL.md not on disk; skipping substitution preview.")
    elif goal is None:
        warning("GOAL.md missing — cannot substitute. (Unexpected after scaffold.)")
        knobs_filled = False
    else:
        knobs = parse_knobs(goal)
        if not knobs:
            warning("no CONFIG KNOBS block found in GOAL.md (expected a ```config-knobs fence).")
            knobs_filled = False
        for key, value in knobs.items():
            if value and value != "{{" + key + "}}" and "{{" not in value:
                substitutions[key] = value
        if knobs.get("RUNAWAY_CAPS", "") in ("", "{{RUNAWAY_CAPS}}") or "{{" in knobs.get("RUNAWAY_CAPS", ""):
            substitutions["RUNAWAY_CAPS"] = DEFAULT_RUNAWAY_CAPS
        unfilled = [key for key in REQUIRED_KNOBS if not knobs.get(key) or "{{" in knobs[key]]
        if unfilled:
            knobs_filled = False
            warning("required CONFIG KNOBS not yet filled in GOAL.md:" + " " + " ".join(unfilled))
        if dry_run:
            note("  (no filled knobs to substitute yet)")
        else:
            for target in SUBST_TARGETS:
                target_path = mission / target
                if not target_path.is_file():
                    continue
                content = target_path.read_text()
                for key, value in substitutions.items():
                    content = content.replace("{{" + key + "}}", value)
                target_path.write_text(content)
                if substitutions:
                    note(f"  substituted knobs in {target}")
                else:
                    note("  (no filled knobs to substitute yet)")

    note("")
    note("[3/3] scanning for unsubstituted {{...}} placeholders")
    leftovers: set[tuple[str, str]] = set()
    for target in SCAN_TARGETS:
        content = safe_read(mission / target)
        if content is None:
            continue
        leftovers.update((target, match) for match in re.findall(r"\{\{[A-Za-z0-9_]+\}\}", content))
    note("")
    note("Environment checks (warnings only — a mission without these still works):")
    if git_output(["rev-parse", "--is-inside-work-tree"]) == "true":
        note("  ok   git repo detected (authorised worktree spikes use primary-root/.worktrees via scripts/worktree).")
    else:
        warning("no git repo detected: worktree isolation for build spikes is OFF.")
    have_claude, have_codex = shutil.which("claude") is not None, shutil.which("codex") is not None
    if have_claude:
        note("  ok   claude CLI detected — reference operator (Workflow()/loop/Stop hook).")
    if have_codex:
        note("  ok   codex CLI detected — external driver + Ultra/native multi-agent (references/codex-operator.md).")
    if not have_claude and not have_codex:
        warning("no operator CLI detected (claude or codex). Manual/agent dispatch still works.")
    note("  info bounded waves + external model-family dispatch route through orchestrate.")
    note("")
    note("==============================================================")
    note("  RECEIPT")
    note("==============================================================")
    note(f"  mission root:    {mission}")
    note("  state files:     GOAL STATE QUEUE HANDOFF README")
    note("  human entry:     README.md")
    note(f"  knob source:     {mission}/GOAL.md  (the ONLY file you edit)")
    note("  delegated:       decision records -> implement/deliver RUN.json in this dir")
    note("                   bounded waves + model routing -> orchestrate")
    if dry_run:
        note("  mode:            DRY-RUN — nothing was changed.")
        note("==============================================================")
        return 0
    if not knobs_filled:
        note("")
        note("  STATUS: SCAFFOLDED — CONFIG KNOBS not filled yet.")
        note("  NEXT:")
        note(f"    1) Open  {mission}/GOAL.md  and fill the ```config-knobs block.")
        note(f"    2) Re-run:  {PROG} {mission_id}")
        note("==============================================================")
        return 3
    if leftovers:
        err("")
        err(f"UNSUBSTITUTED placeholders remain ({len(leftovers)}) — a knob was missed or mistyped:")
        for filename, placeholder in sorted(leftovers):
            err(f"    {placeholder}   in   {filename}")
        err("Fix the CONFIG KNOBS block in GOAL.md, then re-run.")
        return 4
    note("")
    note("  STATUS: READY — knobs substituted, no placeholders remain.")
    note(f"  NEXT: read {mission}/README.md, launch one detected operator, steer via")
    note("  GOAL.md 'Active directives', stop via STATUS: STOP.")
    note("==============================================================")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
