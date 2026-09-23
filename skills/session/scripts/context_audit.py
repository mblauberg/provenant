#!/usr/bin/env python3
"""Read-only audit of project context, logs, handoffs and agent scratch."""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import stat
import sys


SKIP_PARTS = {".git", ".worktrees", "node_modules", ".venv", "venv", ".backups", "archive"}
SCRATCH_NAMES = re.compile(r"^(?:\.temp|temp[-_.]|scratch[-_.]|.*\.(?:tmp|scratch|bak|orig)|.*\.bak[-_.].*)", re.I)
FRESHNESS = re.compile(r"^(?:updated|last updated|last verified|as of)\s*:\s*(\S+)", re.I | re.M)
HANDOFF_FIELD = re.compile(r"^(?:-\s*)?(?:\*\*)?(Status|Effort|Leg|Supersedes|Consumed-at)(?:\*\*)?\s*:\s*(.+?)\s*$", re.I | re.M)
CANONICAL_KEY = re.compile(r"^(?:-\s*)?(?:\*\*)?Canonical key(?:\*\*)?\s*:\s*([A-Za-z0-9._-]+)\s*$", re.I | re.M)


@dataclass(frozen=True)
class Finding:
    severity: str
    code: str
    path: str
    detail: str


def contained(path: Path, root: Path, *, kind: str) -> bool:
    """Require every component and the resolved object to stay below root."""
    try:
        relative = path.relative_to(root)
    except ValueError:
        return False
    current = root
    try:
        for part in relative.parts:
            current = current / part
            if current.is_symlink():
                return False
        resolved = current.resolve(strict=True)
        resolved.relative_to(root)
        observed = os.stat(current, follow_symlinks=False)
        if current.resolve(strict=True) != resolved:
            return False
    except (OSError, ValueError):
        return False
    return stat.S_ISREG(observed.st_mode) if kind == "file" else stat.S_ISDIR(observed.st_mode)


def skipped(path: Path, root: Path) -> bool:
    rel = path.relative_to(root)
    parts = set(rel.parts)
    return bool(parts & SKIP_PARTS)


def walked_files(root: Path):
    root = root.resolve()
    for current, dirs, files in os.walk(root, topdown=True, followlinks=False):
        base = Path(current)
        dirs[:] = [
            name for name in dirs
            if name not in SKIP_PARTS and contained(base / name, root, kind="dir")
        ]
        for name in files:
            path = base / name
            if contained(path, root, kind="file"):
                yield path


def walked_dirs(root: Path, name: str):
    root = root.resolve()
    for current, dirs, _files in os.walk(root, topdown=True, followlinks=False):
        base = Path(current)
        dirs[:] = [
            entry for entry in dirs
            if entry not in SKIP_PARTS and contained(base / entry, root, kind="dir")
        ]
        for entry in dirs:
            if entry == name:
                yield base / entry


def is_hot_markdown(path: Path, root: Path) -> bool:
    rel = path.relative_to(root)
    return path.name in {"SKILL.md", "AGENTS.md", "CLAUDE.md", "STATE.md"} or "docs" in rel.parts or "handoffs" in rel.parts


def has_freshness(text: str) -> bool:
    match = FRESHNESS.search(text)
    if not match or match.group(1).startswith("<"):
        return False
    value = match.group(1)
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        return parsed.tzinfo is not None
    except ValueError:
        return False


def audit(
    root: Path,
    *,
    max_markdown_kb: int = 15,
    max_log_mb: int = 5,
    max_state_lines: int = 120,
    stale_handoff_days: int = 30,
    stale_log_days: int = 30,
    now: datetime | None = None,
) -> list[Finding]:
    root = root.resolve()
    now = now or datetime.now(timezone.utc)
    findings: list[Finding] = []
    canonical_keys: dict[str, str] = {}

    for path in walked_files(root):
        if not path.is_file() or skipped(path, root):
            continue
        rel = path.relative_to(root).as_posix()
        try:
            size = path.stat().st_size
        except OSError:
            continue

        if path.suffix.lower() == ".md" and is_hot_markdown(path, root) and size > max_markdown_kb * 1024:
            findings.append(Finding("warning", "large-agent-doc", rel, f"{size} bytes exceeds {max_markdown_kb} KiB split/merge signal"))
        if path.suffix.lower() in {".log", ".jsonl"} and size > max_log_mb * 1024 * 1024:
            findings.append(Finding("warning", "large-raw-log", rel, f"{size} bytes exceeds {max_log_mb} MiB rotation/retention signal"))
        if path.suffix.lower() in {".log", ".jsonl"}:
            age_days = (now - datetime.fromtimestamp(path.stat().st_mtime, timezone.utc)).days
            if age_days > stale_log_days:
                findings.append(Finding("warning", "stale-raw-log", rel, f"{age_days} days old; apply the owning retention policy"))
        if path.parent == root and SCRATCH_NAMES.match(path.name):
            findings.append(Finding("warning", "root-scratch", rel, "unscoped repo-root scratch candidate; inspect ownership before removal"))

        if path.name == "STATE.md":
            try:
                lines = path.read_text(errors="replace").splitlines()
            except OSError:
                continue
            if len(lines) > max_state_lines:
                findings.append(Finding("warning", "state-over-cap", rel, f"{len(lines)} lines exceeds {max_state_lines}"))
            if not has_freshness("\n".join(lines[:12])):
                findings.append(Finding("warning", "state-freshness-missing", rel, "no anchored Updated/Last verified/As of field in first 12 lines"))

        if path.suffix.lower() == ".md" and "docs" in path.relative_to(root).parts:
            try:
                match = CANONICAL_KEY.search(path.read_text(errors="replace")[:4096])
            except OSError:
                match = None
            if match:
                key = match.group(1).lower()
                if key in canonical_keys:
                    findings.append(Finding("error", "duplicate-canonical-key", rel, f"same canonical key as {canonical_keys[key]}"))
                else:
                    canonical_keys[key] = rel

        if "handoffs" in path.parts:
            age_days = (now - datetime.fromtimestamp(path.stat().st_mtime, timezone.utc)).days
            if age_days > stale_handoff_days:
                findings.append(Finding("warning", "stale-live-handoff", rel, f"{age_days} days old; archive if consumed"))

    run_dirs: set[Path] = set()
    for run_root in walked_dirs(root, ".agent-run"):
        if contained(run_root, root, kind="dir") and not skipped(run_root, root):
            for path in run_root.iterdir():
                if not contained(path, root, kind="dir"):
                    continue
                if path.name == "runs":
                    run_dirs.update(child for child in path.iterdir() if contained(child, root, kind="dir"))
                elif path.name not in {"sessions", "scratch"}:
                    run_dirs.add(path)  # one-release legacy fallback
    for work_root in walked_dirs(root, ".work"):
        wf_root = work_root / "wf"
        if not contained(wf_root, root, kind="dir") or skipped(work_root, root):
            continue
        for workflow_dir in wf_root.iterdir():
            if contained(workflow_dir, root, kind="dir"):
                run_dirs.update(path for path in workflow_dir.iterdir() if contained(path, root, kind="dir"))
    for run_dir in sorted(run_dirs):
        if not contained(run_dir, root, kind="dir"):
            continue
        rel = run_dir.relative_to(root).as_posix()
        scaffold = ["MANIFEST.md", "RUN_RECEIPT.json", "SYNTHESIS.md", "FINAL_GATE.md"]
        new_kind = None
        if run_dir.parent.name == "runs":
            match = re.match(r"^\d{8}-\d{4}-(dispatch|batch|orch|delivery|mission|review|wf)-", run_dir.name)
            if match:
                new_kind = match.group(1)
        direct_delivery = (
            run_dir.parent.name in {".agent-run", "runs"}
            and contained(run_dir / "RUN.json", root, kind="file")
            and not any(contained(run_dir / name, root, kind="file") for name in scaffold)
        )
        if new_kind in {"dispatch", "batch"} or (run_dir.name.startswith("mcp-") and contained(run_dir / "RUN_RECEIPT.json", root, kind="file")):
            required = ["RUN_RECEIPT.json"]
        elif new_kind == "delivery" or direct_delivery:
            required = ["RUN.json"]
        elif new_kind == "mission":
            required = ["GOAL.md"]
        else:
            required = scaffold
        if run_dir.parent.name == "implement":  # orchestrated workflow capsules also require RUN.json
            required.append("RUN.json")
        missing = [
            name
            for name in required
            if not contained(run_dir / name, root, kind="file")
        ]
        if missing:
            findings.append(Finding("error", "incomplete-run-index", rel, "missing " + ", ".join(missing)))
        run_path = run_dir / "RUN.json"
        if contained(run_path, root, kind="file"):
            try:
                run = json.loads(run_path.read_text())
            except (OSError, json.JSONDecodeError):
                run = None
            if isinstance(run, dict) and run.get("schema_version") == 1 and run.get("contract") == "delivery-run":
                declared: set[str] = set()
                for index, artifact in enumerate(run.get("artifacts", [])):
                    if not isinstance(artifact, dict) or not isinstance(artifact.get("path"), str):
                        continue
                    declared.add(artifact["path"])
                    if artifact.get("class") == "scratch" and artifact.get("expires_at"):
                        try:
                            expiry = datetime.fromisoformat(str(artifact["expires_at"]).replace("Z", "+00:00"))
                        except ValueError:
                            findings.append(Finding("error", "invalid-scratch-expiry", f"{rel}/RUN.json", f"artifact {index} expiry is invalid"))
                        else:
                            if expiry <= now:
                                findings.append(Finding("warning", "expired-run-scratch", f"{rel}/{artifact['path']}", "manifest-owned scratch is expired; removal still requires cleanup authority"))
                for candidate in walked_files(run_dir):
                    if not candidate.is_file() or candidate == run_path:
                        continue
                    candidate_rel = candidate.relative_to(run_dir).as_posix()
                    if SCRATCH_NAMES.match(candidate.name) and candidate_rel not in declared:
                        findings.append(Finding("warning", "orphan-run-scratch", f"{rel}/{candidate_rel}", "scratch-like file is absent from the delivery artifact manifest"))

    active_handoffs: dict[tuple[str, str], str] = {}
    for path in (item for item in walked_files(root) if item.match("HANDOFF-*.md")):
        if not path.is_file() or skipped(path, root):
            continue
        rel_path = path.relative_to(root)
        rel = rel_path.as_posix()
        try:
            fields = {key.lower(): value.strip().lower() for key, value in HANDOFF_FIELD.findall(path.read_text(errors="replace")[:4096])}
        except OSError:
            continue
        missing_fields = {"status", "effort", "leg", "supersedes", "consumed-at"} - set(fields)
        if missing_fields:
            findings.append(Finding("error", "handoff-metadata-missing", rel, "missing " + ", ".join(sorted(missing_fields))))
        status = fields.get("status")
        if status not in {"active", "consumed"}:
            continue
        archived = "archive" in rel_path.parts
        if status == "consumed" and not archived:
            findings.append(Finding("error", "consumed-handoff-live", rel, "consumed handoff must move to the archive"))
        if status == "consumed" and fields.get("consumed-at", "pending") in {"", "pending", "none"}:
            findings.append(Finding("error", "consumed-handoff-date-missing", rel, "consumed handoff must record Consumed-at"))
        if status == "active" and archived:
            findings.append(Finding("error", "active-handoff-archived", rel, "active handoff must stay in the live handoff directory"))
        effort = fields.get("effort", "none")
        leg = fields.get("leg", "none")
        key = (effort, leg)
        if status == "active" and not archived and key != ("none", "none"):
            if key in active_handoffs:
                findings.append(Finding("error", "duplicate-active-handoff", rel, f"same effort/leg as {active_handoffs[key]}"))
            else:
                active_handoffs[key] = rel

    return sorted(findings, key=lambda item: (item.severity, item.code, item.path))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", default=".", type=Path)
    parser.add_argument("--max-markdown-kb", type=int, default=15)
    parser.add_argument("--max-log-mb", type=int, default=5)
    parser.add_argument("--max-state-lines", type=int, default=120)
    parser.add_argument("--stale-handoff-days", type=int, default=30)
    parser.add_argument("--stale-log-days", type=int, default=30)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--strict", action="store_true", help="exit 1 on structural errors")
    parser.add_argument("--warnings-as-errors", action="store_true", help="also fail on advisory signals")
    args = parser.parse_args(argv)
    if not args.root.is_dir():
        print(f"context audit root is not a directory: {args.root}", file=sys.stderr)
        return 2
    findings = audit(
        args.root,
        max_markdown_kb=args.max_markdown_kb,
        max_log_mb=args.max_log_mb,
        max_state_lines=args.max_state_lines,
        stale_handoff_days=args.stale_handoff_days,
        stale_log_days=args.stale_log_days,
    )
    if args.json:
        print(json.dumps({"root": str(args.root.resolve()), "findings": [asdict(item) for item in findings]}, indent=2))
    elif findings:
        for item in findings:
            print(f"{item.severity}: {item.code}: {item.path}: {item.detail}")
    else:
        print("clean: no context-hygiene signals")
    has_errors = any(item.severity == "error" for item in findings)
    return 1 if has_errors or (args.warnings_as_errors and findings) else 0


if __name__ == "__main__":
    raise SystemExit(main())
