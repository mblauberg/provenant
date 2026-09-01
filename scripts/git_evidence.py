#!/usr/bin/env python3
"""Run read-only Git evidence commands without inherited routing state."""

from __future__ import annotations

import argparse
import codecs
import json
import os
import subprocess
import tempfile
from collections.abc import Mapping
from pathlib import Path


class GitEvidenceUntrackedError(ValueError):
    """The checkout contains untracked files with no packet content."""


class GitEvidenceChangedError(ValueError):
    """The checkout changed while its packet was being captured."""


def sanitized_git_environment(
    source: Mapping[str, str] | None = None,
) -> dict[str, str]:
    """Build the closed child environment used only for Git evidence reads."""
    inherited = os.environ if source is None else source
    environment = {
        name: inherited[name]
        for name in ("PATH", "HOME", "TMPDIR")
        if name in inherited
    }
    environment.update({
        "LC_ALL": "C",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_NO_REPLACE_OBJECTS": "1",
        "GIT_GRAFT_FILE": os.devnull,
        "GIT_NO_LAZY_FETCH": "1",
        "GIT_OPTIONAL_LOCKS": "0",
        "GIT_TERMINAL_PROMPT": "0",
    })
    return environment


def git_output(
    repository: Path, *args: str, text: bool = True,
    source_environment: Mapping[str, str] | None = None,
) -> str | bytes:
    """Return one fail-closed Git evidence read from the named repository."""
    return subprocess.run(
        ["git", "-C", str(repository), *args],
        check=True,
        capture_output=True,
        text=text,
        env=sanitized_git_environment(source_environment),
    ).stdout


def _text(value: str | bytes) -> str:
    return value if isinstance(value, str) else value.decode("utf-8", errors="replace")


def _untracked_paths(status: str | bytes) -> list[str]:
    """Return untracked paths from either regular or NUL-delimited porcelain."""
    raw = status if isinstance(status, bytes) else status.encode("utf-8")
    records = raw.split(b"\0") if b"\0" in raw else raw.splitlines()
    return [record[3:].decode("utf-8", errors="replace") for record in records if record.startswith(b"?? ")]


def _path_is_selected(untracked: str, selected: list[str]) -> bool:
    path = Path(untracked)
    return any(path == selected_path or selected_path in path.parents for selected_path in map(Path, selected))


def _filter_config_options(
    repository: Path,
    source_environment: Mapping[str, str] | None = None,
) -> list[str]:
    """Disable every effective repository clean/process filter for child reads."""
    try:
        configured = git_output(
            repository,
            "config",
            "--includes",
            "--null",
            "--get-regexp",
            r"^filter\..+\.(clean|process)$",
            source_environment=source_environment,
            text=False,
        )
    except subprocess.CalledProcessError as exc:
        if exc.returncode == 1:
            return []
        raise
    raw = configured if isinstance(configured, bytes) else configured.encode("utf-8")
    drivers: set[str] = set()
    for field in raw.split(b"\0"):
        key_bytes, separator, _value = field.partition(b"\n")
        if not separator:
            continue
        key = os.fsdecode(key_bytes)
        if key.startswith("filter.") and key.rsplit(".", 1)[-1] in {"clean", "process"}:
            drivers.add(key[len("filter."):].rsplit(".", 1)[0])
    options: list[str] = []
    for driver in sorted(drivers):
        options.extend(
            (
                "-c", f"filter.{driver}.clean=",
                "-c", f"filter.{driver}.process=",
                "-c", f"filter.{driver}.required=false",
            )
        )
    return options


def _git_diff_to_file(
    repository: Path, destination, args: list[str],
    source_environment: Mapping[str, str] | None = None,
    git_options: list[str] | None = None,
) -> None:
    """Stream a fixed Git diff into a packet without retaining it in memory."""
    process = subprocess.Popen(
        [
            "git", "-C", str(repository), "-c", "core.fsmonitor=false",
            *(git_options or []), "--literal-pathspecs", *args,
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=sanitized_git_environment(source_environment),
    )
    assert process.stdout is not None
    decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
    try:
        while chunk := process.stdout.read(64 * 1024):
            destination.write(decoder.decode(chunk).encode("utf-8"))
        destination.write(decoder.decode(b"", final=True).encode("utf-8"))
        stderr = process.stderr.read() if process.stderr is not None else b""
        return_code = process.wait()
    except BaseException:
        try:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    process.kill()
        finally:
            try:
                process.wait(timeout=2)
            except (OSError, subprocess.SubprocessError):
                pass
        raise
    finally:
        process.stdout.close()
        if process.stderr is not None:
            process.stderr.close()
    if return_code:
        detail = stderr.decode("utf-8", errors="replace").strip()
        raise subprocess.CalledProcessError(return_code, process.args, stderr=detail)


def _tracked_snapshot(
    repository: Path,
    diff_args: list[str],
    source_environment: Mapping[str, str] | None = None,
    git_options: list[str] | None = None,
) -> dict[str, tuple[int, int, int, int, int] | None]:
    """Capture lightweight filesystem identity for files included by the diff."""
    names = git_output(
        repository,
        "-c",
        "core.fsmonitor=false",
        *(git_options or []),
        "--literal-pathspecs",
        "diff",
        "--name-only",
        "-z",
        *diff_args[1:],
        source_environment=source_environment,
        text=False,
    )
    raw_names = names if isinstance(names, bytes) else names.encode("utf-8")
    result: dict[str, tuple[int, int, int, int, int] | None] = {}
    for raw_name in raw_names.split(b"\0"):
        if not raw_name:
            continue
        name = os.fsdecode(raw_name)
        path = repository / name
        try:
            stat_result = path.lstat()
        except FileNotFoundError:
            result[name] = None
            continue
        result[name] = (
            stat_result.st_dev,
            stat_result.st_ino,
            stat_result.st_mode,
            stat_result.st_size,
            stat_result.st_mtime_ns,
        )
    return result


def materialise_packet(
    repository: Path,
    output: Path,
    *,
    diff_from: str,
    paths: list[str] | None = None,
    source_environment: Mapping[str, str] | None = None,
) -> dict[str, object]:
    """Write one human-readable packet for an exact checkout and selected diff.

    Only the fixed Git reads below are used.  In particular, caller Git routing
    variables cannot redirect a linked worktree to another repository.
    """
    root = repository.expanduser().resolve()
    if not root.is_dir():
        raise ValueError(f"repository is not a directory: {repository}")
    if not diff_from or "\x00" in diff_from or diff_from.startswith("-"):
        raise ValueError("diff-from must be a non-empty Git revision")
    selected_paths = paths or []
    for path in selected_paths:
        if not path or "\x00" in path or path.startswith("-") or Path(path).is_absolute():
            raise ValueError(f"diff path must be a relative path: {path!r}")

    head = _text(
        git_output(
            root,
            "rev-parse",
            "--verify",
            "HEAD^{commit}",
            source_environment=source_environment,
            text=False,
        )
    ).strip()
    top_level = _text(
        git_output(root, "rev-parse", "--show-toplevel", source_environment=source_environment, text=False)
    ).strip()
    resolved_base = _text(
        git_output(
            root,
            "rev-parse",
            "--verify",
            f"{diff_from}^{{commit}}",
            source_environment=source_environment,
            text=False,
        )
    ).strip()
    filter_options = _filter_config_options(root, source_environment)
    status_args = [
        "-c", "core.fsmonitor=false", *filter_options, "--literal-pathspecs", "status",
        "--porcelain=v1", "--untracked-files=all", "-z",
    ]
    status_bytes = git_output(root, *status_args, source_environment=source_environment, text=False)
    status = _text(status_bytes).replace("\x00", "\n")
    untracked = _untracked_paths(status_bytes)
    if (not selected_paths and untracked) or any(
        _path_is_selected(path, selected_paths) for path in untracked
    ):
        raise GitEvidenceUntrackedError(
            "git_evidence_untracked: checkout contains untracked paths without packet content"
        )
    diff_args = [
        "diff", "--no-ext-diff", "--no-textconv", "--no-color", "--binary",
        resolved_base, "--", *selected_paths,
    ]
    tracked_before = _tracked_snapshot(root, diff_args, source_environment, filter_options)
    metadata = {
        "schema_version": 1,
        "record_type": "provenant-git-evidence",
        "repository": str(root),
        "git_root": top_level,
        "head": head,
        "diff_base": resolved_base,
        "working_tree": "dirty" if status else "clean",
        "diff_from": diff_from,
        "paths": selected_paths,
        "encoding": "utf-8-replacement",
    }
    destination = output.expanduser()
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary_name = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="wb", dir=destination.parent, prefix=f".{destination.name}.", delete=False
        ) as temporary:
            temporary_name = temporary.name
            temporary.write(json.dumps(metadata, sort_keys=True, separators=(",", ":")).encode("utf-8"))
            temporary.write(b"\n--- status ---\n")
            temporary.write(status.encode("utf-8"))
            temporary.write(b"--- diff ---\n")
            _git_diff_to_file(root, temporary, diff_args, source_environment, filter_options)
            temporary.flush()
            os.fsync(temporary.fileno())
        final_head = _text(
            git_output(
                root,
                "rev-parse",
                "--verify",
                "HEAD^{commit}",
                source_environment=source_environment,
                text=False,
            )
        ).strip()
        final_status_bytes = git_output(root, *status_args, source_environment=source_environment, text=False)
        tracked_after = _tracked_snapshot(root, diff_args, source_environment, filter_options)
        if final_head != head or final_status_bytes != status_bytes or tracked_after != tracked_before:
            raise GitEvidenceChangedError(
                "git_evidence_changed: checkout HEAD or status changed during capture; retry"
            )
        os.replace(temporary_name, destination)
    except BaseException:
        if temporary_name:
            Path(temporary_name).unlink(missing_ok=True)
        raise
    return metadata


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--diff-from", required=True, help="explicit Git revision to compare against")
    parser.add_argument("--path", action="append", default=[], help="optional literal relative workspace path to include")
    args = parser.parse_args(argv)
    try:
        metadata = materialise_packet(
            args.repository, args.output, diff_from=args.diff_from, paths=args.path
        )
    except (OSError, subprocess.CalledProcessError, ValueError) as exc:
        parser.error(str(exc))
    print(json.dumps(metadata, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
