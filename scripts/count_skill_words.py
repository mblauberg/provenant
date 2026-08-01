"""The single word-counting policy used for skill budget checks."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path


WORD_COUNT_HARD_LIMIT = 500
COUNTER_DESCRIPTION = (
    "Unified counter (whole file including frontmatter; markdown link URLs excluded)"
)
_MARKDOWN_LINK_URL = re.compile(r"\]\([^)]*\)")
_WORD = re.compile(r"\b[\w'-]+\b")
REPOSITORY_ROOT = Path(__file__).resolve().parents[1]


def count_skill_words(text: str) -> int:
    """Count the words users read, including frontmatter but excluding link URLs."""
    # Count the whole file because frontmatter is user-visible skill content. Strip
    # only markdown link destinations because they are navigation metadata, not
    # reading burden; retain the link text itself.
    readable_text = _MARKDOWN_LINK_URL.sub("]", text)
    return len(_WORD.findall(readable_text))


def word_count_diagnostics(path: Path) -> tuple[str, ...]:
    """Return a hard-limit error for a skill file, if applicable."""
    word_count = count_skill_words(path.read_text())
    prefix = f"{path}: whole file has {word_count} words"
    limits = f"hard limit is {WORD_COUNT_HARD_LIMIT}. {COUNTER_DESCRIPTION}."
    if word_count > WORD_COUNT_HARD_LIMIT:
        return (f"error: {prefix}; {limits}",)
    return ()


def _git_file_text(commit_sha: str, path: str) -> str:
    try:
        result = subprocess.run(
            ["git", "-C", str(REPOSITORY_ROOT), "show", f"{commit_sha}:{path}"],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return ""
    return result.stdout


def word_count_delta_warnings(merge_base_sha: str, head_sha: str) -> tuple[str, ...]:
    """Return warnings and hard-limit errors for changed skill entrypoints."""
    try:
        result = subprocess.run(
            [
                "git", "-C", str(REPOSITORY_ROOT), "diff", "--name-only",
                f"{merge_base_sha}..{head_sha}", "--", "skills/*/SKILL.md",
            ],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return ()

    diagnostics: list[str] = []
    for path in result.stdout.splitlines():
        old_text = _git_file_text(merge_base_sha, path)
        new_text = _git_file_text(head_sha, path)
        if not new_text:
            continue
        old_count = count_skill_words(old_text)
        new_count = count_skill_words(new_text)
        delta = new_count - old_count
        if new_count > WORD_COUNT_HARD_LIMIT:
            over = new_count - WORD_COUNT_HARD_LIMIT
            diagnostics.append(
                f"error: {path}: {old_count} -> {new_count} (+{delta}), "
                f"limit {WORD_COUNT_HARD_LIMIT}, {over} over"
            )
            continue
        threshold = 1 if new_count >= 490 else 5 if new_count >= 470 else 10
        if delta >= threshold:
            remaining = WORD_COUNT_HARD_LIMIT - new_count
            diagnostics.append(
                f"warning: {path}: {old_count} -> {new_count} (+{delta}), "
                f"limit {WORD_COUNT_HARD_LIMIT}, {remaining} remaining"
            )
    return tuple(diagnostics)
