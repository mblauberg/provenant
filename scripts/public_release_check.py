"""Fail closed on common mistakes before this harness is published."""

from __future__ import annotations

import argparse
import codecs
import hashlib
import os
import re
import select
import subprocess
import sys
import threading
import time
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Iterator, Mapping

try:
    from scripts.git_evidence import sanitized_git_environment
except ModuleNotFoundError:  # Direct `python scripts/public_release_check.py`.
    from git_evidence import sanitized_git_environment


ROOT = Path(__file__).resolve().parents[1]
REQUIRED_LEGAL_FILES = frozenset({
    "LICENSE",
    "NOTICE",  # Apache-2.0 §4(d) attribution aggregate (Epic #124 Workstream E)
    "THIRD_PARTY_NOTICES.md",
    "LICENSES/grill-me-pocock-MIT.txt",
    "LICENSES/impeccable-APACHE-2.0.txt",
    "LICENSES/modern-screenshot-MIT.txt",
    "LICENSES/playwright-cli-APACHE-2.0.txt",
    "LICENSES/skill-optimizer-MIT.txt",
    "LICENSES/typescript-clean-code-bmad-MIT.txt",
    "LICENSES/ui-ux-pro-max-MIT.txt",
    "LICENSES/vercel-react-best-practices-MIT.txt",
})
REQUIRED_PUBLIC_FILES = frozenset({
    "ACKNOWLEDGEMENTS.md",
    "README.md",
    "MAINTAINING.md",
    "SECURITY.md",
    "docs/ARCHITECTURE.md",
    "docs/worktrees.md",
})
REQUIRED = REQUIRED_LEGAL_FILES | REQUIRED_PUBLIC_FILES
FORBIDDEN_TRACKED = {
    ".DS_Store",
    ".claude/settings.local.json",
    "plugins/marketplace.json",
}
FORBIDDEN_PREFIXES = (
    ".agent-run/",
    ".worktrees/",
    ".pytest_cache/",
    "skills/clean-writing/",  # rejected interim name; capability is natural-writing
    "skills/humanise-text/",  # retired name; capability moved to natural-writing
    "skills/tanstack-query-best-practices/",  # retired local skill must not return
    "skills/vercel-react-best-practices/",  # retired vendor-branded skill name
)
HOME_PATH = re.compile(r"/(?:Users|home)/[A-Za-z0-9._-]+/")
SECRET_PATTERNS = {
    "private key": re.compile(r"-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----"),
    "GitHub token": re.compile(r"\b(?:gh[opusr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,})\b"),
    "OpenAI key": re.compile(r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b"),
    "Anthropic key": re.compile(r"\bsk-ant-[A-Za-z0-9_-]{20,}\b"),
    "AWS access key": re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
}
PERSONAL_EMAIL = re.compile(r"@(?:gmail|outlook|hotmail|icloud|yahoo)\.[A-Za-z.]+$", re.I)
BARE_PYTHON_COMMAND = re.compile(r"(?<![A-Za-z0-9_./-])python(?:3)?(?![A-Za-z0-9_.-])")
SHELL_SUFFIXES = {"", ".sh", ".bash", ".zsh", ".ksh", ".csh", ".fish"}
HOME_PATH_BYTES = re.compile(HOME_PATH.pattern.encode("ascii"))
SECRET_BYTE_PATTERNS = {
    label: re.compile(pattern.pattern.encode("ascii"))
    for label, pattern in SECRET_PATTERNS.items()
}

MAX_PUBLIC_FILE_BYTES = 5 * 1024 * 1024
BLOB_STREAM_CHUNK_SIZE = 64 * 1024
BLOB_SCAN_OVERLAP = 64
MAX_BATCH_HEADER_BYTES = 192
MAX_BATCH_STDERR_BYTES = 64 * 1024
BATCH_MAX_SECONDS = 300.0
MAX_TREE_COMPONENT_BYTES = 255
# RFC 5321's 256-octet forward-path limit includes the surrounding angle brackets.
MAX_IDENTITY_EMAIL_BYTES = 254

_ASCII_WORD_BYTES = frozenset(
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_"
)
_HOME_COMPONENT_BYTES = frozenset(
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-"
)
_GITHUB_TOKEN_BYTES = _ASCII_WORD_BYTES
_DASH_TOKEN_BYTES = frozenset((*_ASCII_WORD_BYTES, ord("-")))
_AWS_TOKEN_BYTES = frozenset(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789")


@dataclass
class EvidenceMemoryAccounting:
    """Deterministic accounting for the bounded evidence-reader contract."""

    batch_processes_started: int = 0
    batch_processes_completed: int = 0
    objects_requested: int = 0
    objects_completed: int = 0
    announced_body_bytes: int = 0
    consumed_body_bytes: int = 0
    max_body_chunk_bytes: int = 0
    max_scanner_carry_bytes: int = 0
    max_resident_body_bytes: int = 0
    retained_full_blob_bodies: int = 0
    retained_full_nonblob_bodies: int = 0
    max_flattened_trees_retained: int = 0
    non_head_flattened_entries_retained: int = 0
    distinct_root_trees_walked: int = 0
    distinct_blobs_scanned: int = 0
    blob_objects_completed: int = 0
    commit_objects_completed: int = 0
    tag_objects_completed: int = 0
    tree_objects_completed: int = 0
    max_tree_entry_bytes: int = 0
    max_tree_object_summary_bytes: int = 0
    max_commit_summary_bytes: int = 0
    retained_commit_summary_bytes: int = 0
    max_tag_summary_bytes: int = 0
    max_identity_email_bytes_retained: int = 0
    max_pending_tree_walk_items: int = 0
    max_tree_path_nodes_retained: int = 0
    max_tree_path_component_bytes_retained: int = 0
    max_materialized_tree_path_bytes: int = 0
    max_batch_stderr_bytes_retained: int = 0
    batch_stderr_bytes_discarded: int = 0
    batch_stderr_drainers_started: int = 0
    batch_stderr_drainers_joined: int = 0
    retained_tree_context_summaries: int = 0
    historical_tree_contexts_released_before_head: bool = False
    head_flattened_entries_retained: int = 0
    max_required_head_entries_retained: int = 0
    oversized_head_blobs: int = 0
    oversized_historical_blobs: int = 0
    _flattened_trees_retained: int = 0

    def observe_body_buffer(self, capacity: int) -> None:
        self.max_resident_body_bytes = max(
            self.max_resident_body_bytes, capacity,
        )

    def retain_flattened_tree(self) -> None:
        self._flattened_trees_retained += 1
        self.max_flattened_trees_retained = max(
            self.max_flattened_trees_retained,
            self._flattened_trees_retained,
        )

    def release_flattened_tree(self) -> None:
        self._flattened_trees_retained -= 1
        if self._flattened_trees_retained < 0:
            raise RuntimeError("publication tree accounting underflow")


@dataclass(frozen=True)
class BlobEvidence:
    size: int
    findings: frozenset[str]


OID = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})")


@dataclass(frozen=True)
class RawCommit:
    object_id: str
    tree: str
    parents: tuple[str, ...]
    message_findings: frozenset[str]
    author_email: str
    committer_email: str


@dataclass(frozen=True)
class RawTag:
    target: str
    declared_type: str
    tagger_email: str
    message_findings: frozenset[str]


@dataclass(frozen=True)
class RawTreeItem:
    mode: str
    kind: str
    object_id: str
    name: str


def _decode_git_error(raw: bytes, fallback: str) -> str:
    return raw.decode("utf-8", errors="replace").strip() or fallback


def _whole_content_findings(raw: bytes) -> frozenset[str]:
    findings: set[str] = set()
    if HOME_PATH_BYTES.search(raw):
        findings.add("personal absolute home path")
    for label, pattern in SECRET_BYTE_PATTERNS.items():
        if pattern.search(raw):
            findings.add(f"possible {label}")
    return frozenset(findings)


def _tree_name_order(
    left: bytes | bytearray,
    left_kind: str,
    right: bytes | bytearray,
    right_kind: str,
) -> int:
    """Compare tree names using Git's base-name ordering without building keys."""
    common = min(len(left), len(right))
    for index in range(common):
        if left[index] != right[index]:
            return -1 if left[index] < right[index] else 1
    left_next = (
        left[common]
        if common < len(left)
        else ord("/") if left_kind == "tree" else 0
    )
    right_next = (
        right[common]
        if common < len(right)
        else ord("/") if right_kind == "tree" else 0
    )
    return (left_next > right_next) - (left_next < right_next)


def parse_raw_tree(raw: bytes, raw_object_id_size: int) -> tuple[RawTreeItem, ...]:
    items: list[RawTreeItem] = []
    previous_name: bytes | None = None
    previous_kind = ""
    position = 0
    mode_kinds = {
        "40000": "tree",
        "100644": "blob",
        "100755": "blob",
        "120000": "blob",
        "160000": "commit",
    }
    while position < len(raw):
        mode_end = raw.find(b" ", position)
        name_end = raw.find(b"\0", mode_end + 1)
        if mode_end <= position or name_end <= mode_end + 1:
            raise RuntimeError("publication tree contains an unparseable entry")
        object_end = name_end + 1 + raw_object_id_size
        if object_end > len(raw):
            raise RuntimeError("publication tree contains a truncated object id")
        raw_name = raw[mode_end + 1:name_end]
        try:
            mode = raw[position:mode_end].decode("ascii")
            name = raw_name.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise RuntimeError("publication tree contains an undecodable entry") from exc
        kind = mode_kinds.get(mode)
        if (
            kind is None
            or not name
            or name in {".", ".."}
            or "/" in name
            or len(raw_name) > MAX_TREE_COMPONENT_BYTES
            or previous_name == raw_name
            or (
                previous_name is not None
                and _tree_name_order(
                    previous_name, previous_kind, raw_name, kind,
                ) >= 0
            )
        ):
            raise RuntimeError("publication tree contains an invalid entry")
        items.append(
            RawTreeItem(
                mode=mode,
                kind=kind,
                object_id=raw[name_end + 1:object_end].hex(),
                name=name,
            )
        )
        previous_name = raw_name
        previous_kind = kind
        position = object_end
    return tuple(items)


@dataclass(frozen=True)
class _TokenSpec:
    label: str
    prefix: bytes
    allowed: frozenset[int]
    minimum: int
    maximum: int | None = None


_TOKEN_SPECS = (
    *(
        _TokenSpec("GitHub token", prefix, _GITHUB_TOKEN_BYTES, 20)
        for prefix in (b"gho_", b"ghp_", b"ghu_", b"ghs_", b"ghr_", b"github_pat_")
    ),
    _TokenSpec("OpenAI key", b"sk-", _DASH_TOKEN_BYTES, 20),
    _TokenSpec("Anthropic key", b"sk-ant-", _DASH_TOKEN_BYTES, 20),
    _TokenSpec("AWS access key", b"AKIA", _AWS_TOKEN_BYTES, 16, 16),
)
_TOKEN_PREFIX_PATTERNS = tuple(
    re.compile(re.escape(spec.prefix)) for spec in _TOKEN_SPECS
)
_TOKEN_RUN_PATTERNS = tuple(
    re.compile(b"[" + re.escape(bytes(sorted(spec.allowed))) + b"]*")
    for spec in _TOKEN_SPECS
)
_HOME_PREFIXES = (b"/Users/", b"/home/")
_PRIVATE_KEY_LITERALS = tuple(
    b"-----BEGIN " + prefix + b"PRIVATE KEY-----"
    for prefix in (b"", b"RSA ", b"EC ", b"OPENSSH ", b"DSA ")
)
_HOME_RUN_PATTERN = re.compile(
    b"[" + re.escape(bytes(sorted(_HOME_COMPONENT_BYTES))) + b"]*"
)
_WORD_HYPHEN_PATTERN = re.compile(rb"[A-Za-z0-9_]-")


class _BoundedContentScanner:
    """Match release-policy content without retaining an unbounded token tail."""

    def __init__(self) -> None:
        self.findings: set[str] = set()
        self._tail = bytearray()
        self._seen = 0
        self._home_active = False
        self._home_has_component = False
        self._token_counts: list[int | None] = [None] * len(_TOKEN_SPECS)
        self._token_last_word = [False] * len(_TOKEN_SPECS)
        self.chunks_scanned = 0
        self.active_candidate_chunks = 0

    def feed(self, raw: memoryview) -> None:
        if not raw:
            return
        self.chunks_scanned += 1
        prior_tail = self._tail
        if (
            "personal absolute home path" not in self.findings
            and HOME_PATH_BYTES.search(raw) is not None
        ):
            self.findings.add("personal absolute home path")
        for label, pattern in SECRET_BYTE_PATTERNS.items():
            finding = f"possible {label}"
            if finding in self.findings:
                continue
            if label in {"private key"}:
                matched = pattern.search(raw) is not None
                if not matched:
                    for literal in _PRIVATE_KEY_LITERALS:
                        for split in range(1, len(literal)):
                            suffix = literal[split:]
                            if (
                                len(raw) >= len(suffix)
                                and prior_tail.endswith(literal[:split])
                                and all(
                                    raw[index] == value
                                    for index, value in enumerate(suffix)
                                )
                            ):
                                matched = True
                                break
                        if matched:
                            break
            else:
                matched = False
                for match in pattern.finditer(raw):
                    if match.end() >= len(raw):
                        continue
                    if (
                        match.start() == 0 and prior_tail
                        and prior_tail[-1] in _ASCII_WORD_BYTES
                    ):
                        continue
                    matched = True
                    break
            if matched:
                self.findings.add(finding)

        self._advance_active_candidates(raw)
        self._activate_home_candidate(raw, prior_tail)
        for index in range(len(_TOKEN_SPECS)):
            self._activate_token_candidate(index, raw, prior_tail)

        if len(raw) >= BLOB_SCAN_OVERLAP:
            self._tail[:] = raw[-BLOB_SCAN_OVERLAP:]
        else:
            self._tail.extend(raw)
            if len(self._tail) > BLOB_SCAN_OVERLAP:
                del self._tail[:-BLOB_SCAN_OVERLAP]
        self._seen += len(raw)

    @staticmethod
    def _candidate_matches(spec: _TokenSpec, count: int, last_word: bool) -> bool:
        if spec.maximum is None:
            return count >= spec.minimum and last_word
        return count == spec.maximum and last_word

    @staticmethod
    def _bounded_candidate_count(spec: _TokenSpec, count: int) -> int:
        limit = spec.minimum if spec.maximum is None else spec.maximum + 1
        return min(count, limit)

    def _retain_token_candidate(
        self, index: int, count: int, last_word: bool,
    ) -> None:
        spec = _TOKEN_SPECS[index]
        bounded = self._bounded_candidate_count(spec, count)
        if spec.maximum is not None and bounded > spec.maximum:
            return
        current = self._token_counts[index]
        if current is None or bounded > current:
            self._token_counts[index] = bounded
            self._token_last_word[index] = last_word
        elif bounded == current:
            self._token_last_word[index] = (
                self._token_last_word[index] or last_word
            )

    @staticmethod
    def _dash_boundary_in_run(
        spec: _TokenSpec,
        raw: memoryview,
        start: int,
        end: int,
        initial_count: int,
        initial_last_word: bool,
    ) -> bool:
        if spec.allowed != _DASH_TOKEN_BYTES or start >= end:
            return False
        if (
            raw[start] == ord("-")
            and initial_count >= spec.minimum
            and initial_last_word
        ):
            return True
        threshold = start + max(0, spec.minimum - initial_count)
        search_start = max(start, threshold - 1)
        match = _WORD_HYPHEN_PATTERN.search(raw, search_start, end)
        return match is not None and match.end() - 1 >= threshold

    def _advance_active_candidates(self, raw: memoryview) -> None:
        for index, spec in enumerate(_TOKEN_SPECS):
            count = self._token_counts[index]
            if count is None or f"possible {spec.label}" in self.findings:
                continue
            self.active_candidate_chunks += 1
            end = _TOKEN_RUN_PATTERNS[index].match(raw, 0, len(raw)).end()
            if self._dash_boundary_in_run(
                spec,
                raw,
                0,
                end,
                count,
                self._token_last_word[index],
            ):
                self.findings.add(f"possible {spec.label}")
            if end:
                count += end
                self._token_last_word[index] = raw[end - 1] in _ASCII_WORD_BYTES
            bounded_count = self._bounded_candidate_count(spec, count)
            self._token_counts[index] = bounded_count
            if end == len(raw):
                if spec.maximum is not None and count > spec.maximum:
                    self._token_counts[index] = None
                continue
            if (
                self._candidate_matches(
                    spec, count, self._token_last_word[index],
                )
                and raw[end] not in _ASCII_WORD_BYTES
            ):
                self.findings.add(f"possible {spec.label}")
            self._token_counts[index] = None

        if self._home_active and "personal absolute home path" not in self.findings:
            self.active_candidate_chunks += 1
            end = _HOME_RUN_PATTERN.match(raw, 0, len(raw)).end()
            if end:
                self._home_has_component = True
            if end == len(raw):
                return
            if raw[end] == ord("/") and self._home_has_component:
                self.findings.add("personal absolute home path")
            self._home_active = False
            self._home_has_component = False

    def _activate_home_candidate(
        self, raw: memoryview, prior_tail: bytearray,
    ) -> None:
        if "personal absolute home path" in self.findings:
            return
        for prefix in _HOME_PREFIXES:
            for match in re.finditer(re.escape(prefix), raw):
                end = _HOME_RUN_PATTERN.match(raw, match.end(), len(raw)).end()
                if end == len(raw):
                    self._home_active = True
                    self._home_has_component = end > match.end()
            for split in range(1, len(prefix)):
                if (
                    prior_tail.endswith(prefix[:split])
                    and len(raw) >= len(prefix) - split
                    and all(
                        raw[index] == value
                        for index, value in enumerate(prefix[split:])
                    )
                ):
                    end = _HOME_RUN_PATTERN.match(
                        raw, len(prefix) - split, len(raw),
                    ).end()
                    if end == len(raw):
                        self._home_active = True
                        self._home_has_component = end > len(prefix) - split
                    elif (
                        end > len(prefix) - split and raw[end] == ord("/")
                    ):
                        self.findings.add("personal absolute home path")

    def _activate_token_candidate(
        self, index: int, raw: memoryview, prior_tail: bytearray,
    ) -> None:
        spec = _TOKEN_SPECS[index]
        if f"possible {spec.label}" in self.findings:
            return
        pattern = _TOKEN_PREFIX_PATTERNS[index]
        run = _TOKEN_RUN_PATTERNS[index]
        for match in pattern.finditer(raw):
            start = match.start()
            previous = raw[start - 1] if start else (
                prior_tail[-1] if prior_tail else None
            )
            if previous is not None and previous in _ASCII_WORD_BYTES:
                continue
            end = run.match(raw, match.end(), len(raw)).end()
            count = end - match.end()
            last_word = end > match.end() and raw[end - 1] in _ASCII_WORD_BYTES
            if self._dash_boundary_in_run(
                spec, raw, match.end(), end, 0, False,
            ):
                self.findings.add(f"possible {spec.label}")
            if end == len(raw):
                self._retain_token_candidate(index, count, last_word)
            elif (
                self._candidate_matches(spec, count, last_word)
                and raw[end] not in _ASCII_WORD_BYTES
            ):
                self.findings.add(f"possible {spec.label}")
        prefix = spec.prefix
        for split in range(1, len(prefix)):
            suffix_length = len(prefix) - split
            if (
                not prior_tail.endswith(prefix[:split])
                or len(raw) < suffix_length
                or any(
                    raw[position] != value
                    for position, value in enumerate(prefix[split:])
                )
            ):
                continue
            start = len(prior_tail) - split
            previous = prior_tail[start - 1] if start else (
                None if self._seen == len(prior_tail) else None
            )
            if previous is not None and previous in _ASCII_WORD_BYTES:
                continue
            end = run.match(raw, suffix_length, len(raw)).end()
            if self._dash_boundary_in_run(
                spec, raw, suffix_length, end, 0, False,
            ):
                self.findings.add(f"possible {spec.label}")
            if end == len(raw):
                self._retain_token_candidate(
                    index,
                    end - suffix_length,
                    end > suffix_length and raw[end - 1] in _ASCII_WORD_BYTES,
                )
            elif (
                self._candidate_matches(
                    spec,
                    end - suffix_length,
                    end > suffix_length and raw[end - 1] in _ASCII_WORD_BYTES,
                )
                and raw[end] not in _ASCII_WORD_BYTES
            ):
                self.findings.add(f"possible {spec.label}")

    def finish(self) -> frozenset[str]:
        for index, spec in enumerate(_TOKEN_SPECS):
            count = self._token_counts[index]
            if count is None:
                continue
            if self._candidate_matches(
                spec, count, self._token_last_word[index],
            ):
                self.findings.add(f"possible {spec.label}")
        return frozenset(self.findings)


class _DiscardObjectConsumer:
    def feed(self, raw: memoryview) -> None:
        return None

    def finish(self) -> None:
        return None


class _BlobObjectConsumer:
    def __init__(self) -> None:
        self.scanner = _BoundedContentScanner()

    def feed(self, raw: memoryview) -> None:
        self.scanner.feed(raw)

    def finish(self) -> frozenset[str]:
        return self.scanner.finish()


class _IdentityValueParser:
    def __init__(
        self, *, retain_email: bool, accounting: EvidenceMemoryAccounting,
    ) -> None:
        self.retain_email = retain_email
        self.accounting = accounting
        self.state = "name"
        self.previous: int | None = None
        self.email = bytearray()
        self.email_decoder = codecs.getincrementaldecoder("utf-8")("strict")
        self.email_count = 0
        self.timestamp_digits = 0
        self.timezone_digits = 0

    def feed(self, value: int) -> None:
        if self.state == "name":
            if value in (ord("\r"), ord("\n"), ord(">")):
                raise ValueError
            if value == ord("<"):
                if self.previous != ord(" "):
                    raise ValueError
                self.state = "email"
                return
            self.previous = value
            return
        if self.state == "email":
            if value == ord(">"):
                if not self.email_count:
                    raise ValueError
                self.email_decoder.decode(b"", final=True)
                self.state = "email-space"
                return
            if value in (ord("\r"), ord("\n"), ord("<")):
                raise ValueError
            if self.email_count >= MAX_IDENTITY_EMAIL_BYTES:
                raise ValueError
            raw = bytes((value,))
            self.email_decoder.decode(raw, final=False)
            if self.retain_email:
                self.email.append(value)
                self.accounting.max_identity_email_bytes_retained = max(
                    self.accounting.max_identity_email_bytes_retained,
                    len(self.email),
                )
            self.email_count += 1
            return
        if self.state == "email-space":
            if value != ord(" "):
                raise ValueError
            self.state = "timestamp"
            return
        if self.state == "timestamp":
            if ord("0") <= value <= ord("9"):
                self.timestamp_digits += 1
                return
            if value == ord(" ") and self.timestamp_digits:
                self.state = "timezone-sign"
                return
            raise ValueError
        if self.state == "timezone-sign":
            if value not in (ord("+"), ord("-")):
                raise ValueError
            self.state = "timezone"
            return
        if self.state == "timezone":
            if not ord("0") <= value <= ord("9") or self.timezone_digits >= 4:
                raise ValueError
            self.timezone_digits += 1
            return
        raise ValueError

    def finish(self) -> str:
        if self.state != "timezone" or self.timezone_digits != 4:
            raise ValueError
        if not self.retain_email:
            return ""
        return self.email.decode("utf-8")


class _CommitObjectConsumer:
    _CAPTURED = {b"tree", b"parent", b"author", b"committer"}

    def __init__(self, object_id: str, accounting: EvidenceMemoryAccounting) -> None:
        self.object_id = object_id
        self.accounting = accounting
        self.in_headers = True
        self.boundary_seen = False
        self.line_length = 0
        self.continuation = False
        self.name = bytearray()
        self.name_too_long = False
        self.saw_space = False
        self.value_nonempty = False
        self.value = bytearray()
        self.identity: _IdentityValueParser | None = None
        self.previous_name: bytes | None = None
        self.trees: list[str] = []
        self.tree_headers = 0
        self.parents: dict[str, None] = {}
        self.authors: list[str] = []
        self.author_headers = 0
        self.committer_emails: list[str] = []
        self.committers = 0
        self.scanner = _BoundedContentScanner()
        self.summary_bytes = 0

    def _reset_line(self) -> None:
        self.line_length = 0
        self.continuation = False
        self.name.clear()
        self.name_too_long = False
        self.saw_space = False
        self.value_nonempty = False
        self.value.clear()
        self.identity = None

    def _field(self) -> bytes | None:
        if self.name_too_long:
            return None
        name = bytes(self.name)
        return name if name in self._CAPTURED else None

    def _message_error(self, field: bytes | None = None) -> RuntimeError:
        if field == b"tree":
            return RuntimeError(
                f"publication commit {self.object_id} has an invalid tree"
            )
        if field == b"parent":
            return RuntimeError(
                f"publication commit {self.object_id} has an invalid parent"
            )
        if field in {b"author", b"committer"}:
            return RuntimeError(
                f"publication commit {self.object_id} has an invalid author"
            )
        return RuntimeError(
            f"publication commit {self.object_id} has an invalid header"
        )

    def _finish_line(self) -> None:
        if self.continuation:
            if self.previous_name is None:
                raise RuntimeError(
                    f"publication commit {self.object_id} has an orphan "
                    "header continuation"
                )
            if self.previous_name in self._CAPTURED:
                raise self._message_error(self.previous_name)
            return
        if not self.saw_space or not self.value_nonempty:
            raise self._message_error()
        field = self._field()
        self.previous_name = bytes(self.name) if not self.name_too_long else b""
        if field in {b"tree", b"parent"}:
            try:
                value = self.value.decode("ascii")
            except UnicodeDecodeError as exc:
                raise self._message_error(field) from exc
            if OID.fullmatch(value) is None:
                raise self._message_error(field)
            if field == b"tree":
                self.tree_headers += 1
                if self.tree_headers == 1:
                    self.trees.append(value)
            else:
                if value in self.parents:
                    raise RuntimeError(
                        f"publication commit {self.object_id} has a duplicate parent"
                    )
                self.parents[value] = None
        elif field in {b"author", b"committer"}:
            assert self.identity is not None
            try:
                email = self.identity.finish()
            except (UnicodeDecodeError, ValueError) as exc:
                raise self._message_error(field) from exc
            if field == b"author":
                self.author_headers += 1
                if self.author_headers == 1:
                    self.authors.append(email)
            else:
                self.committers += 1
                if self.committers == 1:
                    self.committer_emails.append(email)

    def _feed_header_byte(self, value: int) -> None:
        if self.line_length == 0 and value == ord(" "):
            self.continuation = True
            self.line_length = 1
            return
        self.line_length += 1
        if self.continuation:
            return
        if not self.saw_space:
            if value == ord(" "):
                if not self.name or self.name_too_long:
                    if not self.name_too_long:
                        raise self._message_error()
                self.saw_space = True
                field = self._field()
                if field in {b"author", b"committer"}:
                    self.identity = _IdentityValueParser(
                        retain_email=True, accounting=self.accounting,
                    )
                return
            if not (
                ord("A") <= value <= ord("Z")
                or ord("a") <= value <= ord("z")
                or ord("0") <= value <= ord("9")
                or value == ord("-")
            ):
                raise self._message_error()
            if len(self.name) <= max(len(name) for name in self._CAPTURED):
                self.name.append(value)
            else:
                self.name_too_long = True
            return
        self.value_nonempty = True
        field = self._field()
        if field in {b"tree", b"parent"}:
            if len(self.value) <= 64:
                self.value.append(value)
        elif field in {b"author", b"committer"}:
            assert self.identity is not None
            try:
                self.identity.feed(value)
            except (UnicodeDecodeError, ValueError) as exc:
                raise self._message_error(field) from exc

    def feed(self, raw: memoryview) -> None:
        position = 0
        continuation_start = 0 if self.continuation else None
        if self.in_headers:
            while position < len(raw):
                value = raw[position]
                position += 1
                if value != ord("\n"):
                    self._feed_header_byte(value)
                    if self.continuation and continuation_start is None:
                        continuation_start = position - 1
                    continue
                if self.line_length == 0:
                    self.in_headers = False
                    self.boundary_seen = True
                    break
                if continuation_start is not None:
                    self.scanner.feed(raw[continuation_start:position - 1])
                    self.scanner.feed(memoryview(b"\n"))
                    continuation_start = None
                self._finish_line()
                self._reset_line()
        if self.in_headers and continuation_start is not None:
            self.scanner.feed(raw[continuation_start:position])
        if not self.in_headers and position < len(raw):
            self.scanner.feed(raw[position:])

    def finish(self) -> RawCommit:
        if not self.boundary_seen:
            raise RuntimeError(
                f"publication commit {self.object_id} has no message boundary"
            )
        for required, values in (
            ("tree", self.tree_headers), ("author", self.author_headers),
        ):
            if values != 1:
                raise RuntimeError(
                    f"publication commit {self.object_id} must contain exactly "
                    f"one {required} header"
                )
        if self.committers != 1:
            raise RuntimeError(
                f"publication commit {self.object_id} must contain exactly one "
                "committer header"
            )
        self.summary_bytes = (
            len(self.trees[0]) + sum(map(len, self.parents))
            + len(self.authors[0].encode("utf-8"))
            + len(self.committer_emails[0].encode("utf-8"))
        )
        self.accounting.max_commit_summary_bytes = max(
            self.accounting.max_commit_summary_bytes, self.summary_bytes,
        )
        return RawCommit(
            object_id=self.object_id,
            tree=self.trees[0],
            parents=tuple(self.parents),
            message_findings=self.scanner.finish(),
            author_email=self.authors[0],
            committer_email=self.committer_emails[0],
        )


class _TagObjectConsumer:
    _CAPTURED = {b"object", b"type"}
    _REQUIRED = {b"object", b"type", b"tag", b"tagger"}
    _LINE_BREAKS = frozenset((10, 13))

    def __init__(
        self, object_id: str, accounting: EvidenceMemoryAccounting,
    ) -> None:
        self.object_id = object_id
        self.accounting = accounting
        self.in_headers = True
        self.boundary_seen = False
        self.raw_previous_lf = False
        self.previous_break_was_cr = False
        self.line_length = 0
        self.continuation = False
        self.name = bytearray()
        self.name_too_long = False
        self.saw_space = False
        self.value_nonempty = False
        self.value = bytearray()
        self.identity: _IdentityValueParser | None = None
        self.previous_name: bytes | None = None
        self.counts = {name: 0 for name in self._REQUIRED}
        self.values = {name: [] for name in self._CAPTURED}
        self.tagger_emails: list[str] = []
        self.scanner = _BoundedContentScanner()

    def _error(self) -> RuntimeError:
        return RuntimeError(f"publication tag {self.object_id} is malformed")

    def _reset_line(self) -> None:
        self.line_length = 0
        self.continuation = False
        self.name.clear()
        self.name_too_long = False
        self.saw_space = False
        self.value_nonempty = False
        self.value.clear()
        self.identity = None

    def _feed_byte(self, value: int) -> None:
        if self.line_length == 0 and value == ord(" "):
            self.continuation = True
            self.line_length = 1
            return
        self.line_length += 1
        if self.continuation:
            return
        if not self.saw_space:
            if value == ord(" "):
                if not self.name or self.name_too_long:
                    if not self.name_too_long:
                        raise self._error()
                self.saw_space = True
                if not self.name_too_long and bytes(self.name) == b"tagger":
                    self.identity = _IdentityValueParser(
                        retain_email=True, accounting=self.accounting,
                    )
                return
            if not (
                ord("A") <= value <= ord("Z")
                or ord("a") <= value <= ord("z")
                or ord("0") <= value <= ord("9")
                or value == ord("-")
            ):
                raise self._error()
            if len(self.name) <= max(len(name) for name in self._REQUIRED):
                self.name.append(value)
            else:
                self.name_too_long = True
            return
        self.value_nonempty = True
        name = bytes(self.name) if not self.name_too_long else b""
        if name in self._CAPTURED:
            if len(self.value) <= 64:
                self.value.append(value)
        elif name == b"tagger":
            assert self.identity is not None
            try:
                self.identity.feed(value)
            except (UnicodeDecodeError, ValueError) as exc:
                raise self._error() from exc

    def _finish_line(self) -> None:
        if self.continuation:
            if (
                self.previous_name is None
                or self.previous_name in self._REQUIRED
            ):
                raise self._error()
            return
        if not self.saw_space or not self.value_nonempty:
            raise self._error()
        name = bytes(self.name) if not self.name_too_long else b""
        self.previous_name = name
        if name in self.counts:
            self.counts[name] += 1
        if name == b"tagger":
            assert self.identity is not None
            try:
                email = self.identity.finish()
            except (UnicodeDecodeError, ValueError) as exc:
                raise self._error() from exc
            if self.counts[name] == 1:
                self.tagger_emails.append(email)
        if name in self.values and self.counts[name] == 1:
            self.values[name].append(bytes(self.value))
        retained = (
            sum(len(values[0]) for values in self.values.values() if values)
            + sum(len(email.encode("utf-8")) for email in self.tagger_emails)
        )
        self.accounting.max_tag_summary_bytes = max(
            self.accounting.max_tag_summary_bytes, retained,
        )

    def feed(self, raw: memoryview) -> None:
        if not self.in_headers:
            self.scanner.feed(raw)
            return
        continuation_start = 0 if self.continuation else None
        for position, value in enumerate(raw):
            if self.raw_previous_lf and value == ord("\n"):
                self.in_headers = False
                self.boundary_seen = True
                if position + 1 < len(raw):
                    self.scanner.feed(raw[position + 1:])
                return
            self.raw_previous_lf = value == ord("\n")
            if self.previous_break_was_cr and value == ord("\n"):
                self.previous_break_was_cr = False
                continue
            self.previous_break_was_cr = False
            if value in self._LINE_BREAKS:
                if self.line_length == 0:
                    raise self._error()
                if continuation_start is not None:
                    self.scanner.feed(raw[continuation_start:position])
                    self.scanner.feed(memoryview(b"\n"))
                    continuation_start = None
                self._finish_line()
                self._reset_line()
                self.previous_break_was_cr = value == ord("\r")
            else:
                self._feed_byte(value)
                if self.continuation and continuation_start is None:
                    continuation_start = position
        if continuation_start is not None:
            self.scanner.feed(raw[continuation_start:])

    def finish(self) -> RawTag:
        if not self.boundary_seen or any(
            self.counts[required] != 1
            for required in self._REQUIRED
        ):
            raise self._error()
        try:
            target = self.values[b"object"][0].decode("ascii").lower()
            declared_type = self.values[b"type"][0].decode("ascii")
        except UnicodeDecodeError as exc:
            raise self._error() from exc
        if OID.fullmatch(target) is None:
            raise RuntimeError(
                f"publication tag {self.object_id} has an invalid target"
            )
        if declared_type not in {"blob", "commit", "tag", "tree"}:
            raise RuntimeError(
                f"publication tag {self.object_id} has an invalid target type"
            )
        return RawTag(
            target=target,
            declared_type=declared_type,
            tagger_email=self.tagger_emails[0],
            message_findings=self.scanner.finish(),
        )


class _TreeObjectConsumer:
    def __init__(
        self,
        raw_object_id_size: int,
        visitor: Callable[[RawTreeItem], None],
        accounting: EvidenceMemoryAccounting,
    ) -> None:
        self.raw_object_id_size = raw_object_id_size
        self.visitor = visitor
        self.accounting = accounting
        self.stage = "mode"
        self.mode = bytearray()
        self.name = bytearray()
        self.object_id = bytearray()
        self.previous_name: bytearray | None = None
        self.previous_kind = ""

    def _observe_summary(self) -> None:
        retained = len(self.name) + (
            len(self.previous_name) if self.previous_name is not None else 0
        )
        self.accounting.max_tree_object_summary_bytes = max(
            self.accounting.max_tree_object_summary_bytes, retained,
        )

    def _finish_entry(self) -> None:
        mode_kinds = {
            "40000": "tree", "100644": "blob", "100755": "blob",
            "120000": "blob", "160000": "commit",
        }
        try:
            mode = self.mode.decode("ascii")
            name = self.name.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise RuntimeError(
                "publication tree contains an undecodable entry"
            ) from exc
        kind = mode_kinds.get(mode)
        if (
            kind is None or not name or name in {".", ".."}
            or "/" in name
            or self.previous_name == self.name
            or (
                self.previous_name is not None
                and _tree_name_order(
                    self.previous_name, self.previous_kind, self.name, kind,
                ) >= 0
            )
        ):
            raise RuntimeError("publication tree contains an invalid entry")
        self._observe_summary()
        entry_bytes = (
            len(self.mode) + 1 + len(self.name) + 1 + len(self.object_id)
        )
        self.accounting.max_tree_entry_bytes = max(
            self.accounting.max_tree_entry_bytes, entry_bytes,
        )
        self.visitor(
            RawTreeItem(
                mode=mode,
                kind=kind,
                object_id=self.object_id.hex(),
                name=name,
            )
        )
        self.stage = "mode"
        self.previous_name = self.name
        self.previous_kind = kind
        self.mode.clear()
        self.name = bytearray()
        self.object_id.clear()

    def feed(self, raw: memoryview) -> None:
        for value in raw:
            if self.stage == "mode":
                if value == ord(" "):
                    if not self.mode:
                        raise RuntimeError(
                            "publication tree contains an unparseable entry"
                        )
                    self.stage = "name"
                elif len(self.mode) >= 6:
                    raise RuntimeError("publication tree contains an invalid entry")
                else:
                    self.mode.append(value)
                continue
            if self.stage == "name":
                if value == 0:
                    if not self.name:
                        raise RuntimeError(
                            "publication tree contains an unparseable entry"
                        )
                    self.stage = "object-id"
                else:
                    if len(self.name) >= MAX_TREE_COMPONENT_BYTES:
                        raise RuntimeError(
                            "publication tree contains an invalid entry"
                        )
                    self.name.append(value)
                    self._observe_summary()
                continue
            self.object_id.append(value)
            if len(self.object_id) == self.raw_object_id_size:
                self._finish_entry()

    def finish(self) -> None:
        if self.stage == "object-id":
            raise RuntimeError("publication tree contains a truncated object id")
        if self.stage != "mode" or self.mode:
            raise RuntimeError("publication tree contains an unparseable entry")
        return None


class _BoundedStderrDrain:
    """Drain a child pipe concurrently while retaining at most the policy cap."""

    _READ_SIZE = 16 * 1024

    def __init__(self, accounting: EvidenceMemoryAccounting) -> None:
        self.accounting = accounting
        self.buffer = bytearray()
        self.discarded = 0
        self.stream: object | None = None
        self.thread: threading.Thread | None = None
        self.stop = threading.Event()
        self.error: Exception | None = None
        self.joined = False

    def _retain(self, raw: bytes) -> None:
        remaining = MAX_BATCH_STDERR_BYTES - len(self.buffer)
        if remaining > 0:
            self.buffer.extend(raw[:remaining])
        discarded = len(raw) - max(0, remaining)
        if discarded > 0:
            self.discarded += discarded
            self.accounting.batch_stderr_bytes_discarded += discarded
        self.accounting.max_batch_stderr_bytes_retained = max(
            self.accounting.max_batch_stderr_bytes_retained,
            len(self.buffer),
        )

    def _drain_blocking_stream(self) -> None:
        assert self.stream is not None
        while True:
            raw = self.stream.read(self._READ_SIZE)  # type: ignore[attr-defined]
            if not raw:
                return
            self._retain(raw)

    def _drain_descriptor(self, descriptor: int) -> None:
        while True:
            readable, _, _ = select.select([descriptor], [], [], 0.05)
            if not readable:
                if self.stop.is_set():
                    return
                continue
            raw = os.read(descriptor, self._READ_SIZE)
            if not raw:
                return
            self._retain(raw)

    def _run(self) -> None:
        assert self.stream is not None
        try:
            try:
                descriptor = self.stream.fileno()  # type: ignore[attr-defined]
            except (AttributeError, OSError, ValueError):
                descriptor = None
            if descriptor is None:
                self._drain_blocking_stream()
            else:
                self._drain_descriptor(descriptor)
        except Exception as exc:  # any drain failure must reach the joining thread
            if not self.stop.is_set():
                self.error = exc

    def start(self, stream: object) -> None:
        self.stream = stream
        thread = threading.Thread(
            target=self._run,
            name="publication-stderr-drain",
            daemon=True,
        )
        thread.start()
        self.thread = thread
        self.accounting.batch_stderr_drainers_started += 1

    def stop_and_join(self) -> None:
        if self.thread is None or self.joined:
            return
        self.stop.set()
        self.thread.join(timeout=1.0)
        if self.thread.is_alive() and self.stream is not None:
            try:
                self.stream.close()  # type: ignore[attr-defined]
            except OSError:
                pass
            self.thread.join(timeout=1.0)
        if self.thread.is_alive():
            raise RuntimeError("publication object stderr drainer did not stop")
        self.joined = True
        self.accounting.batch_stderr_drainers_joined += 1
        if self.error is not None:
            raise RuntimeError(
                f"publication object stderr drain failed: {self.error}"
            ) from self.error

    def output(self) -> bytes:
        raw = bytes(self.buffer)
        if self.discarded:
            return raw + b"\n[stderr truncated]"
        return raw


class _StrictBatchReader:
    def __init__(
        self,
        endpoint: EvidenceGitEndpoint,
        *,
        chunk_size: int,
        accounting: EvidenceMemoryAccounting,
        deadline_seconds: float = BATCH_MAX_SECONDS,
    ) -> None:
        if chunk_size <= 0:
            raise RuntimeError("publication object chunk size must be positive")
        self.endpoint = endpoint
        self.chunk_size = chunk_size
        self.accounting = accounting
        self.deadline_seconds = deadline_seconds
        self.stderr_drain = _BoundedStderrDrain(accounting)
        self.process: subprocess.Popen[bytes] | None = None
        self.deadline = 0.0
        self.provisional_completed = 0
        self.provisional_kinds = {
            "blob": 0, "commit": 0, "tag": 0, "tree": 0,
        }
        self.provisional_distinct_blobs = 0
        self.provisional_oversized_head_blobs = 0
        self.provisional_oversized_historical_blobs = 0
        self.provisional_commit_summary_bytes = 0
        self.buffer = bytearray(chunk_size)
        self.accounting.observe_body_buffer(chunk_size + BLOB_SCAN_OVERLAP)

    def __enter__(self) -> _StrictBatchReader:
        try:
            self.process = subprocess.Popen(
                self.endpoint._command("cat-file", "--batch"),
                cwd=self.endpoint.root,
                env=self.endpoint.environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                bufsize=0,
            )
        except OSError as exc:
            raise RuntimeError(
                f"publication object batch extraction failed: {exc}"
            ) from exc
        stderr = getattr(self.process, "stderr", None)
        if stderr is not None:
            try:
                self.stderr_drain.start(stderr)
            except BaseException:
                self._abort_unstarted()
                raise
        self.deadline = time.monotonic() + self.deadline_seconds
        self.accounting.batch_processes_started += 1
        return self

    def _remaining(self) -> float:
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError("publication object batch extraction timed out")
        return remaining

    def _stdout_fileno(self) -> int | None:
        assert self.process is not None and self.process.stdout is not None
        try:
            return self.process.stdout.fileno()
        except (AttributeError, OSError, ValueError):
            return None

    def _wait_readable(self) -> None:
        descriptor = self._stdout_fileno()
        if descriptor is None:
            return
        try:
            readable, _, _ = select.select(
                [descriptor], [], [], self._remaining(),
            )
        except (OSError, ValueError) as exc:
            raise RuntimeError(
                f"publication object batch extraction failed: {exc}"
            ) from exc
        if not readable:
            raise RuntimeError("publication object batch extraction timed out")

    def _read_byte(self) -> bytes:
        assert self.process is not None and self.process.stdout is not None
        self._wait_readable()
        try:
            return self.process.stdout.read(1)
        except OSError as exc:
            raise RuntimeError(
                f"publication object batch extraction failed: {exc}"
            ) from exc

    def _readinto(self, target: memoryview) -> int:
        assert self.process is not None and self.process.stdout is not None
        self._wait_readable()
        try:
            return self.process.stdout.readinto(target) or 0
        except OSError as exc:
            raise RuntimeError(
                f"publication object batch extraction failed: {exc}"
            ) from exc

    def _stderr(self) -> bytes:
        return self.stderr_drain.output()

    def _join_stderr(self) -> None:
        self.stderr_drain.stop_and_join()

    def _read_header(self, context: str) -> bytes:
        header = bytearray()
        while len(header) <= MAX_BATCH_HEADER_BYTES:
            value = self._read_byte()
            if not value:
                raise RuntimeError(
                    f"publication {context} batch output has a truncated header"
                )
            header.extend(value)
            if len(header) > MAX_BATCH_HEADER_BYTES:
                raise RuntimeError(
                    f"publication {context} batch output has an invalid header"
                )
            if value == b"\n":
                return bytes(header)
        raise RuntimeError(
            f"publication {context} batch output has an invalid header"
        )

    def read_object(
        self,
        expected: str,
        *,
        expected_kind: str | None,
        label: str,
        consumer: object,
        context: str = "object",
        mismatch: str | None = None,
    ) -> tuple[str, int, object]:
        self.endpoint._validate_object_id(expected, label)
        assert self.process is not None
        assert self.process.stdin is not None
        request = f"{expected}\n".encode("ascii")
        try:
            written = 0
            while written < len(request):
                count = self.process.stdin.write(request[written:])
                if not count:
                    raise RuntimeError(
                        "publication object batch request was truncated"
                    )
                written += count
            self.process.stdin.flush()
        except OSError as exc:
            raise RuntimeError(
                "publication object batch request was truncated"
            ) from exc
        self.accounting.objects_requested += 1
        header = self._read_header(context)
        match = re.fullmatch(
            rb"([0-9a-f]+) (blob|commit|tag|tree) (0|[1-9][0-9]*)\n",
            header,
        )
        if match is None:
            if header == f"{expected} missing\n".encode("ascii"):
                raise RuntimeError(f"cannot extract publication {context} {expected}")
            raise RuntimeError(
                f"publication {context} batch output has an invalid header"
            )
        actual = match.group(1).decode("ascii")
        kind = match.group(2).decode("ascii")
        size = int(match.group(3))
        if actual != expected or (
            expected_kind is not None and kind != expected_kind
        ):
            raise RuntimeError(
                mismatch
                or f"publication {context} {expected} does not match its request"
            )
        self.accounting.announced_body_bytes += size
        digest = hashlib.new(self.endpoint.object_format)
        digest.update(f"{kind} {size}\0".encode("ascii"))
        remaining = size
        consumer_error: BaseException | None = None
        active_consumer = consumer
        while remaining:
            requested = min(self.chunk_size, remaining)
            target = memoryview(self.buffer)[:requested]
            count = self._readinto(target)
            if not count:
                raise RuntimeError(
                    f"publication {context} {expected} output is truncated"
                )
            raw = target[:count]
            digest.update(raw)
            if consumer_error is None:
                try:
                    active_consumer.feed(raw)
                except Exception as exc:
                    consumer_error = exc
                    active_consumer = _DiscardObjectConsumer()
            self.accounting.consumed_body_bytes += count
            self.accounting.max_body_chunk_bytes = max(
                self.accounting.max_body_chunk_bytes, count,
            )
            self.accounting.max_scanner_carry_bytes = max(
                self.accounting.max_scanner_carry_bytes, BLOB_SCAN_OVERLAP,
            )
            remaining -= count
        if self._read_byte() != b"\n":
            raise RuntimeError(
                f"publication {context} {expected} output is truncated"
            )
        self.endpoint.verify_object_digest(
            expected, digest.hexdigest(), label,
        )
        if consumer_error is not None:
            raise consumer_error
        result = active_consumer.finish()
        self.endpoint._object_headers[expected] = (kind, size)
        self.provisional_completed += 1
        self.provisional_kinds[kind] += 1
        return kind, size, result

    def _close_streams(self) -> None:
        if self.process is None:
            return
        if self.process.stdout is not None:
            self.process.stdout.close()
        if self.process.stdin is not None and not self.process.stdin.closed:
            self.process.stdin.close()
        stderr = getattr(self.process, "stderr", None)
        if stderr is not None:
            stderr.close()

    def _abort_unstarted(self) -> None:
        """Reap the child when the drain never started, preserving the start failure."""
        assert self.process is not None
        try:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait()
        except OSError:
            pass
        finally:
            self._join_stderr()
            try:
                self._close_streams()
            except OSError:
                pass

    def _abort(self, original: BaseException) -> None:
        assert self.process is not None
        if self.process.stdin is not None and not self.process.stdin.closed:
            try:
                self.process.stdin.close()
            except OSError:
                pass
        terminated = False
        try:
            if self.process.poll() is None:
                try:
                    self.process.wait(timeout=min(1.0, max(0.01, self._remaining())))
                except (subprocess.TimeoutExpired, RuntimeError):
                    terminated = True
                    self.process.terminate()
                    try:
                        self.process.wait(timeout=1)
                    except subprocess.TimeoutExpired:
                        self.process.kill()
                        self.process.wait()
        finally:
            self._join_stderr()
        if (
            isinstance(original, Exception)
            and self.process.returncode not in (None, 0)
            and not terminated
        ):
            raise RuntimeError(
                _decode_git_error(
                    self._stderr(), "publication object batch extraction failed",
                )
            ) from original

    def _finish(self) -> None:
        assert self.process is not None and self.process.stdin is not None
        self.process.stdin.close()
        if self._read_byte():
            raise RuntimeError("publication object batch output has trailing data")
        try:
            returncode = self.process.wait(timeout=self._remaining())
        except subprocess.TimeoutExpired as exc:
            raise RuntimeError("publication object batch extraction timed out") from exc
        self._join_stderr()
        if returncode:
            raise RuntimeError(
                _decode_git_error(
                    self._stderr(), "publication object batch extraction failed",
                )
            )
        self.accounting.batch_processes_completed += 1
        self.accounting.objects_completed += self.provisional_completed
        self.accounting.blob_objects_completed += self.provisional_kinds["blob"]
        self.accounting.commit_objects_completed += self.provisional_kinds["commit"]
        self.accounting.tag_objects_completed += self.provisional_kinds["tag"]
        self.accounting.tree_objects_completed += self.provisional_kinds["tree"]
        self.accounting.distinct_blobs_scanned += self.provisional_distinct_blobs
        self.accounting.oversized_head_blobs += (
            self.provisional_oversized_head_blobs
        )
        self.accounting.oversized_historical_blobs += (
            self.provisional_oversized_historical_blobs
        )
        self.accounting.retained_commit_summary_bytes += (
            self.provisional_commit_summary_bytes
        )

    def __exit__(self, exc_type, exc, traceback) -> bool:
        try:
            if exc is None:
                try:
                    self._finish()
                except BaseException as close_error:
                    self._abort(close_error)
                    raise
            else:
                self._abort(exc)
        except BaseException as close_error:
            self._close_streams()
            raise close_error
        self._close_streams()
        return False


class EvidenceGitEndpoint:
    """A repository-bound, unvirtualized Git object endpoint."""

    def __init__(
        self, root: Path, git_dir: Path, common_dir: Path, environment: dict[str, str],
    ) -> None:
        self.root = root
        self.git_dir = git_dir
        self.common_dir = common_dir
        self.environment = environment
        self._object_headers: dict[str, tuple[str, int]] = {}
        self._commits: dict[str, RawCommit] = {}
        self._batch_reader: _StrictBatchReader | None = None
        self.object_format = ""
        self.raw_object_id_size = 0

    @staticmethod
    def _discovery_process(
        root: Path, environment: Mapping[str, str],
    ) -> subprocess.CompletedProcess[bytes]:
        try:
            return subprocess.run(
                [
                    "git", "--no-replace-objects", "--no-optional-locks",
                    "-c", "core.useReplaceRefs=false", "-c", "core.fsmonitor=false",
                    "-C", str(root), "rev-parse", "--path-format=absolute",
                    "--show-toplevel", "--absolute-git-dir", "--git-common-dir",
                ],
                env=dict(environment), stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                check=False,
            )
        except OSError as exc:
            raise RuntimeError(f"git evidence endpoint discovery failed: {exc}") from exc

    @classmethod
    def open(cls, root: Path = ROOT) -> EvidenceGitEndpoint:
        try:
            anchored_root = root.resolve(strict=True)
        except OSError as exc:
            raise RuntimeError(f"cannot resolve publication repository root: {exc}") from exc
        environment = sanitized_git_environment()
        discovery = cls._discovery_process(anchored_root, environment)
        if discovery.returncode:
            raise RuntimeError(
                _decode_git_error(
                    discovery.stderr, "git evidence endpoint discovery failed",
                )
            )
        try:
            lines = discovery.stdout.decode("utf-8").splitlines()
        except UnicodeDecodeError as exc:
            raise RuntimeError("git evidence endpoint identity is not UTF-8") from exc
        if len(lines) != 3 or any(not line for line in lines):
            raise RuntimeError("git evidence endpoint identity is malformed")
        try:
            top_level, git_dir, common_dir = (
                Path(line).resolve(strict=True) for line in lines
            )
        except OSError as exc:
            raise RuntimeError(f"git evidence endpoint identity is invalid: {exc}") from exc
        if top_level != anchored_root:
            raise RuntimeError("git evidence endpoint is not anchored to the repository root")

        endpoint = cls(anchored_root, git_dir, common_dir, environment)
        endpoint._reject_repository_virtualization()
        endpoint.environment = {**endpoint.environment, "GIT_SHALLOW_FILE": os.devnull}
        endpoint._bind_object_format()
        endpoint.assert_identity()
        return endpoint

    def _command(self, *args: str) -> list[str]:
        return [
            "git", "--no-replace-objects", "--no-optional-locks",
            "-c", "core.useReplaceRefs=false", "-c", "core.fsmonitor=false",
            f"--git-dir={self.git_dir}", f"--work-tree={self.root}", *args,
        ]

    def run_bytes(
        self, *args: str, input_data: bytes | None = None,
    ) -> subprocess.CompletedProcess[bytes]:
        try:
            return subprocess.run(
                self._command(*args), cwd=self.root, env=self.environment,
                input=input_data, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                check=False,
            )
        except OSError as exc:
            raise RuntimeError(f"git evidence read failed: {exc}") from exc

    def require_bytes(
        self, *args: str, fallback: str, input_data: bytes | None = None,
    ) -> bytes:
        result = self.run_bytes(*args, input_data=input_data)
        if result.returncode:
            raise RuntimeError(_decode_git_error(result.stderr, fallback))
        return result.stdout

    def _reject_nonempty_state(self, relative: Path, message: str) -> None:
        candidates = {self.git_dir / relative, self.common_dir / relative}
        for path in candidates:
            try:
                metadata = path.lstat()
            except FileNotFoundError:
                continue
            except OSError as exc:
                raise RuntimeError(f"cannot inspect publication repository state: {exc}") from exc
            if path.is_symlink() or not path.is_file() or metadata.st_size:
                raise RuntimeError(message)

    def _reject_repository_virtualization(self) -> None:
        self._reject_nonempty_state(
            Path("info/grafts"),
            "publication evidence rejected: nonempty repository grafts",
        )
        self._reject_nonempty_state(
            Path("shallow"),
            "publication evidence rejected: shallow repository",
        )

    def _read_object_format(self) -> str:
        raw = self.require_bytes(
            "rev-parse", "--show-object-format=storage",
            fallback="cannot determine publication repository object format",
        ).strip()
        try:
            object_format = raw.decode("ascii")
        except UnicodeDecodeError as exc:
            raise RuntimeError("publication repository object format is malformed") from exc
        if object_format not in {"sha1", "sha256"}:
            raise RuntimeError(
                f"unsupported publication repository object format: {object_format!r}"
            )
        return object_format

    def _bind_object_format(self) -> None:
        self.object_format = self._read_object_format()
        self.raw_object_id_size = {"sha1": 20, "sha256": 32}[self.object_format]

    def _validate_object_id(self, expected: str, label: str) -> None:
        expected_length = self.raw_object_id_size * 2
        if (
            len(expected) != expected_length
            or OID.fullmatch(expected) is None
        ):
            raise RuntimeError(
                f"{label} has an invalid {self.object_format} object id"
            )

    def verify_object_digest(
        self, expected: str, actual_digest: str, label: str,
    ) -> None:
        self._validate_object_id(expected, label)
        if actual_digest != expected:
            raise RuntimeError(
                f"{label} object identity does not match {self.object_format}"
            )

    def object_header(
        self, object_id: str, *, label: str | None = None, refresh: bool = False,
    ) -> tuple[str, int]:
        object_label = label or f"publication object {object_id}"
        self._validate_object_id(object_id, object_label)
        if not refresh and object_id in self._object_headers:
            return self._object_headers[object_id]
        raw = self.require_bytes(
            "cat-file", "--batch-check=%(objectname) %(objecttype) %(objectsize)",
            input_data=f"{object_id}\n".encode("ascii"),
            fallback=f"cannot inspect {object_label}",
        )
        match = re.fullmatch(
            rb"([0-9a-f]+) (blob|commit|tag|tree) (0|[1-9][0-9]*)\n", raw,
        )
        if match is None:
            raise RuntimeError(f"cannot inspect {object_label}")
        try:
            actual = match.group(1).decode("ascii")
            kind = match.group(2).decode("ascii")
            size = int(match.group(3))
        except (UnicodeDecodeError, ValueError) as exc:
            raise RuntimeError(f"cannot inspect {object_label}") from exc
        if actual != object_id:
            raise RuntimeError(f"{object_label} does not match its request")
        self._object_headers[object_id] = (kind, size)
        return kind, size

    @contextmanager
    def object_batch(
        self,
        *,
        chunk_size: int = BLOB_STREAM_CHUNK_SIZE,
        accounting: EvidenceMemoryAccounting | None = None,
        deadline_seconds: float = BATCH_MAX_SECONDS,
    ) -> Iterator[_StrictBatchReader]:
        if self._batch_reader is not None:
            raise RuntimeError("publication object batch is already active")
        reader = _StrictBatchReader(
            self,
            chunk_size=chunk_size,
            accounting=accounting or EvidenceMemoryAccounting(),
            deadline_seconds=deadline_seconds,
        )
        self._batch_reader = reader
        try:
            with reader:
                yield reader
        finally:
            self._batch_reader = None

    def _reader(self) -> _StrictBatchReader:
        if self._batch_reader is None:
            raise RuntimeError("publication object batch is not active")
        return self._batch_reader

    def verify_stored_object(
        self,
        object_id: str,
        *,
        expected_kind: str | None = None,
        label: str | None = None,
    ) -> tuple[str, int]:
        object_label = label or f"publication object {object_id}"
        kind, size, _ = self._reader().read_object(
            object_id,
            expected_kind=expected_kind,
            label=object_label,
            consumer=_DiscardObjectConsumer(),
        )
        return kind, size

    def assert_identity(self) -> None:
        raw = self.require_bytes(
            "rev-parse", "--path-format=absolute", "--show-toplevel",
            "--absolute-git-dir", "--git-common-dir",
            fallback="git evidence endpoint identity check failed",
        )
        try:
            lines = raw.decode("utf-8").splitlines()
            identity = tuple(Path(line).resolve(strict=True) for line in lines)
        except (UnicodeDecodeError, OSError) as exc:
            raise RuntimeError(f"git evidence endpoint identity is invalid: {exc}") from exc
        if identity != (self.root, self.git_dir, self.common_dir):
            raise RuntimeError("git evidence endpoint identity changed during the scan")
        if self.object_format and self._read_object_format() != self.object_format:
            raise RuntimeError("publication repository object format changed during the scan")

    def resolve_commit_endpoint(self, revision: str, *, refresh: bool = False) -> str:
        result = self.run_bytes(
            "rev-parse", "--verify", "--end-of-options", revision,
        )
        try:
            resolved = result.stdout.decode("ascii").strip().lower()
        except UnicodeDecodeError:
            resolved = ""
        if result.returncode or OID.fullmatch(resolved) is None:
            detail = _decode_git_error(
                result.stderr, "revision does not resolve to one object",
            )
            raise RuntimeError(f"cannot resolve publication endpoint {revision!r}: {detail}")
        kind, _ = self.verify_stored_object(
            resolved,
            label=f"publication endpoint {revision!r}",
        )
        if kind != "commit":
            raise RuntimeError(
                f"publication endpoint {revision!r} does not match its raw commit identity"
            )
        return resolved

    def assert_endpoint_binding(self, revision: str, expected: str) -> None:
        if self.resolve_commit_endpoint(revision, refresh=True) != expected:
            raise RuntimeError(
                f"publication endpoint {revision!r} changed during the scan"
            )

    def commit(self, object_id: str) -> RawCommit:
        if object_id not in self._commits:
            consumer = _CommitObjectConsumer(
                object_id, self._reader().accounting,
            )
            _, _, commit = self._reader().read_object(
                object_id,
                expected_kind="commit",
                label=f"publication commit {object_id}",
                consumer=consumer,
                mismatch=f"publication object {object_id} is not a commit",
            )
            assert isinstance(commit, RawCommit)
            self._commits[object_id] = commit
            self._reader().provisional_commit_summary_bytes += consumer.summary_bytes
        return self._commits[object_id]

    def visit_tree(
        self, object_id: str, visitor: Callable[[RawTreeItem], None],
    ) -> None:
        consumer = _TreeObjectConsumer(
            self.raw_object_id_size, visitor, self._reader().accounting,
        )
        self._reader().read_object(
            object_id,
            expected_kind="tree",
            label=f"publication tree {object_id}",
            consumer=consumer,
            mismatch=f"publication object {object_id} is not a tree",
        )

    def read_tag(self, object_id: str) -> RawTag:
        consumer = _TagObjectConsumer(
            object_id, self._reader().accounting,
        )
        _, _, result = self._reader().read_object(
            object_id,
            expected_kind="tag",
            label=f"publication tag {object_id}",
            consumer=consumer,
            mismatch=f"publication object {object_id} is not a tag",
        )
        assert isinstance(result, RawTag)
        return result

    def ancestors(self, start: str) -> dict[str, RawCommit]:
        commits: dict[str, RawCommit] = {}
        pending = [start]
        while pending:
            object_id = pending.pop()
            if object_id in commits:
                continue
            commit = self.commit(object_id)
            commits[object_id] = commit
            pending.extend(commit.parents)
        return commits


def git(*args: str, root: Path = ROOT) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args], cwd=root, text=True, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, check=False,
    )


def tracked_files() -> list[str]:
    result = git("ls-files", "-z")
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "git ls-files failed")
    return [value for value in result.stdout.split("\0") if value]


def _is_script_python(relative: str) -> bool:
    return relative.startswith("scripts/") and relative.endswith(".py")


def _is_shell_surface(relative: str, text: str) -> bool:
    if relative == "scripts/lib/harness-python.sh":
        return False
    if relative.startswith(".github/workflows/"):
        return True
    if not relative.startswith("scripts/") or _is_script_python(relative):
        return False
    first_line = text.splitlines()[0] if text else ""
    if first_line.startswith("#!") and re.search(r"\bpython(?:3)?\b", first_line):
        return False
    return Path(relative).suffix in SHELL_SUFFIXES


def scan_paths(paths: list[str], root: Path = ROOT) -> list[str]:
    errors: list[str] = []
    tracked = set(paths)
    for required in sorted(REQUIRED):
        if not (root / required).is_file():
            errors.append(f"missing required public file: {required}")
        elif required not in tracked:
            errors.append(f"required public file is not tracked: {required}")
    for relative in paths:
        if relative in FORBIDDEN_TRACKED or any(relative.startswith(p) for p in FORBIDDEN_PREFIXES):
            errors.append(f"forbidden tracked path: {relative}")
            continue
        path = root / relative
        if path.is_symlink():
            errors.append(f"tracked symlink is not portable: {relative}")
            continue
        if not path.is_file():
            continue
        if _is_script_python(relative) and path.stat().st_mode & 0o111:
            errors.append(f"forbidden executable Python script: {relative}")
        if path.stat().st_size > MAX_PUBLIC_FILE_BYTES:
            errors.append(f"tracked file exceeds 5 MiB: {relative}")
            continue
        try:
            text = path.read_text()
        except (UnicodeDecodeError, OSError):
            continue
        if _is_script_python(relative) and text.startswith("#!"):
            errors.append(f"forbidden Python shebang: {relative}")
        if _is_shell_surface(relative, text) and BARE_PYTHON_COMMAND.search(text):
            errors.append(f"bare Python command in shell surface: {relative}")
        if HOME_PATH.search(text):
            errors.append(f"personal absolute home path: {relative}")
        for label, pattern in SECRET_PATTERNS.items():
            if pattern.search(text):
                errors.append(f"possible {label}: {relative}")
    return errors


TreeEntry = tuple[str, str, str, int | None, str]


@dataclass(frozen=True)
class TreeMaterial:
    head_entries: tuple[TreeEntry, ...]
    blob_ids: frozenset[str]
    head_blob_ids: frozenset[str]
    forbidden_paths: frozenset[str]


class _TreePathInterner:
    """Retain each distinct path edge once and materialize only leaf paths."""

    def __init__(self, accounting: EvidenceMemoryAccounting) -> None:
        self.accounting = accounting
        self.nodes: list[tuple[int, str]] = [(-1, "")]
        self.index: dict[tuple[int, str], int] = {}
        self.component_bytes = 0

    def child(self, parent: int, component: str) -> int:
        key = (parent, component)
        known = self.index.get(key)
        if known is not None:
            return known
        path_id = len(self.nodes)
        self.nodes.append(key)
        self.index[key] = path_id
        self.component_bytes += len(component.encode("utf-8"))
        self.accounting.max_tree_path_nodes_retained = max(
            self.accounting.max_tree_path_nodes_retained, len(self.nodes) - 1,
        )
        self.accounting.max_tree_path_component_bytes_retained = max(
            self.accounting.max_tree_path_component_bytes_retained,
            self.component_bytes,
        )
        return path_id

    def materialize(self, path_id: int, leaf: str) -> str:
        components = [leaf]
        while path_id:
            path_id, component = self.nodes[path_id]
            components.append(component)
        relative = "/".join(reversed(components))
        self.accounting.max_materialized_tree_path_bytes = max(
            self.accounting.max_materialized_tree_path_bytes,
            len(relative.encode("utf-8")),
        )
        return relative

    def clear(self) -> None:
        self.nodes.clear()
        self.index.clear()
        self.component_bytes = 0


def visit_raw_tree_entries(
    endpoint: EvidenceGitEndpoint,
    tree: str,
    visitor: Callable[[TreeEntry], None],
    *,
    paths: _TreePathInterner | None = None,
    visited: set[tuple[str, int]] | None = None,
) -> None:
    accounting = endpoint._reader().accounting
    path_interner = paths if paths is not None else _TreePathInterner(accounting)
    active: set[str] = set()
    contexts = set() if visited is None else visited
    pending: list[tuple[str, str, int]] = [("enter", tree, 0)]
    while pending:
        action, object_id, path_id = pending.pop()
        if action == "exit":
            active.remove(object_id)
            continue
        if object_id in active:
            raise RuntimeError(f"publication tree {object_id} contains a cycle")
        context = (object_id, path_id)
        if context in contexts:
            continue
        contexts.add(context)
        active.add(object_id)
        children: list[tuple[str, int]] = []

        def consume(item: RawTreeItem) -> None:
            if item.kind == "tree":
                children.append(
                    (item.object_id, path_interner.child(path_id, item.name))
                )
            else:
                visitor(
                    (
                        item.mode,
                        item.kind,
                        item.object_id,
                        None,
                        path_interner.materialize(path_id, item.name),
                    )
                )
        endpoint.visit_tree(object_id, consume)
        pending.append(("exit", object_id, 0))
        new_entries = [
            ("enter", child_id, child_path_id)
            for child_id, child_path_id in reversed(children)
        ]
        pending.extend(new_entries)
        accounting.max_pending_tree_walk_items = max(
            accounting.max_pending_tree_walk_items, len(pending),
        )
        accounting.retained_tree_context_summaries = max(
            accounting.retained_tree_context_summaries, len(contexts),
        )


def raw_tree_entries(endpoint: EvidenceGitEndpoint, tree: str) -> tuple[TreeEntry, ...]:
    entries: list[TreeEntry] = []
    visit_raw_tree_entries(endpoint, tree, entries.append)
    return tuple(entries)


def collect_tree_material(
    endpoint: EvidenceGitEndpoint,
    commits: Mapping[str, RawCommit],
    head_tree: str,
    accounting: EvidenceMemoryAccounting,
    extra_root_trees: set[str] | frozenset[str] = frozenset(),
) -> TreeMaterial:
    blob_ids: set[str] = set()
    forbidden_paths: set[str] = set()
    commit_root_trees = {commit.tree for commit in commits.values()}
    if head_tree not in commit_root_trees:
        raise RuntimeError("publication HEAD tree is outside the selected commits")
    root_trees = commit_root_trees | set(extra_root_trees)

    def summarize(entry: TreeEntry) -> None:
        _, kind, object_id, _, relative = entry
        if relative in FORBIDDEN_TRACKED or any(
            relative.startswith(prefix) for prefix in FORBIDDEN_PREFIXES
        ):
            forbidden_paths.add(relative)
        if kind == "blob":
            blob_ids.add(object_id)

    summary_paths = _TreePathInterner(accounting)
    summary_visited: set[tuple[str, int]] = set()
    for tree in sorted(root_trees - {head_tree}):
        visit_raw_tree_entries(
            endpoint,
            tree,
            summarize,
            paths=summary_paths,
            visited=summary_visited,
        )
        accounting.distinct_root_trees_walked += 1
    summary_visited.clear()
    summary_paths.clear()
    accounting.historical_tree_contexts_released_before_head = True

    accounting.retain_flattened_tree()
    try:
        head_entries = raw_tree_entries(endpoint, head_tree)
    except BaseException:
        accounting.release_flattened_tree()
        raise
    accounting.distinct_root_trees_walked += 1
    accounting.head_flattened_entries_retained = max(
        accounting.head_flattened_entries_retained, len(head_entries),
    )
    for entry in head_entries:
        summarize(entry)
    head_blob_ids = frozenset(
        object_id
        for _, kind, object_id, _, _ in head_entries
        if kind == "blob"
    )
    return TreeMaterial(
        head_entries=head_entries,
        blob_ids=frozenset(blob_ids),
        head_blob_ids=head_blob_ids,
        forbidden_paths=frozenset(forbidden_paths),
    )


def stream_blob_evidence(
    endpoint: EvidenceGitEndpoint,
    object_ids: set[str] | frozenset[str],
    *,
    head_blob_ids: set[str] | frozenset[str] = frozenset(),
    labels: Mapping[str, str] | None = None,
    chunk_size: int = BLOB_STREAM_CHUNK_SIZE,
    accounting: EvidenceMemoryAccounting | None = None,
) -> dict[str, BlobEvidence]:
    if chunk_size <= 0:
        raise RuntimeError("publication blob chunk size must be positive")
    stats = accounting or EvidenceMemoryAccounting()
    ordered_ids = sorted(object_ids)
    if not ordered_ids:
        return {}
    if endpoint._batch_reader is None:
        with endpoint.object_batch(chunk_size=chunk_size, accounting=stats):
            return stream_blob_evidence(
                endpoint,
                object_ids,
                head_blob_ids=head_blob_ids,
                labels=labels,
                chunk_size=chunk_size,
                accounting=stats,
            )

    reader = endpoint._reader()
    if reader.chunk_size != chunk_size:
        raise RuntimeError("publication object batch chunk size changed during scan")
    evidence: dict[str, BlobEvidence] = {}
    for expected in ordered_ids:
        label = (
            labels[expected]
            if labels is not None and expected in labels
            else f"publication blob {expected}"
        )
        consumer = _BlobObjectConsumer()
        _, size, findings = reader.read_object(
            expected,
            expected_kind="blob",
            label=label,
            consumer=consumer,
            context="blob",
            mismatch=(
                f"publication blob {expected} does not match its tree entry"
            ),
        )
        assert isinstance(findings, frozenset)
        if size > MAX_PUBLIC_FILE_BYTES:
            if expected in head_blob_ids:
                reader.provisional_oversized_head_blobs += 1
            else:
                reader.provisional_oversized_historical_blobs += 1
        evidence[expected] = BlobEvidence(size=size, findings=findings)
    reader.provisional_distinct_blobs += len(evidence)
    return evidence


def raw_publication_tree_errors(
    entries: tuple[TreeEntry, ...],
    blobs: Mapping[str, BlobEvidence],
    accounting: EvidenceMemoryAccounting | None = None,
) -> list[str]:
    entry_errors: list[str] = []
    required_kinds: dict[str, str] = {}
    for mode, kind, object_id, _, relative in entries:
        if relative in REQUIRED:
            required_kinds[relative] = kind
        if relative in FORBIDDEN_TRACKED or any(
            relative.startswith(prefix) for prefix in FORBIDDEN_PREFIXES
        ):
            entry_errors.append(
                f"publication HEAD forbidden tracked path: {relative}"
            )
            continue
        if mode == "120000":
            entry_errors.append(
                f"publication HEAD tracked symlink is not portable: {relative}"
            )
            continue
        if kind != "blob":
            continue
        evidence = blobs[object_id]
        if evidence.size > MAX_PUBLIC_FILE_BYTES:
            entry_errors.append(
                f"publication HEAD tracked file exceeds 5 MiB: {relative}"
            )
            continue
        if "personal absolute home path" in evidence.findings:
            entry_errors.append(
                f"publication HEAD personal absolute home path: {relative}"
            )
        for label in SECRET_BYTE_PATTERNS:
            if f"possible {label}" in evidence.findings:
                entry_errors.append(f"publication HEAD possible {label}: {relative}")
    if accounting is not None:
        accounting.max_required_head_entries_retained = max(
            accounting.max_required_head_entries_retained, len(required_kinds),
        )
    missing_errors = [
        f"publication HEAD missing required public file: {required}"
        for required in sorted(REQUIRED)
        if required_kinds.get(required) != "blob"
    ]
    return [*missing_errors, *entry_errors]


def tree_evidence_findings(
    material: TreeMaterial,
    blobs: Mapping[str, BlobEvidence],
) -> tuple[set[str], set[str]]:
    findings: set[str] = set()
    for object_id in material.blob_ids:
        findings.update(blobs[object_id].findings)
    return set(material.forbidden_paths), findings


def commit_message_findings(commits: Mapping[str, RawCommit]) -> set[str]:
    findings: set[str] = set()
    for commit in commits.values():
        findings.update(commit.message_findings)
    return findings


def raw_ref_objects(endpoint: EvidenceGitEndpoint) -> tuple[str, ...]:
    result = endpoint.run_bytes("show-ref", "--head", "--hash")
    if result.returncode not in (0, 1):
        raise RuntimeError(
            _decode_git_error(result.stderr, "publication reference enumeration failed")
        )
    try:
        lines = result.stdout.decode("ascii").splitlines()
    except UnicodeDecodeError as exc:
        raise RuntimeError("publication reference enumeration is malformed") from exc
    object_ids = tuple(sorted(set(lines)))
    if any(OID.fullmatch(object_id) is None for object_id in object_ids):
        raise RuntimeError("publication reference enumeration is malformed")
    return object_ids


def raw_tag_target(endpoint: EvidenceGitEndpoint, object_id: str) -> RawTag:
    tag = endpoint.read_tag(object_id)
    actual_type, _ = endpoint.object_header(
        tag.target, label=f"publication tag target {tag.target}",
    )
    if tag.declared_type != actual_type:
        raise RuntimeError(f"publication tag {object_id} target type does not match")
    return tag


def raw_history_commit_roots(
    endpoint: EvidenceGitEndpoint, ref_objects: tuple[str, ...],
) -> tuple[set[str], set[str], set[str], set[str], set[str]]:
    roots: set[str] = set()
    raw_blob_ids: set[str] = set()
    raw_tree_ids: set[str] = set()
    tagger_emails: set[str] = set()
    tag_message_findings: set[str] = set()
    pending = list(ref_objects)
    seen: set[str] = set()
    while pending:
        object_id = pending.pop()
        if object_id in seen:
            continue
        seen.add(object_id)
        kind, _ = endpoint.object_header(object_id)
        if kind == "commit":
            endpoint.commit(object_id)
            roots.add(object_id)
        elif kind == "tag":
            tag = raw_tag_target(endpoint, object_id)
            tagger_emails.add(tag.tagger_email)
            tag_message_findings.update(tag.message_findings)
            pending.append(tag.target)
        elif kind == "blob":
            raw_blob_ids.add(object_id)
        elif kind == "tree":
            raw_tree_ids.add(object_id)
        else:
            raise RuntimeError(f"publication object {object_id} has unknown type")
    return (
        roots,
        raw_blob_ids,
        raw_tree_ids,
        tagger_emails,
        tag_message_findings,
    )


def history_errors(
    root: Path = ROOT,
    *,
    blob_chunk_size: int = BLOB_STREAM_CHUNK_SIZE,
    accounting: EvidenceMemoryAccounting | None = None,
) -> list[str]:
    stats = accounting or EvidenceMemoryAccounting()
    try:
        endpoint = EvidenceGitEndpoint.open(root)
        with endpoint.object_batch(
            chunk_size=blob_chunk_size, accounting=stats,
        ):
            head = endpoint.resolve_commit_endpoint("HEAD")
            raw_index = endpoint.require_bytes(
                "ls-files", "-z", fallback="publication index enumeration failed",
            )
            index_records = raw_index.split(b"\0")
            if index_records[-1] != b"":
                raise RuntimeError("publication index enumeration is not NUL-terminated")
            try:
                paths = [record.decode("utf-8") for record in index_records[:-1]]
            except UnicodeDecodeError as exc:
                raise RuntimeError("publication index contains an undecodable path") from exc
            if len(paths) != len(set(paths)):
                raise RuntimeError("publication index contains duplicate paths")
            try:
                current_errors = scan_paths(paths, root)
            except OSError as exc:
                raise RuntimeError(f"publication working tree scan failed: {exc}") from exc
            ref_objects = raw_ref_objects(endpoint)
            (
                roots,
                raw_ref_blob_ids,
                raw_ref_tree_ids,
                tagger_emails,
                tag_message_findings,
            ) = raw_history_commit_roots(endpoint, ref_objects)
            roots.add(head)
            commits: dict[str, RawCommit] = {}
            for object_id in roots:
                commits.update(endpoint.ancestors(object_id))
            material = collect_tree_material(
                endpoint,
                commits,
                commits[head].tree,
                stats,
                extra_root_trees=raw_ref_tree_ids,
            )
            try:
                all_blob_ids = set(material.blob_ids) | raw_ref_blob_ids
                blobs = stream_blob_evidence(
                    endpoint,
                    all_blob_ids,
                    head_blob_ids=material.head_blob_ids,
                    labels={
                        object_id: f"publication object {object_id}"
                        for object_id in raw_ref_blob_ids
                    },
                    chunk_size=blob_chunk_size,
                    accounting=stats,
                )
                head_errors = raw_publication_tree_errors(
                    material.head_entries, blobs, stats,
                )
                forbidden_paths, content_findings = tree_evidence_findings(
                    material, blobs,
                )
                for object_id in raw_ref_blob_ids:
                    content_findings.update(blobs[object_id].findings)
            finally:
                stats.release_flattened_tree()
            message_findings = commit_message_findings(commits)
            endpoint.assert_identity()
            endpoint.assert_endpoint_binding("HEAD", head)
            if raw_ref_objects(endpoint) != ref_objects:
                raise RuntimeError("publication references changed during the scan")
    except RuntimeError as exc:
        return [str(exc)]

    errors = [*current_errors, *head_errors]
    errors.extend(
        f"reachable history contains a forbidden tracked path: {relative}"
        for relative in sorted(forbidden_paths)
    )
    errors.extend(
        f"reachable history contains a {finding}"
        for finding in sorted(content_findings)
    )
    errors.extend(
        f"reachable history commit message contains a {finding}"
        for finding in sorted(message_findings)
    )
    errors.extend(
        f"reachable history annotated tag message contains a {finding}"
        for finding in sorted(tag_message_findings)
    )
    for email in sorted(
        {commit.author_email for commit in commits.values()}
        | {commit.committer_email for commit in commits.values()}
        | tagger_emails
    ):
        if PERSONAL_EMAIL.search(email.strip()):
            errors.append(f"reachable history exposes a personal email: {email.strip()}")
    return errors


def publication_range_errors(
    base_revision: str,
    head_revision: str,
    root: Path = ROOT,
    *,
    blob_chunk_size: int = BLOB_STREAM_CHUNK_SIZE,
    accounting: EvidenceMemoryAccounting | None = None,
) -> list[str]:
    stats = accounting or EvidenceMemoryAccounting()
    try:
        endpoint = EvidenceGitEndpoint.open(root)
        with endpoint.object_batch(
            chunk_size=blob_chunk_size, accounting=stats,
        ):
            base = endpoint.resolve_commit_endpoint(base_revision)
            head = endpoint.resolve_commit_endpoint(head_revision)
            head_history = endpoint.ancestors(head)
            if base not in head_history:
                return ["publication range base is not an ancestor of head"]
            base_history = endpoint.ancestors(base)
            selected_ids = set(head_history) - set(base_history)
            if not selected_ids:
                return ["publication range must contain at least one commit"]
            selected = {
                object_id: head_history[object_id]
                for object_id in selected_ids
            }
            head_commit = head_history[head]
            material = collect_tree_material(
                endpoint, selected, head_commit.tree, stats,
            )
            try:
                blobs = stream_blob_evidence(
                    endpoint,
                    material.blob_ids,
                    head_blob_ids=material.head_blob_ids,
                    chunk_size=blob_chunk_size,
                    accounting=stats,
                )
                errors = raw_publication_tree_errors(
                    material.head_entries, blobs, stats,
                )
                forbidden_paths, content_findings = tree_evidence_findings(
                    material, blobs,
                )
            finally:
                stats.release_flattened_tree()
            errors.extend(
                f"publication range contains a forbidden tracked path: {relative}"
                for relative in sorted(forbidden_paths)
            )
            errors.extend(
                f"publication range contains a {finding}"
                for finding in sorted(content_findings)
            )
            errors.extend(
                f"publication range commit message contains a {finding}"
                for finding in sorted(commit_message_findings(selected))
            )
            for email in sorted(
                {commit.author_email for commit in selected.values()}
                | {commit.committer_email for commit in selected.values()}
            ):
                if PERSONAL_EMAIL.search(email.strip()):
                    errors.append(
                        f"publication range exposes a personal email: {email.strip()}"
                    )
            endpoint.assert_identity()
            endpoint.assert_endpoint_binding(base_revision, base)
            endpoint.assert_endpoint_binding(head_revision, head)
        return errors
    except RuntimeError as exc:
        return [str(exc)]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--history", action="store_true", help="scan all reachable commits too")
    mode.add_argument(
        "--publication-range", nargs=2, metavar=("BASE", "HEAD"),
        help="scan only commits in HEAD that are not reachable from ancestor BASE",
    )
    args = parser.parse_args(argv)
    if args.publication_range:
        errors = publication_range_errors(*args.publication_range)
        success_scope = "publication range"
    elif args.history:
        errors = history_errors()
        success_scope = "public tree and reachable history"
    else:
        try:
            errors = scan_paths(tracked_files())
        except RuntimeError as exc:
            errors = [str(exc)]
        success_scope = "public tree"
    if errors:
        for error in errors:
            print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print(f"PASS: {success_scope} clean")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
