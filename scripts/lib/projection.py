"""Marked-region projection: compare a rendered document against disk, report drift.

A projection is a region of a documentation file derived from a source of truth
elsewhere in the repository. The region sits between HTML comment markers
(`<!-- name:start -->` / `<!-- name:end -->`), the caller renders the document
from the source, and this module owns the compare, the drift report and the
write. The markers are load-bearing: a document whose expected region is absent
fails loudly, because deleting a marker must not opt a document out of its
drift gate.
"""

from __future__ import annotations

import difflib
from pathlib import Path


class ProjectionError(ValueError):
    """The projection cannot be established; callers report it and fail."""


def split_marked_region(text: str, marker: str, name: str = "document") -> tuple[str, str, str]:
    """Split text around its marked region.

    Returns (head, region, tail) where head ends with the start marker and tail
    begins with the end marker, so ``head + region + tail == text``. Exactly one
    ordered pair is required: zero pairs leaves the drift gate nothing to own,
    and a second pair would sit outside the rendered region and drift unchecked.
    """
    start_marker = f"<!-- {marker}:start -->"
    end_marker = f"<!-- {marker}:end -->"
    starts, ends = text.count(start_marker), text.count(end_marker)
    if starts != 1 or ends != 1:
        raise ProjectionError(
            f"{name} needs exactly one {start_marker} / {end_marker} pair, "
            f"found {starts} start and {ends} end markers"
        )
    start, end = text.find(start_marker), text.find(end_marker)
    if end < start:
        raise ProjectionError(f"{name} has {end_marker} before {start_marker}")
    return text[: start + len(start_marker)], text[start + len(start_marker) : end], text[end:]


def project(path: Path, marker: str, rendered_text: str, check: bool, *, source: str | None = None) -> list[str]:
    """Compare the document at path against its rendered form, and report drift.

    ``rendered_text`` is the caller's full rendered document. Both it and the
    text on disk must contain the marked region; a missing region raises, it is
    never skipped. The return value is the unified diff from the on-disk text
    to the rendered text; an empty list means the document matches its source.
    With ``check`` true nothing is written; otherwise a drifted document is
    rewritten in place. ``source`` names the source of truth in the report.
    """
    current = path.read_text()
    split_marked_region(current, marker, path.name)
    split_marked_region(rendered_text, marker, f"rendered {path.name}")
    if rendered_text == current:
        return []
    rendered_label = f"(rendered from {source})" if source else "(rendered)"
    report = list(
        difflib.unified_diff(
            current.splitlines(keepends=True),
            rendered_text.splitlines(keepends=True),
            fromfile=f"{path.name} (on disk)",
            tofile=f"{path.name} {rendered_label}",
        )
    )
    if not check:
        path.write_text(rendered_text)
    return report
