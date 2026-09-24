"""Bounded, high-signal scan of content about to be sent to a provider."""

from __future__ import annotations

import os
from pathlib import Path
import re
import stat
import subprocess
from dataclasses import dataclass, field

MAX_FILE_BYTES = 1024 * 1024
MAX_FILES = 2000
MAX_TOTAL_BYTES = 20 * 1024 * 1024
SKIP_DIRS = {".git", "node_modules", ".venv", "vendor", "vendors", "third_party",
             "third-party", "dist", "build", "coverage", ".agent-run"}
SKIP_SUFFIXES = {".7z", ".a", ".bin", ".class", ".dll", ".dylib", ".exe", ".gif", ".gz",
                 ".jar", ".jpeg", ".jpg", ".mp3", ".mp4", ".o", ".pdf", ".png", ".pyc",
                 ".so", ".tar", ".webp", ".zip"}

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
    budget_exceeded: bool = False
    budget_path: str | None = None

    def fix(self) -> str:
        if self.budget_exceeded:
            location = f" in {self.budget_path}" if self.budget_path else ""
            return (f"Secret scan budget exceeded{location}; narrow the prompt or pass a narrower path "
                    "or pass allow_secrets: true and explain why in the prompt.")
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
    if SKIP_DIRS.intersection(directory.parts):
        return
    try:
        repository = subprocess.run(
            ["git", "-C", str(directory), "rev-parse", "--show-toplevel"],
            capture_output=True, text=True, timeout=5,
        )
        repo_root = Path(repository.stdout.strip()).resolve() if repository.returncode == 0 else None
    except (OSError, subprocess.SubprocessError):
        repo_root = None

    def ignored(paths: list[Path]) -> set[Path]:
        if repo_root is None or not paths:
            return set()
        relative: dict[bytes, Path] = {}
        for path in paths:
            try:
                relative[os.fsencode(path.resolve().relative_to(repo_root).as_posix())] = path
            except (OSError, ValueError):
                continue
        ignored_paths: set[Path] = set()
        encoded = list(relative)
        for offset in range(0, len(encoded), 256):
            batch = encoded[offset:offset + 256]
            try:
                checked = subprocess.run(
                    ["git", "-C", str(repo_root), "check-ignore", "-z", "--no-index", "--stdin"],
                    input=b"\0".join(batch) + b"\0", capture_output=True, timeout=5,
                )
                ignored_paths.update(relative[item] for item in checked.stdout.split(b"\0") if item in relative)
            except (OSError, subprocess.SubprocessError):
                continue
        return ignored_paths

    def raise_walk_error(error: OSError) -> None:
        raise error

    for root, dirs, files in os.walk(directory, followlinks=False, onerror=raise_walk_error):
        candidates = [Path(root) / name for name in dirs if name not in SKIP_DIRS]
        ignored_directories = ignored(candidates)
        dirs[:] = [path.name for path in candidates if path not in ignored_directories]
        files_to_check = [Path(root) / name for name in files
                          if name not in SKIP_DIRS and Path(name).suffix.casefold() not in SKIP_SUFFIXES]
        ignored_files = ignored(files_to_check)
        yield from (path for path in files_to_check if path not in ignored_files)


def scan_inputs(prompt: bytes, prompt_path: str, add_dirs: list[str] | None = None) -> ScanResult:
    result = ScanResult(scan_bytes(prompt, prompt_path))
    files_seen = 0
    bytes_seen = 0
    for raw_dir in add_dirs or []:
        for path in _files(Path(raw_dir).expanduser().resolve()):
            try:
                metadata = path.lstat()
                if not stat.S_ISREG(metadata.st_mode):
                    continue
                if metadata.st_size > MAX_FILE_BYTES:
                    with path.open("rb") as stream:
                        binary_probe = stream.read(8192)
                    if b"\0" in binary_probe or path.suffix.casefold() in SKIP_SUFFIXES:
                        continue
                    result.budget_exceeded = True
                    result.budget_path = str(Path(raw_dir).expanduser().resolve())
                    result.warnings.append("secret scan budget reached")
                    return result
                with path.open("rb") as stream:
                    content = stream.read(MAX_FILE_BYTES + 1)
                if len(content) > MAX_FILE_BYTES or b"\0" in content:
                    continue
                if files_seen >= MAX_FILES or bytes_seen + metadata.st_size > MAX_TOTAL_BYTES:
                    result.budget_exceeded = True
                    result.budget_path = str(Path(raw_dir).expanduser().resolve())
                    result.warnings.append("secret scan budget reached")
                    return result
                files_seen += 1
                bytes_seen += len(content)
                result.findings.extend(scan_bytes(content, str(path)))
            except FileNotFoundError:
                continue
            except OSError as exc:
                raise OSError(f"cannot scan additional file: {path}") from exc
    return result
