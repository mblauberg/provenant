#!/usr/bin/env python3
"""Fail closed on common mistakes before this harness is published."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from collections.abc import Iterable, Mapping, Sequence
from pathlib import Path

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
    "LICENSES/skill-optimizer-MIT.txt",
    "LICENSES/ui-ux-pro-max-MIT.txt",
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
    "skills/academic-writing/",  # retired skill; absorbed by natural/engineering-writing
    "skills/humanise-text/",  # retired name; capability moved to natural-writing
    "skills/tanstack-query-best-practices/",  # retired local skill must not return
    "skills/vercel-react-best-practices/",  # retired vendor-branded skill name
    "skills/playwright/",  # moved to the personal ~/Repos/skills catalogue
    "skills/react-performance/",  # moved to the personal ~/Repos/skills catalogue
    "skills/tanstack-query/",  # moved to the personal ~/Repos/skills catalogue
    "skills/typescript-clean-code/",  # moved to the personal ~/Repos/skills catalogue
    "skills/uml-diagrams/",  # moved to the personal ~/Repos/skills catalogue
    "skills/web-stack-conventions/",  # retired; deltas rot on a fixed schedule
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
HOME_PATH_BYTES = re.compile(HOME_PATH.pattern.encode("ascii"))
SECRET_BYTE_PATTERNS = {
    label: re.compile(pattern.pattern.encode("ascii"))
    for label, pattern in SECRET_PATTERNS.items()
}

# Read-only evidence commands must not honour replacement refs or hand work to a
# repository-configured filesystem monitor.
GIT_SAFE_CONFIG: tuple[str, ...] = (
    "-c", "core.useReplaceRefs=false", "-c", "core.fsmonitor=false",
)
OID = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})")
MAX_PUBLIC_FILE_BYTES = 5 * 1024 * 1024
# `git grep` is invoked over batches of blob names so the argument vector stays
# well inside the platform limit on repositories with a long history.
GREP_BATCH_SIZE = 1000

# The registry above is the single source of truth for what counts as a
# finding. `git grep` only ever acts as a prefilter, so its patterns are a
# deliberate superset of the Python ones and every matched line is reclassified
# with the registry pattern itself.
FINDING_PATTERNS: dict[str, re.Pattern[bytes]] = {
    "personal absolute home path": HOME_PATH_BYTES,
    **{
        f"possible {label}": pattern
        for label, pattern in SECRET_BYTE_PATTERNS.items()
    },
}


def grep_pattern(pattern: re.Pattern[str] | re.Pattern[bytes]) -> str:
    """Widen one registry pattern into POSIX ERE for the `git grep` prefilter.

    Non-capturing groups become capturing ones and word boundaries are dropped,
    both of which only ever widen the match, so the prefilter cannot lose a hit
    that the registry pattern would have found.
    """
    source = pattern.pattern
    if isinstance(source, bytes):
        source = source.decode("ascii")
    return source.replace("(?:", "(").replace(chr(92) + "b", "")


GREP_ARGUMENTS: tuple[str, ...] = tuple(
    argument
    for pattern in FINDING_PATTERNS.values()
    for argument in ("-e", grep_pattern(pattern))
)


def git(*args: str, root: Path = ROOT) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *GIT_SAFE_CONFIG, *args], cwd=root, text=True, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, check=False, env=sanitized_git_environment(),
    )


def git_bytes(*args: str, root: Path = ROOT) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        ["git", *GIT_SAFE_CONFIG, *args], cwd=root, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, check=False, env=sanitized_git_environment(),
    )


def require_bytes(*args: str, root: Path = ROOT, fallback: str) -> bytes:
    result = git_bytes(*args, root=root)
    if result.returncode:
        raise RuntimeError(
            result.stderr.decode("utf-8", errors="replace").strip() or fallback
        )
    return result.stdout


def resolve_commit(revision: str, root: Path = ROOT) -> str:
    """Resolve one revision to a commit id, refusing anything option-shaped."""
    result = git("rev-parse", "--verify", "--end-of-options", revision, root=root)
    resolved = result.stdout.strip().lower()
    if result.returncode or OID.fullmatch(resolved) is None:
        detail = result.stderr.strip() or "revision does not resolve to one object"
        raise RuntimeError(f"cannot resolve publication endpoint {revision!r}: {detail}")
    kind = git("cat-file", "-t", resolved, root=root)
    if kind.returncode or kind.stdout.strip() != "commit":
        raise RuntimeError(
            f"publication endpoint {revision!r} does not name a commit"
        )
    return resolved


def tracked_files(root: Path = ROOT) -> list[str]:
    result = git("ls-files", "-z", root=root)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "git ls-files failed")
    return [value for value in result.stdout.split("\0") if value]


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
        if path.stat().st_size > MAX_PUBLIC_FILE_BYTES:
            errors.append(f"tracked file exceeds 5 MiB: {relative}")
            continue
        try:
            text = path.read_text()
        except (UnicodeDecodeError, OSError):
            continue
        if HOME_PATH.search(text):
            errors.append(f"personal absolute home path: {relative}")
        for label, pattern in SECRET_PATTERNS.items():
            if pattern.search(text):
                errors.append(f"possible {label}: {relative}")
    return errors


def is_forbidden(relative: str) -> bool:
    return relative in FORBIDDEN_TRACKED or any(
        relative.startswith(prefix) for prefix in FORBIDDEN_PREFIXES
    )


TreeEntry = tuple[str, str, str, int | None, str]


def tree_entries(revision: str, root: Path = ROOT) -> tuple[TreeEntry, ...]:
    """Read one revision's whole tree as (mode, kind, object id, size, path)."""
    raw = require_bytes(
        "ls-tree", "-r", "-l", "-z", revision, root=root,
        fallback=f"publication tree enumeration failed for {revision}",
    )
    entries: list[TreeEntry] = []
    for record in raw.split(b"\0"):
        if not record:
            continue
        try:
            header, relative = record.decode("utf-8").split("\t", 1)
        except (UnicodeDecodeError, ValueError) as exc:
            raise RuntimeError("publication tree enumeration is malformed") from exc
        fields = header.split()
        if len(fields) != 4:
            raise RuntimeError("publication tree enumeration is malformed")
        mode, kind, object_id, raw_size = fields
        size = None if raw_size == "-" else int(raw_size)
        entries.append((mode, kind, object_id, size, relative))
    return tuple(entries)


def reachable_objects(
    revision_arguments: Sequence[str], root: Path = ROOT,
) -> tuple[str, ...]:
    """List every object reachable from the named revisions.

    This is the `git rev-list --all` walk that replaced the hand-rolled object
    reader: it yields each reachable object exactly once, so the `git grep`
    below scans a blob once rather than once per commit that carries it.
    """
    raw = require_bytes(
        "rev-list", "--objects", *revision_arguments, root=root,
        fallback="publication object enumeration failed",
    )
    object_ids: dict[str, None] = {}
    for line in raw.splitlines():
        object_id, _, _ = line.partition(b" ")
        object_ids[object_id.decode("ascii")] = None
    return tuple(object_ids)


def history_paths(
    revision_arguments: Sequence[str], root: Path = ROOT,
) -> set[str]:
    """List every tracked path that appears anywhere in the named revisions.

    `git rev-list --objects` cannot answer this: it reports each object once, so
    a blob tracked at two paths only ever names one of them. Walking the commits
    for their changed paths is exact whenever the walk is closed under ancestry,
    as `--all` is: the earliest commit whose tree carries a path is itself in the
    walk, and shows that path as added. A range is not closed under ancestry, so
    `range_paths` reads the selected trees instead.
    """
    raw = require_bytes(
        "log", *revision_arguments, "--format=", "--name-only", "-z",
        "--diff-merges=first-parent", root=root,
        fallback="publication path enumeration failed",
    )
    return {
        record.decode("utf-8", errors="replace")
        for record in raw.split(b"\0")
        if record
    }


def range_paths(commits: Sequence[str], root: Path = ROOT) -> set[str]:
    """List every path tracked by the selected commits, reading their trees.

    A range excludes the commits that introduced paths inherited from before the
    base, so the changed-path walk `history_paths` uses would miss them. Trees
    are deduplicated first, so commits that left the tree untouched cost nothing.
    """
    revisions = require_bytes(
        "rev-list", "--no-walk", "--format=%T", *commits, root=root,
        fallback="publication range tree enumeration failed",
    ).decode("ascii").split()
    trees = dict.fromkeys(
        revision for revision in revisions if revision != "commit"
    )
    paths: set[str] = set()
    for tree in trees:
        raw = require_bytes(
            "ls-tree", "-r", "--name-only", "-z", tree, root=root,
            fallback=f"publication range tree enumeration failed for {tree}",
        )
        paths.update(
            record.decode("utf-8", errors="replace")
            for record in raw.split(b"\0")
            if record
        )
    return paths


def forbidden_paths(paths: Iterable[str]) -> list[str]:
    return sorted({path for path in paths if is_forbidden(path)})


def blob_ids(object_ids: Iterable[str], root: Path = ROOT) -> tuple[str, ...]:
    """Keep only the blobs, so `git grep` never re-walks a tree."""
    candidates = list(object_ids)
    if not candidates:
        return ()
    result = subprocess.run(
        ["git", *GIT_SAFE_CONFIG, "cat-file", "--batch-check"], cwd=root,
        input="\n".join(candidates).encode("ascii") + b"\n",
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
        env=sanitized_git_environment(),
    )
    if result.returncode:
        raise RuntimeError(
            result.stderr.decode("utf-8", errors="replace").strip()
            or "publication object type enumeration failed"
        )
    blobs: list[str] = []
    for line in result.stdout.split(b"\n"):
        fields = line.split()
        if len(fields) == 3 and fields[1] == b"blob":
            blobs.append(fields[0].decode("ascii"))
    return tuple(blobs)


def classify(line: bytes) -> frozenset[str]:
    return frozenset(
        label for label, pattern in FINDING_PATTERNS.items() if pattern.search(line)
    )


def grep_findings(
    blobs: Sequence[str],
    root: Path = ROOT,
    *,
    batch_size: int = GREP_BATCH_SIZE,
) -> dict[str, frozenset[str]]:
    """Map blob id to findings using `git grep` over the reachable blobs."""
    findings: dict[str, set[str]] = {}
    for start in range(0, len(blobs), batch_size):
        batch = blobs[start:start + batch_size]
        result = git_bytes(
            "grep", "-a", "--no-textconv", "-n", "-E", *GREP_ARGUMENTS, *batch,
            root=root,
        )
        if result.returncode not in (0, 1):
            raise RuntimeError(
                result.stderr.decode("utf-8", errors="replace").strip()
                or "publication history scan failed"
            )
        for line in result.stdout.split(b"\n"):
            object_id, separator, remainder = line.partition(b":")
            if not separator:
                continue
            _, _, text = remainder.partition(b":")
            labels = classify(text)
            if labels:
                findings.setdefault(object_id.decode("ascii"), set()).update(labels)
    return {object_id: frozenset(labels) for object_id, labels in findings.items()}


def publication_tree_errors(
    entries: Sequence[TreeEntry],
    findings: Mapping[str, frozenset[str]],
) -> list[str]:
    entry_errors: list[str] = []
    required_kinds: dict[str, str] = {}
    for mode, kind, object_id, size, relative in entries:
        if relative in REQUIRED:
            required_kinds[relative] = kind
        if is_forbidden(relative):
            entry_errors.append(f"publication HEAD forbidden tracked path: {relative}")
            continue
        if mode == "120000":
            entry_errors.append(
                f"publication HEAD tracked symlink is not portable: {relative}"
            )
            continue
        if kind != "blob":
            continue
        if size is not None and size > MAX_PUBLIC_FILE_BYTES:
            entry_errors.append(
                f"publication HEAD tracked file exceeds 5 MiB: {relative}"
            )
            continue
        blob_findings = findings.get(object_id, frozenset())
        if "personal absolute home path" in blob_findings:
            entry_errors.append(
                f"publication HEAD personal absolute home path: {relative}"
            )
        for label in SECRET_PATTERNS:
            if f"possible {label}" in blob_findings:
                entry_errors.append(f"publication HEAD possible {label}: {relative}")
    missing_errors = [
        f"publication HEAD missing required public file: {required}"
        for required in sorted(REQUIRED)
        if required_kinds.get(required) != "blob"
    ]
    return [*missing_errors, *entry_errors]


def text_findings(raw: bytes) -> set[str]:
    return {
        label for label, pattern in FINDING_PATTERNS.items() if pattern.search(raw)
    }


def commit_metadata(
    revision_arguments: Sequence[str], root: Path = ROOT,
) -> tuple[set[str], set[str]]:
    """Return commit-message findings and the author/committer email set."""
    raw = require_bytes(
        "log", *revision_arguments, "--format=%ae%n%ce%n%B%x00", root=root,
        fallback="publication commit enumeration failed",
    )
    findings: set[str] = set()
    emails: set[str] = set()
    for record in raw.split(b"\0"):
        lines = record.lstrip(b"\n").split(b"\n")
        if len(lines) < 2:
            continue
        emails.add(lines[0].decode("utf-8", errors="replace").strip())
        emails.add(lines[1].decode("utf-8", errors="replace").strip())
        findings.update(text_findings(b"\n".join(lines[2:])))
    return findings, emails


def annotated_tag_metadata(root: Path = ROOT) -> tuple[set[str], set[str]]:
    """Return annotated-tag message findings and the tagger email set."""
    raw = require_bytes(
        "for-each-ref", "--format=%(objecttype)%00%(taggeremail)%00%(contents)%00%00",
        "refs/tags", root=root, fallback="publication tag enumeration failed",
    )
    findings: set[str] = set()
    emails: set[str] = set()
    for record in raw.split(b"\0\0"):
        fields = record.lstrip(b"\n").split(b"\0")
        if len(fields) < 3 or fields[0] != b"tag":
            continue
        emails.add(
            fields[1].decode("utf-8", errors="replace").strip().strip("<>").strip()
        )
        findings.update(text_findings(fields[2]))
    return findings, emails


def personal_email_errors(emails: Iterable[str], prefix: str) -> list[str]:
    return [
        f"{prefix} exposes a personal email: {email}"
        for email in sorted({value.strip() for value in emails if value.strip()})
        if PERSONAL_EMAIL.search(email.strip())
    ]


def history_errors(root: Path = ROOT) -> list[str]:
    try:
        current_errors = scan_paths(tracked_files(root), root)
        head = resolve_commit("HEAD", root)
        findings = grep_findings(
            blob_ids(reachable_objects(["--all"], root), root), root,
        )
        paths = history_paths(["--all"], root)
        head_errors = publication_tree_errors(tree_entries(head, root), findings)
        message_findings, emails = commit_metadata(["--all"], root)
        tag_findings, tagger_emails = annotated_tag_metadata(root)
    except (RuntimeError, OSError) as exc:
        return [str(exc)]
    content_findings: set[str] = set()
    for labels in findings.values():
        content_findings.update(labels)
    errors = [*current_errors, *head_errors]
    errors.extend(
        f"reachable history contains a forbidden tracked path: {relative}"
        for relative in forbidden_paths(paths)
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
        for finding in sorted(tag_findings)
    )
    errors.extend(personal_email_errors(emails | tagger_emails, "reachable history"))
    return errors


def publication_range_errors(
    base_revision: str, head_revision: str, root: Path = ROOT,
) -> list[str]:
    try:
        base = resolve_commit(base_revision, root)
        head = resolve_commit(head_revision, root)
        ancestry = git("merge-base", "--is-ancestor", base, head, root=root)
        if ancestry.returncode == 1:
            return ["publication range base is not an ancestor of head"]
        if ancestry.returncode:
            raise RuntimeError(
                ancestry.stderr.strip() or "publication range resolution failed"
            )
        selected = require_bytes(
            "rev-list", f"{base}..{head}", root=root,
            fallback="publication range enumeration failed",
        ).decode("ascii").split()
        if not selected:
            return ["publication range must contain at least one commit"]
        findings = grep_findings(
            blob_ids(reachable_objects(["--no-walk", *selected], root), root), root,
        )
        paths = range_paths(selected, root)
        errors = publication_tree_errors(tree_entries(head, root), findings)
        message_findings, emails = commit_metadata([f"{base}..{head}"], root)
    except (RuntimeError, OSError) as exc:
        return [str(exc)]
    content_findings: set[str] = set()
    for labels in findings.values():
        content_findings.update(labels)
    errors.extend(
        f"publication range contains a forbidden tracked path: {relative}"
        for relative in forbidden_paths(paths)
    )
    errors.extend(
        f"publication range contains a {finding}"
        for finding in sorted(content_findings)
    )
    errors.extend(
        f"publication range commit message contains a {finding}"
        for finding in sorted(message_findings)
    )
    errors.extend(personal_email_errors(emails, "publication range"))
    return errors


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
