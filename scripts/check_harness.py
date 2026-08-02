"""Static checks for the global agent harness."""

from __future__ import annotations

import re
from pathlib import Path
import subprocess
import sys

import yaml

try:
    from scripts.count_skill_words import (
        WORD_COUNT_HARD_LIMIT,
        word_count_delta_warnings,
        word_count_diagnostics,
    )
except ModuleNotFoundError:  # Direct execution: python scripts/check_harness.py
    from count_skill_words import WORD_COUNT_HARD_LIMIT, word_count_delta_warnings, word_count_diagnostics


ROOT = Path(__file__).resolve().parents[1]
SKILL_NAME = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
# OpenAI's documented fallback catalogue budget, reviewed 2026-07-11. Keep a
# small wrapper/version margin below the hard provider ceiling.
CATALOGUE_TARGET_CHARS = 7_600
CATALOGUE_HARD_LIMIT_CHARS = 8_000
DESCRIPTION_LIMIT_CHARS = 1_024
SKILL_ENTRYPOINT_WORD_LIMIT = WORD_COUNT_HARD_LIMIT
RETIRED_NAMES = {
    "au-legal-writing-style",
    "clear-engineering-writing",
    "clean-writing",
    "eng-docs",
    "humanise-text",
    "multi-agent-orchestration",
    "skill-optimizer",
    "vercel-react-best-practices",
    "wayfinder",
    "write-a-skill",
}


def skill_errors() -> tuple[list[str], dict[str, object]]:
    errors: list[str] = []
    warnings: list[str] = []
    link_pattern = re.compile(r"\[[^]]+\]\(([^)]+)\)")
    descriptions: dict[str, str] = {}
    for skill in sorted((ROOT / "skills").glob("*/SKILL.md")):
        text = skill.read_text()
        match = re.match(r"^---\n(.*?)\n---\n", text, re.DOTALL)
        if not match:
            errors.append(f"{skill.relative_to(ROOT)}: missing YAML frontmatter")
            continue
        try:
            frontmatter = yaml.safe_load(match.group(1))
        except yaml.YAMLError as exc:
            errors.append(f"{skill.relative_to(ROOT)}: invalid YAML: {exc}")
            continue
        if not isinstance(frontmatter, dict):
            errors.append(f"{skill.relative_to(ROOT)}: frontmatter must be a mapping")
            continue
        if set(frontmatter) != {"name", "description"}:
            errors.append(f"{skill.relative_to(ROOT)}: local frontmatter profile permits only name and description")
        expected = skill.parent.name
        if len(expected) > 64 or not SKILL_NAME.fullmatch(expected):
            errors.append(f"{skill.relative_to(ROOT)}: directory name must be 1-64 lowercase kebab-case characters")
        if frontmatter.get("name") != expected:
            errors.append(
                f"{skill.relative_to(ROOT)}: name {frontmatter.get('name')!r} != directory {expected!r}"
            )
        description = frontmatter.get("description")
        if not isinstance(description, str) or not description.startswith("Use"):
            errors.append(f"{skill.relative_to(ROOT)}: description must start with 'Use'")
        elif len(description) > DESCRIPTION_LIMIT_CHARS:
            errors.append(f"{skill.relative_to(ROOT)}: description exceeds {DESCRIPTION_LIMIT_CHARS} characters")
        else:
            descriptions[expected] = description
            boundary = description[:250].lower()
            if not any(marker in boundary for marker in ("not for", "not a ", "only when")):
                errors.append(f"{skill.relative_to(ROOT)}: first 250 description characters need an explicit exclusion")
        for diagnostic in word_count_diagnostics(skill):
            relative = diagnostic.replace(f"{skill}:", f"{skill.relative_to(ROOT)}:", 1)
            if diagnostic.startswith("warning:"):
                warnings.append(relative)
            else:
                errors.append(relative)
        fixture = skill.parent / "evals" / "trigger_cases.yaml"
        if not fixture.is_file():
            errors.append(f"{skill.relative_to(ROOT)}: missing evals/trigger_cases.yaml")
        for target in link_pattern.findall(text):
            if target.startswith(("http://", "https://", "#", "/")):
                continue
            relative = target.split("#", 1)[0]
            if relative and not (skill.parent / relative).exists():
                errors.append(f"{skill.relative_to(ROOT)}: broken link {target}")
    duplicate_descriptions = {
        description for description in descriptions.values()
        if list(descriptions.values()).count(description) > 1
    }
    for description in sorted(duplicate_descriptions):
        names = sorted(name for name, value in descriptions.items() if value == description)
        errors.append(f"duplicate skill descriptions: {', '.join(names)}")
    catalogue = "".join(f"- {name}: {descriptions[name]}\n" for name in sorted(descriptions))
    if len(catalogue) > CATALOGUE_HARD_LIMIT_CHARS:
        errors.append(
            f"canonical catalogue has {len(catalogue)}/{CATALOGUE_HARD_LIMIT_CHARS} characters; "
            "compress or consolidate routing descriptions"
        )
    return errors, {
        "skills": len(descriptions),
        "description_chars": sum(map(len, descriptions.values())),
        "catalogue_chars": len(catalogue),
        "catalogue_bytes": len(catalogue.encode()),
        "warnings": warnings,
    }


def word_count_delta_diagnostics() -> tuple[str, ...]:
    """Use merge-train deltas when the repository exposes usable Git history."""
    try:
        base = subprocess.run(
            ["git", "-C", str(ROOT), "merge-base", "HEAD", "origin/main"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        head = subprocess.run(
            ["git", "-C", str(ROOT), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return ()
    if not base or not head:
        return ()
    return word_count_delta_warnings(base, head)


def openai_sidecar_errors() -> list[str]:
    errors: list[str] = []
    for path in sorted((ROOT / "skills").glob("*/agents/openai.yaml")):
        skill = path.parents[1].name
        try:
            data = yaml.safe_load(path.read_text())
        except yaml.YAMLError as exc:
            errors.append(f"{path.relative_to(ROOT)}: invalid YAML: {exc}")
            continue
        interface = data.get("interface") if isinstance(data, dict) else None
        if not isinstance(interface, dict):
            errors.append(f"{path.relative_to(ROOT)}: interface mapping is required")
            continue
        for field in ("display_name", "short_description", "default_prompt"):
            if not isinstance(interface.get(field), str) or not interface[field].strip():
                errors.append(f"{path.relative_to(ROOT)}: interface.{field} is required")
        short_description = interface.get("short_description")
        if isinstance(short_description, str) and short_description.strip() and not 25 <= len(short_description) <= 64:
            errors.append(
                f"{path.relative_to(ROOT)}: interface.short_description must be 25-64 characters"
            )
        if f"${skill}" not in str(interface.get("default_prompt", "")):
            errors.append(f"{path.relative_to(ROOT)}: default_prompt must invoke ${skill}")
        for icon in ("icon_small", "icon_large"):
            value = interface.get(icon)
            if isinstance(value, str) and value.startswith("./") and not (path.parents[1] / value).exists():
                errors.append(f"{path.relative_to(ROOT)}: missing {icon} asset {value}")
    return errors


def stale_name_errors() -> list[str]:
    errors: list[str] = []
    roots = [ROOT / "README.md", ROOT / "HARNESS.md", ROOT / "MAINTAINING.md", ROOT / "skills"]
    for root in roots:
        paths = [root] if root.is_file() else root.rglob("*")
        for path in paths:
            if not path.is_file() or ".backups" in path.parts or path.suffix in {".pyc", ".png"}:
                continue
            try:
                text = path.read_text()
            except UnicodeDecodeError:
                continue
            for name in RETIRED_NAMES:
                pattern = rf"(?<![A-Za-z0-9-]){re.escape(name)}(?![A-Za-z0-9-])"
                if re.search(pattern, text):
                    errors.append(f"{path.relative_to(ROOT)}: retired name {name}")
    return errors


def main() -> int:
    skill_failures, metrics = skill_errors()
    delta_diagnostics = word_count_delta_diagnostics()
    errors = skill_failures + openai_sidecar_errors() + stale_name_errors()
    for diagnostic in delta_diagnostics:
        if diagnostic.startswith("error:"):
            errors.append(diagnostic)
        else:
            metrics["warnings"].append(diagnostic)
    for warning in metrics["warnings"]:
        print(f"\033[33mWARN:\033[0m {warning}", file=sys.stderr)
    if errors:
        for error in errors:
            print(f"\033[31mFAIL:\033[0m {error}", file=sys.stderr)
        return 1
    target_state = "within target" if metrics["catalogue_chars"] <= CATALOGUE_TARGET_CHARS else "above target"
    print(
        f"PASS: {metrics['skills']} skills; descriptions={metrics['description_chars']} chars; "
        f"catalogue={metrics['catalogue_chars']}/{CATALOGUE_HARD_LIMIT_CHARS} chars "
        f"({metrics['catalogue_bytes']} bytes, {target_state}); frontmatter, fixtures, links and sidecars clean"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
