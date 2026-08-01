"""The single word-counting policy used for skill budget checks."""

from __future__ import annotations

import re
import subprocess
from pathlib import Path


WORD_COUNT_HARD_LIMIT = 500
WORD_COUNT_WARNING_LOW_WATERMARK = 470
WORD_COUNT_WARNING_HIGH_WATERMARK = 490
WORD_COUNT_WARNING_LOW_DELTA = 10
WORD_COUNT_WARNING_MIDDLE_DELTA = 5
WORD_COUNT_WARNING_HIGH_DELTA = 1
COUNTER_DESCRIPTION = (
    "Unified counter (whole file including frontmatter; Markdown links, reference "
    "definitions, and document path tokens excluded)"
)
WARNING_POLICY_DESCRIPTION = (
    "delta warning boundaries: +10 below 470, +5 at 470-489, +1 at 490-499"
)
_DOCUMENT_EXTENSION = r"(?:md|rst|txt|py|rs|go|java|js|jsx|mjs|cjs|ts|tsx|vue|json|ya?ml|toml|xml|sql|sh|html|css|csv|pdf|docx)"
_DOCUMENT_PATH_TOKEN = (
    rf"(?:[A-Za-z0-9_.-]+/)+[A-Za-z0-9_.-]+\.{_DOCUMENT_EXTENSION}"
    rf"(?:#[A-Za-z0-9_./-]+)?"
)
_DOCUMENT_NAME_TOKEN = rf"[A-Za-z0-9_-]+\.{_DOCUMENT_EXTENSION}"
_REFERENCE_DESTINATION = (
    rf"(?:https?://|mailto:)[^\s<>()]+"
    rf"|(?:\.\.?/|/)?{_DOCUMENT_PATH_TOKEN}"
    rf"|{_DOCUMENT_NAME_TOKEN}"
)
_MARKDOWN_INLINE_LINK = re.compile(r"(?P<label>!?\[[^]\n]*\])\([^\)\n]*\)")
_MARKDOWN_REFERENCE_LINK = re.compile(r"(?P<label>!?\[[^]\n]*\])\[[^]\n]*\]")
_MARKDOWN_REFERENCE_DEFINITION = re.compile(
    rf"(?m)^[ \t]{{0,3}}\[[^]\n]+\]:[ \t]*(?:{_REFERENCE_DESTINATION})"
    rf"(?:[ \t]+(?:\"[^\n\"]*\"|'[^\n']*'|\([^\n)]*\)))?[ \t]*$"
)
_BARE_DOCUMENT_PATH = re.compile(
    rf"(?<![A-Za-z0-9_./-]){_DOCUMENT_PATH_TOKEN}(?![A-Za-z0-9_.-])"
)
_BARE_DOCUMENT_NAME = re.compile(
    rf"(?<![A-Za-z0-9_./-]){_DOCUMENT_NAME_TOKEN}(?![A-Za-z0-9_.-])"
)
_WORD = re.compile(r"\b[\w'-]+\b")
REPOSITORY_ROOT = Path(__file__).resolve().parents[1]


def count_skill_words(text: str) -> int:
    """Count readable words in the complete skill file, including frontmatter."""
    # These are explicit Markdown/navigation tokens, not prose. Keep the removal
    # syntax narrow so ordinary slash and hyphen prose remains visible to _WORD.
    readable_text = _MARKDOWN_INLINE_LINK.sub(r"\g<label>", text)
    readable_text = _MARKDOWN_REFERENCE_LINK.sub(r"\g<label>", readable_text)
    for pattern in (
        _MARKDOWN_REFERENCE_DEFINITION,
        _BARE_DOCUMENT_PATH,
        _BARE_DOCUMENT_NAME,
    ):
        readable_text = pattern.sub(" ", readable_text)
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
        if new_count >= WORD_COUNT_WARNING_HIGH_WATERMARK:
            threshold = WORD_COUNT_WARNING_HIGH_DELTA
        elif new_count >= WORD_COUNT_WARNING_LOW_WATERMARK:
            threshold = WORD_COUNT_WARNING_MIDDLE_DELTA
        else:
            threshold = WORD_COUNT_WARNING_LOW_DELTA
        if new_count < WORD_COUNT_HARD_LIMIT and delta >= threshold:
            remaining = WORD_COUNT_HARD_LIMIT - new_count
            diagnostics.append(
                f"warning: {path}: {old_count} -> {new_count} (+{delta}), "
                f"limit {WORD_COUNT_HARD_LIMIT}, {remaining} remaining; "
                f"{WARNING_POLICY_DESCRIPTION}"
            )
    return tuple(diagnostics)
