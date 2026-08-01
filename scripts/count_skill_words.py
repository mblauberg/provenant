"""The single word-counting policy used for skill budget checks."""

from __future__ import annotations

import re
from pathlib import Path


WORD_COUNT_WARNING_THRESHOLD = 460
WORD_COUNT_HARD_LIMIT = 500
COUNTER_DESCRIPTION = (
    "Unified counter (whole file including frontmatter; markdown link URLs excluded)"
)
_MARKDOWN_LINK_URL = re.compile(r"\]\([^)]*\)")
_WORD = re.compile(r"\b[\w'-]+\b")


def count_skill_words(text: str) -> int:
    """Count the words users read, including frontmatter but excluding link URLs."""
    # Count the whole file because frontmatter is user-visible skill content. Strip
    # only markdown link destinations because they are navigation metadata, not
    # reading burden; retain the link text itself.
    readable_text = _MARKDOWN_LINK_URL.sub("]", text)
    return len(_WORD.findall(readable_text))


def word_count_diagnostics(path: Path) -> tuple[str, ...]:
    """Return a warning or hard-limit error for a skill file, if applicable."""
    word_count = count_skill_words(path.read_text())
    prefix = f"{path}: whole file has {word_count} words"
    limits = (
        f"warning threshold is {WORD_COUNT_WARNING_THRESHOLD} and "
        f"hard limit is {WORD_COUNT_HARD_LIMIT}. {COUNTER_DESCRIPTION}."
    )
    if word_count > WORD_COUNT_HARD_LIMIT:
        return (f"error: {prefix}; {limits}",)
    if WORD_COUNT_WARNING_THRESHOLD <= word_count < WORD_COUNT_HARD_LIMIT:
        return (f"warning: {prefix}; {limits}",)
    return ()
