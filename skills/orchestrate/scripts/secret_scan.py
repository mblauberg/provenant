"""Bounded, high-signal scan of content about to be sent to a provider."""

from __future__ import annotations

import os
from pathlib import Path
import re
import stat
from dataclasses import dataclass, field

MAX_FILE_BYTES = 1024 * 1024
MAX_FILES = 2000
MAX_TOTAL_BYTES = 20 * 1024 * 1024

PATTERNS = (
    ("PEM private key", re.compile(rb"-----BEGIN (?:[A-Z0-9 ]+ )?PRIVATE KEY-----")),
    ("AWS access key ID", re.compile(rb"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b")),
    ("GitHub token", re.compile(rb"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b")),
    ("OpenAI/Anthropic key", re.compile(rb"\b(?:sk-ant-|sk-proj-|sk-)[A-Za-z0-9_-]{40,}\b")),
    ("Slack token", re.compile(rb"\bxox[baprs]-[A-Za-z0-9-]{10,}\b")),
    ("Google API key", re.compile(rb"\bAIza[A-Za-z0-9_-]{35}\b")),
    ("Stripe live key", re.compile(rb"\b(?:sk_live_|rk_live_)[A-Za-z0-9]{16,}\b")),
    ("Bearer JWT", re.compile(rb"\bBearer [A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{16,}\b")),
)


@dataclass(frozen=True)
class Finding:
    name: str
    path: str
    line: int


@dataclass
class ScanResult:
    findings: list[Finding] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)

    def fix(self) -> str:
        finding = self.findings[0]
        return (f"Remove the {finding.name} at {finding.path}:{finding.line} "
                "or pass allow_secrets: true to send it anyway.")

    def names(self) -> list[str]:
        return sorted({finding.name for finding in self.findings})


def scan_bytes(content: bytes, path: str) -> list[Finding]:
    findings = []
    shown_path = path.replace("\n", "?").replace("\r", "?")
    if any(pattern.search(os.fsencode(shown_path)) for _, pattern in PATTERNS):
        shown_path = "<file>"
    for name, pattern in PATTERNS:
        for match in pattern.finditer(content):
            candidate = match.group().upper()
            if name == "PEM private key":
                candidate += content[match.end():].lstrip().splitlines()[0].upper() if content[match.end():].strip() else b""
            before = content[max(0, match.start() - 3):match.start()]
            after = content[match.end():match.end() + 3]
            if (any(marker in candidate for marker in (b"EXAMPLE", b"XXXX", b"<", b"..."))
                    or (before.endswith(b"<") and after.startswith(b">"))
                    or before.endswith(b"...") or after.startswith(b"...")):
                continue
            findings.append(Finding(name, shown_path, content.count(b"\n", 0, match.start()) + 1))
    return sorted(findings, key=lambda item: item.line)


def _files(directory: Path):
    if not directory.is_dir():
        raise OSError(f"additional directory unavailable: {directory}")
    if {"node_modules", ".git"}.intersection(directory.parts):
        return
    def raise_walk_error(error: OSError) -> None:
        raise error

    for root, dirs, files in os.walk(directory, followlinks=False, onerror=raise_walk_error):
        dirs[:] = [name for name in dirs if name not in {"node_modules", ".git"}]
        for name in files:
            if name not in {"node_modules", ".git"}:
                yield Path(root) / name


def scan_inputs(prompt: bytes, prompt_path: str, add_dirs: list[str] | None = None) -> ScanResult:
    result = ScanResult(scan_bytes(prompt, prompt_path))
    files_seen = 0
    bytes_seen = 0
    for raw_dir in add_dirs or []:
        for path in _files(Path(raw_dir).expanduser().resolve()):
            try:
                metadata = path.lstat()
                if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_FILE_BYTES:
                    continue
                if files_seen >= MAX_FILES or bytes_seen + metadata.st_size > MAX_TOTAL_BYTES:
                    result.warnings.append("secret scan directory budget reached")
                    return result
                files_seen += 1
                with path.open("rb") as stream:
                    content = stream.read(MAX_FILE_BYTES + 1)
                bytes_seen += len(content)
                if len(content) > MAX_FILE_BYTES or b"\0" in content:
                    continue
                result.findings.extend(scan_bytes(content, str(path)))
            except FileNotFoundError:
                continue
            except OSError as exc:
                raise OSError(f"cannot scan additional file: {path}") from exc
    return result
