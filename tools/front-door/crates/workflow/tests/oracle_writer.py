"""Differential oracles for workflow's writer and Herdr policy ports."""
import importlib.util
import contextlib
import io
import json
import os
from pathlib import Path
import sys
import tempfile
from unittest.mock import patch

root, unit = Path(sys.argv[1]), sys.argv[2]
sources = {
    "writer_branch": ("skills/orchestrate/scripts/writer_lease.py", "writer_lease_oracle"),
    "writer_retry": ("skills/orchestrate/scripts/writer_lease.py", "writer_retry_oracle"),
    "writer_unchanged": ("skills/orchestrate/scripts/writer_lease.py", "writer_unchanged_oracle"),
    "writer_modified_since": ("skills/orchestrate/scripts/writer_lease.py", "writer_modified_since_oracle"),
    "writer_git_env": ("skills/orchestrate/scripts/writer_lease.py", "writer_git_env_oracle"),
    "git_evidence_packet": ("skills/orchestrate/scripts/git_evidence_packet.py", "git_evidence_packet_oracle"),
    "git_evidence_header": ("skills/orchestrate/scripts/git_evidence_packet.py", "git_evidence_header_oracle"),
    "herdr_policy": ("skills/orchestrate/scripts/check_herdr_cli.py", "check_herdr_cli_oracle"),
    "herdr_args": ("skills/orchestrate/scripts/check_herdr_cli.py", "check_herdr_cli_args_oracle"),
}
source, name = sources[unit]
spec = importlib.util.spec_from_file_location(name, root / source)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
cases = json.load(sys.stdin)
results = []


def type_error(error):
    text = str(error)
    kind = "list" if "'list'" in text else "dict" if "'dict'" in text else None
    if kind is None:
        raise error
    return {"error_type": "TypeError", "unhashable_type": kind}


def state(value):
    if value is None:
        return None
    return value["head"], bytes(value["porcelain"])


for case in cases:
    if unit == "writer_branch":
        results.append(
            None
            if len(case) <= 48 and module.BRANCH_PATTERN.fullmatch(case)
            else f"branch {case} is outside <type>/<area>-<slug> (or land/, proto/); rename for repository convention"
        )
    elif unit == "writer_retry":
        record = {"status": case["status"], "retryable": True}
        current = state(case["current"])
        module.writer_worktree_state = lambda _cwd, value=current: value
        module.modified_since = lambda _cwd, _since, changed=case["modified"]: changed
        start_state = state(case["start"])
        start = (start_state, 100.0) if start_state is not None else None
        try:
            module.guard_writer_retry(record, start, "/fixture")
            results.append({"record": record})
        except TypeError as error:
            results.append(type_error(error))
    elif unit == "writer_unchanged":
        current = state(case["current"])
        module.writer_worktree_state = lambda _cwd, value=current: value
        start_state = state(case["start"])
        results.append(module.worktree_unchanged(start_state, "/fixture"))
    elif unit == "writer_modified_since":
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "work"
            kind = case["kind"]
            if kind != "missing":
                root.mkdir()
                if kind == "file_changed":
                    target = root / "work.txt"
                    target.write_text("fixture", encoding="utf-8")
                    os.utime(target, (5_000_000_000, 5_000_000_000))
                elif kind == "skipped_directory":
                    ignored = root / ".git"
                    ignored.mkdir()
                    target = ignored / "work.txt"
                    target.write_text("fixture", encoding="utf-8")
                    os.utime(target, (5_000_000_000, 5_000_000_000))
                    os.utime(ignored, (0, 0))
                os.utime(root, (0, 0))
            results.append(module.modified_since(root, case["since"]))
    elif unit == "writer_git_env":
        with tempfile.TemporaryDirectory() as directory:
            worktree = Path(directory) / "linked"
            worktree.mkdir()
            git_dir = case["git_dir"]
            common_dir = case["common_dir"]
            modes = []

            def fake_git(args, **kwargs):
                explicit_env = kwargs.get("env")
                clears_git = explicit_env is not None and not any(
                    key.startswith("GIT_") for key in explicit_env
                )
                modes.append("clear" if clears_git else "inherit")
                command = "list" if args[-4:-2] == ["worktree", "list"] else args[-1]
                if command == "--show-toplevel":
                    stdout = str(worktree) + "\n"
                elif command == "list":
                    stdout = f"worktree {worktree}\0".encode()
                elif command in ("--absolute-git-dir", "--git-dir"):
                    stdout = (common_dir if clears_git else git_dir) + "\n"
                elif command == "--git-common-dir":
                    stdout = common_dir + "\n"
                else:
                    raise AssertionError(f"unexpected Git command: {args!r}")
                return module.subprocess.CompletedProcess(
                    args, 0, stdout, b"" if isinstance(stdout, bytes) else ""
                )

            with patch.dict(
                os.environ,
                {"GIT_DIR": git_dir, "GIT_WORK_TREE": str(worktree)},
                clear=True,
            ), patch.object(module.subprocess, "run", side_effect=fake_git):
                try:
                    module.resolve_writer_worktree(worktree)
                    accepted = True
                except module.WorktreeLeaseError:
                    accepted = False
            results.append({"accepted": accepted, "modes": modes})
    elif unit == "git_evidence_packet":
        scanner = module.PacketScanner(
            ValueError, case["limit"], validate_on_feed=case["validate"]
        )
        try:
            for chunk in case["chunks"]:
                scanner.feed(bytes(chunk))
            results.append(
                {
                    "metadata": scanner.finish(),
                    "status": scanner.has_status,
                    "diff": scanner.has_diff,
                }
            )
        except ValueError as error:
            results.append({"error": str(error)})
    elif unit == "git_evidence_header":
        try:
            results.append(module.validate_header(case))
        except TypeError as error:
            results.append(type_error(error))
    elif unit == "herdr_policy":
        operation = case["op"]
        try:
            if operation == "commands":
                result = sorted(module._documented_commands(case["reference"]))
            elif operation == "has_command":
                result = module._has_command(case["surface"], case["command"])
            elif operation == "documented":
                module._validate_documented_commands(case["reference"], case["surfaces"])
                result = None
            elif operation == "statuses":
                module._validate_statuses(case["reference"], case["surfaces"])
                result = None
            elif operation == "version":
                outputs = iter(case["outputs"])

                def fixture_invoke(_binary, args):
                    output = next(outputs)
                    if output["return_code"] != 0:
                        command = " ".join(("herdr", *args))
                        detail = (output["stderr"] or output["stdout"]).strip()
                        raise module.ContractError(
                            f"non-mutating command failed: {command}: {detail}"
                        )
                    return output["stdout"] + output["stderr"]

                module._invoke = fixture_invoke
                with tempfile.TemporaryDirectory() as directory:
                    reference_path = Path(directory) / "herdr-panes.md"
                    reference_path.write_text(case["reference"], encoding="utf-8")
                    result = module.check("herdr", reference_path)
            else:
                raise ValueError(f"unknown operation: {operation}")
            results.append({"ok": result})
        except module.ContractError as error:
            results.append({"error": str(error)})
    elif unit == "herdr_args":
        stdout, stderr = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            sys.argv[0] = "check_herdr_cli.py"
            try:
                code = module.main(case["argv"])
            except SystemExit as error:
                code = error.code if isinstance(error.code, int) else 0
        results.append({"exit_code": code, "stdout": stdout.getvalue(), "stderr": stderr.getvalue()})

json.dump(results, sys.stdout)
