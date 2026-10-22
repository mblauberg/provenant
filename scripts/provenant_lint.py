#!/usr/bin/env python3
"""Lint branch names and commit messages against the repository's branch and commit doctrine.

`lint branch [NAME]` checks a branch (default: the current one). `lint commit-msg FILE` checks a commit
message file and works directly as a git `commit-msg` hook. Exit 0 passes (or skips), 1 reports violations,
2 is a usage error.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path
import re
import subprocess
import sys

_POLICY = Path(__file__).resolve().parents[1] / "skills/orchestrate/scripts/fabric_policy.py"
_spec = importlib.util.spec_from_file_location("provenant_fabric_policy", _POLICY)
if _spec is None or _spec.loader is None:  # pragma: no cover - defensive
    raise ModuleNotFoundError(f"fabric policy reader is missing: {_POLICY}")
fabric_policy = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = fabric_policy
_spec.loader.exec_module(fabric_policy)

# Ported from Sealle's branch-name.mjs and land.mjs. ASCII mode matches JS `\b`, `\d` and `\w`; JS `\s`
# also spans Unicode space, so it is spelled out. tests/test_provenant_lint.py checks the equivalence.
_SP = r"[\s   -     　﻿]"
TYPES = "feat|fix|refactor|perf|test|docs|chore|ci|build"
SLUG = r"[a-z][a-z0-9]*(?:-[a-z][a-z0-9]*){1,4}"
RESERVED = re.compile(r"(?:^|[-/])(?:batch|wave|round|lane|agent|codex|claude|gemini|opencode)(?:$|-)", re.ASCII)
DEPENDENCY_UPDATE = re.compile(r"dependabot/[A-Za-z0-9._@/-]+", re.ASCII)
LAND = re.compile(rf"land/[0-9]{{8}}-{SLUG}", re.ASCII)
MAX_BRANCH = 48

SESSION_PHRASES = re.compile(
    r"\b(?:WP\d+|(?:wave|phase|batch|lane)(?:\d+[a-z0-9-]*|SP+\d+[a-z0-9-]*|-[a-z]*\d[a-z0-9-]*)"
    r"|round(?:\d+[a-z0-9-]*|-[a-z]*\d[a-z0-9-]*)|\d{8}-\d{4}-[a-z0-9-]+-[a-f0-9]{6}"
    r"|[a-z]+-\d{4}-\d{2}-\d{2}-[a-z0-9-]+|(?:fabric|run)[-:/][a-f0-9]{8,}(?:-[a-f0-9]+)*)\b".replace("SP", _SP),
    re.ASCII | re.IGNORECASE,
)
CLOSING_REFERENCE = re.compile(
    r"\b(?:closes?|closed|fix(?:es|ed)?|resolves?|resolved):?" + _SP + r"+#([1-9]\d*)\b", re.ASCII | re.IGNORECASE
)
ATTRIBUTION = (
    re.compile(r"^\s*co-authored-by\s*:", re.IGNORECASE),
    re.compile(
        r"\bgenerated\s+(?:with|by)\s+\[?(?:claude|codex|gemini|chatgpt|copilot|cursor|openai|anthropic|an?\s+ai\b)",
        re.IGNORECASE,
    ),
    re.compile(r"noreply@anthropic\.com", re.IGNORECASE),
)
# Messages git writes itself: they name branches and PRs, not work.
GENERATED = re.compile(r"(?:Merge (?:branch|branches|remote-tracking branch|pull request|tag|commit) |Revert \")")
SCISSORS = ">8"


def area_pattern(areas: list[str]) -> str:
    return "|".join(areas) if areas else "[a-z][a-z0-9]*"


def branch_problems(branch: str, areas: list[str]) -> list[str]:
    if DEPENDENCY_UPDATE.fullmatch(branch):
        return []
    problems = []
    if len(branch) > MAX_BRANCH:
        problems.append(f"{len(branch)} characters; the limit is {MAX_BRANCH}")
    if RESERVED.search(branch):
        problems.append("run vocabulary (batch, wave, round, lane) or an agent or model name")
    area = area_pattern(areas)
    shapes = (
        re.compile(rf"(?:{TYPES})/(?:{area})-{SLUG}", re.ASCII),
        re.compile(rf"proto/(?:{area})-{SLUG}", re.ASCII),
        LAND,
    )
    if not any(shape.fullmatch(branch) for shape in shapes):
        problems.append(
            "shape: use <type>/<area>-<2-to-5-word-slug>, proto/<area>-<slug> or land/<yyyymmdd>-<slug>"
            + (f" (areas: {', '.join(areas)})" if areas else "")
        )
    return problems


def strip_html_comments(text: str) -> str:
    return re.sub(r"<!--[\s\S]*?-->", "", text)


def clean_message(raw: bytes, comment_char: str = "#") -> list[str]:
    """Message lines as git would keep them: LF endings, no scissors tail, no comment lines."""
    text = raw.decode("utf-8", errors="replace").replace("\r\n", "\n").replace("\r", "\n")
    kept = []
    for line in text.split("\n"):
        if line.startswith(f"{comment_char} ------------------------ {SCISSORS} ------------------------"):
            break
        if not line.startswith(comment_char):
            kept.append(line)
    return strip_html_comments("\n".join(kept)).split("\n")


def message_problems(lines: list[str], lint: dict) -> list[str]:
    body = [line for line in lines if line.strip()]
    if not body:
        return ["empty commit message (comment-only or blank)"]
    if GENERATED.match(body[0]):
        return []
    problems = []
    for line in lines:
        if SESSION_PHRASES.search(line):
            problems.append(f"session marker: {line}")
        if lint["forbid_attribution"] and any(p.search(line) for p in ATTRIBUTION):
            problems.append(f"agent attribution: {line}")
        if lint["forbid_closing_keywords"] and CLOSING_REFERENCE.search(line):
            problems.append(f"closing keyword (the pull request owns closure; use Refs #n): {line}")
    return problems


def git(*arguments: str, cwd=None) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *arguments], cwd=cwd, capture_output=True, text=True, check=False)


def comment_char(cwd=None) -> str:
    if git("config", "--get", "commit.cleanup", cwd=cwd).stdout.strip() == "verbatim":
        return "\0"  # no line can start with it, so nothing is stripped
    char = git("config", "--get", "core.commentChar", cwd=cwd).stdout.rstrip("\n")
    return char if char and char != "auto" else "#"


def load_lint(cwd=None) -> dict:
    top = git("rev-parse", "--show-toplevel", cwd=cwd)
    root = top.stdout.strip() if top.returncode == 0 and top.stdout.strip() else (cwd or ".")
    tools, warnings = fabric_policy.tools_policy(root)
    for warning in warnings:
        print(f"provenant lint: warning: {warning}", file=sys.stderr)
    return tools


def usage() -> int:
    print("usage: provenant lint branch [NAME] | commit-msg FILE", file=sys.stderr)
    return 2


def lint_branch(arguments: list[str]) -> int:
    if len(arguments) > 1:
        return usage()
    tools = load_lint()
    if arguments:
        branch = arguments[0]
    else:
        head = git("symbolic-ref", "--short", "-q", "HEAD")
        branch = head.stdout.strip()
        if not branch:
            print("provenant lint: detached HEAD or not a git checkout; no branch to lint, skipping")
            return 0
    if branch in {tools["default_branch"], "main", "master"}:
        print(f"provenant lint: {branch} is a default branch; skipping")
        return 0
    problems = branch_problems(branch, tools["lint"]["areas"])
    for problem in problems:
        print(f"provenant lint: branch {branch}: {problem}", file=sys.stderr)
    return 1 if problems else 0


def lint_commit_msg(arguments: list[str]) -> int:
    if len(arguments) != 1:
        return usage()
    try:
        raw = Path(arguments[0]).read_bytes()
    except OSError as exc:
        print(f"provenant lint: cannot read {arguments[0]}: {exc.strerror or exc}", file=sys.stderr)
        return 2
    tools = load_lint()
    problems = message_problems(clean_message(raw, comment_char()), tools["lint"])
    for problem in problems:
        print(f"provenant lint: {problem}", file=sys.stderr)
    return 1 if problems else 0


def main(argv: list[str]) -> int:
    if not argv:
        return usage()
    verb, rest = argv[0], argv[1:]
    if verb == "branch":
        return lint_branch(rest)
    if verb == "commit-msg":
        return lint_commit_msg(rest)
    return usage()


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
