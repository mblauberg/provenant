#!/usr/bin/env python3
"""Behaviour tests for cf_dispatch.sh with stubbed CLIs."""
import json
import os
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import textwrap
import time
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from _shared.worker_outcome import accept_worker_outcome
from orchestrate.scripts import run_dir_finalize


HERE = Path(__file__).resolve().parent
PRODUCT_ROOT = HERE.parents[2]
SCRIPT = HERE.parent / "scripts" / "cf_dispatch.sh"
PUBLISH_HELPER = HERE.parent / "scripts" / "cf_dispatch_publish.py"
RUN_DIR_SCRIPT = HERE.parent / "scripts" / "run_dir_init.sh"
DISPATCH_SCHEMA = {
    "id",
    "attempt_id",
    "tool",
    "adapter",
    "model",
    "requested_model",
    "fallback_model",
    "effort",
    "requested_effort",
    "effort_source",
    "effort_capability_source",
    "effort_substitution",
    "substitution",
    "status",
    "exit",
    "terminal_observed",
    "output_path",
    "output_sha256",
    "terminal_artifact_path",
    "terminal_artifact_sha256",
    "read_only_guarantee",
    "orchestrator_family",
    "provider_family",
    "endpoint_provider",
    "model_family",
    "resolved_model",
    "identity_source",
    "catalog_model",
    "model_selection",
    "route_alias",
    "reviewer_id",
    "risk_tier",
    "policy_override",
    "certification_eligible",
    "cross_family",
}
REQUIRED_GATE_ROWS = [
    "P0/P1 findings triaged or explicitly deferred",
    "status=ok, cross_family=true, and read_only_guarantee=enforced/oauth_safe_mode",
    "CROSS-FAMILY-NOT-RUN reasons recorded",
    "Advisory cross-family findings triaged and either verified or rejected",
    "Document update wave run or explicitly N/A",
    "Updated docs verified against current source/artifacts",
]


def write_executable(path, body):
    path.write_text(textwrap.dedent(body), encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def fabric_free_env():
    # Stripping the fabric variables stops an inherited developer instance
    # from steering these evals through an installed provenant command.
    return {
        key: value
        for key, value in os.environ.items()
        if key != "AGENTS_HOME" and not key.startswith("AGENT_FABRIC_")
    }


def run_dispatch_with_stub(
    stub,
    role="reviewer",
    extra_args=None,
    provenant_stub=None,
    output_path=None,
):
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "claude", stub)
        if provenant_stub is not None:
            write_executable(bin_dir / "provenant", provenant_stub)
        out = Path(output_path) if output_path is not None else tmp / "out.txt"
        # PATH precedence keeps the checkout's stubs first.
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        command = [
                str(SCRIPT),
                "--tool",
                "claude",
                "--orchestrator-family",
                "codex",
                "--role",
                role,
                "--out",
                str(out),
                "--prompt",
                "Reply exactly OK",
            ]
        extra = list(extra_args or [])
        if "--evidence-root" not in extra:
            # The dispatcher no longer derives its own boundary, so every caller
            # states one. Tests that assert on containment pass their own.
            extra.extend(["--evidence-root", str(out.parent)])
        command.extend(extra)
        result = subprocess.run(
            command,
            cwd=str(tmp),
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        return result, record, out.read_text(encoding="utf-8") if out.exists() else ""


def test_invalid_routing_output_returns_a_structured_failure_record():
    stub = """\
        #!/usr/bin/env bash
        echo "provider must not run" >&2
        exit 9
    """
    result, record, _ = run_dispatch_with_stub(
        stub,
        provenant_stub="""\
            #!/usr/bin/env bash
            exit 127
        """,
    )

    assert result.returncode != 0
    assert record["status"] == "routing_record_invalid"
    assert record["read_only_guarantee"] == "none"


def test_claude_other_primary_uses_opus_without_implicit_fable_route():
    stub = """\
        #!/usr/bin/env bash
        model=""
        while [ $# -gt 0 ]; do
          if [ "$1" = "--model" ]; then model="$2"; shift 2; else shift; fi
        done
        cat >/dev/null
        [ "$model" = "opus" ] || exit 9
        echo "OPUS OK"
    """
    result, record, output = run_dispatch_with_stub(stub, role="other-primary")
    assert result.returncode == 0, result.stderr
    assert record["resolved_model"] == "opus"
    assert record["requested_model"] == "opus"
    assert record["fallback_model"] == ""
    assert record["identity_source"] == "dated-catalog"
    assert record["substitution"] == ""
    assert output.strip() == "OPUS OK"


def test_claude_crucial_synthesis_dispatches_explicit_fable_override():
    stub = """\
        #!/usr/bin/env bash
        model=""
        while [ $# -gt 0 ]; do
          if [ "$1" = "--model" ]; then model="$2"; shift 2; else shift; fi
        done
        cat >/dev/null
        [ "$model" = "fable" ] || exit 9
        echo "FABLE OK"
    """
    result, record, output = run_dispatch_with_stub(
        stub,
        role="synthesis",
        extra_args=["--risk-tier", "crucial", "--model", "fable", "--effort", "medium"],
    )
    assert result.returncode == 0, result.stderr
    assert record["resolved_model"] == "fable"
    assert record["risk_tier"] == "crucial"
    assert record["policy_override"] == "crucial-fable-synthesis-adjudication"
    assert output.strip() == "FABLE OK"


def test_reviewer_id_round_trips_into_dispatch_receipt():
    stub = """\
        #!/usr/bin/env bash
        cat >/dev/null
        echo "OK"
    """
    result, record, output = run_dispatch_with_stub(
        stub, extra_args=["--reviewer-id", "reviewer-1"]
    )
    assert result.returncode == 0, result.stderr
    assert record["reviewer_id"] == "reviewer-1"
    assert output.strip() == "OK"


def test_dispatch_receipt_owns_terminal_fact_and_output_digest():
    stub = """\
        #!/usr/bin/env bash
        cat >/dev/null
        printf 'OK\\n'
    """
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "claude", stub)
        out = tmp / "out.txt"
        receipt_path = tmp / "dispatch.json"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT), "--tool", "claude", "--orchestrator-family", "codex",
                "--task-id", "task-1", "--attempt-id", "attempt-2", "--receipt", str(receipt_path),
                "--out", str(out), "--prompt", "Reply exactly OK",
                "--evidence-root", str(tmp),
            ],
            cwd=str(tmp), env=env, text=True, capture_output=True,
        )
        record = json.loads(result.stdout)
        persisted = json.loads(receipt_path.read_text(encoding="utf-8"))
        assert result.returncode == 0, result.stderr
        assert persisted == record
        assert record["id"] == "task-1"
        assert record["attempt_id"] == "attempt-2"
        assert record["terminal_observed"] is True
        assert record["output_path"] == str(out)
        assert record["output_sha256"].startswith("sha256:")


def test_documented_workflow_separates_answer_from_terminal_artifact_and_joins_end_to_end():
    stub = """\
        #!/usr/bin/env bash
        cat >/dev/null
        printf 'Human answer\\n'
    """
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "claude", stub)
        answer = tmp / "crossfamily" / "review.out.txt"
        terminal = tmp / "crossfamily" / "review.terminal.json"
        worker_terminal = tmp / "crossfamily" / "review.worker.json"
        receipt_path = tmp / "crossfamily" / "review.route.json"
        answer.parent.mkdir()
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT), "--tool", "claude", "--orchestrator-family", "codex",
                "--task-id", "review-1", "--attempt-id", "attempt-1",
                "--receipt", str(receipt_path), "--out", str(answer),
                "--terminal-artifact", str(terminal), "--prompt", "Reply exactly OK",
                "--evidence-root", str(tmp),
            ],
            cwd=str(tmp), env=env, text=True, capture_output=True,
        )

        record = json.loads(result.stdout)
        persisted = json.loads(receipt_path.read_text(encoding="utf-8"))
        terminal_value = json.loads(terminal.read_text(encoding="utf-8"))
        worker_terminal.write_text(json.dumps({
            "id": "review-1", "attempt_id": "attempt-1", "kind": "complete",
            "summary": "normalised worker result", "verdict": "pass",
        }), encoding="utf-8")
        accepted = accept_worker_outcome(tmp, {
            "id": "review-1",
            "dispatch_receipt": {"path": "crossfamily/review.route.json", "digest": _digest(receipt_path)},
            "terminal_artifact": {"path": "crossfamily/review.worker.json", "digest": _digest(worker_terminal)},
            "dispatch_terminal_artifact": {"path": "crossfamily/review.terminal.json", "digest": _digest(terminal)},
            "worktree_receipt": None,
        })
        finalizer_error, finalizer_leg = run_dir_finalize._direct_worker_leg(
            tmp,
            {"id": "review-1", "verdict": "pass"},
            {"path": "crossfamily/review.route.json", "digest": _digest(receipt_path)},
            record,
            {"path": "crossfamily/review.worker.json", "digest": _digest(worker_terminal)},
            json.loads(worker_terminal.read_text(encoding="utf-8")),
            "openai",
            "substantial",
        )

        assert result.returncode == 0, result.stderr
        assert answer.read_text(encoding="utf-8") == "Human answer\n"
        assert terminal_value == {
            "id": "review-1", "attempt_id": "attempt-1", "kind": "complete",
            "summary": "dispatcher observed a completed provider answer",
        }
        assert persisted == record
        assert record["output_path"] == str(answer)
        assert record["output_sha256"] == _digest(answer)
        assert record["terminal_artifact_path"] == str(terminal)
        assert record["terminal_artifact_sha256"] == _digest(terminal)
        assert accepted["status"] == "accepted"
        assert accepted["certifying"] is True
        assert finalizer_error is None
        assert finalizer_leg["status"] == "pass"


@pytest.mark.parametrize("alias_kind", ("path", "symlink", "hardlink"))
def test_cli_rejects_answer_and_dispatcher_terminal_aliases_before_certification(alias_kind):
    stub = """
        #!/usr/bin/env bash
        cat >/dev/null
        printf 'Human answer\\n'
    """
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        answer = tmp / "answer.txt"
        terminal = tmp / "dispatcher.terminal.json"
        if alias_kind == "path":
            terminal = answer
        elif alias_kind == "symlink":
            terminal.write_text("existing terminal\\n", encoding="utf-8")
            answer.symlink_to(terminal)
        else:
            answer.write_text("existing answer\\n", encoding="utf-8")
            terminal.hardlink_to(answer)

        result, record, _ = run_dispatch_with_stub(
            stub,
            extra_args=["--terminal-artifact", str(terminal)],
            output_path=answer,
        )

        assert result.returncode != 0
        assert record["status"] == "evidence_paths_not_distinct"
        assert record["certification_eligible"] is False
        assert record["terminal_observed"] is False


@pytest.mark.parametrize("receipt_alias", ("answer", "terminal"))
def test_cli_rejects_dispatch_receipt_aliases_before_certification(receipt_alias):
    stub = """
        #!/usr/bin/env bash
        cat >/dev/null
        printf 'Human answer\\n'
    """
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        answer = tmp / "answer.txt"
        terminal = tmp / "dispatcher.terminal.json"
        receipt = answer if receipt_alias == "answer" else terminal

        result, record, _ = run_dispatch_with_stub(
            stub,
            extra_args=[
                "--terminal-artifact", str(terminal),
                "--receipt", str(receipt),
            ],
            output_path=answer,
        )

        assert result.returncode != 0
        assert record["status"] == "evidence_paths_not_distinct"
        assert record["certification_eligible"] is False
        assert record["terminal_observed"] is False


@pytest.mark.parametrize("target_name", ("answer.txt", "dispatcher.terminal.json", "dispatch.json"))
@pytest.mark.parametrize("race_kind", ("rename", "hardlink"))
def test_publication_race_over_answer_terminal_or_receipt_fails_closed(target_name, race_kind):
    stub = """
        #!/usr/bin/env bash
        cat >/dev/null
        printf 'Human answer\\n'
    """
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "claude", stub)
        answer = tmp / "answer.txt"
        terminal = tmp / "dispatcher.terminal.json"
        receipt = tmp / "dispatch.json"
        barrier = tmp / "barrier"
        target = {answer.name: answer, terminal.name: terminal, receipt.name: receipt}[target_name]
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        env["CF_DISPATCH_TEST_BARRIER_DIR"] = str(barrier)
        env["CF_DISPATCH_TEST_BARRIER_MATCH"] = str(target)
        command = [
            str(SCRIPT), "--tool", "claude", "--orchestrator-family", "codex",
            "--task-id", "race-1", "--attempt-id", "race-attempt",
            "--receipt", str(receipt), "--out", str(answer),
            "--terminal-artifact", str(terminal), "--prompt", "Reply exactly OK",
            "--evidence-root", str(tmp),
        ]
        process = subprocess.Popen(
            command, cwd=str(tmp), env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        deadline = time.monotonic() + 10
        ready = []
        while time.monotonic() < deadline:
            ready = list(barrier.glob("*.publish.ready"))
            if ready:
                break
            time.sleep(0.01)
        assert ready, "publication barrier was not reached"
        payload = target.read_bytes()
        if race_kind == "rename":
            moved = target.with_name(target.name + ".moved")
            target.rename(moved)
            target.write_bytes(payload)
        else:
            alias = target.with_name(target.name + ".alias")
            alias.hardlink_to(target)
        release = ready[0].with_name(ready[0].name.replace(".ready", ".release"))
        release.write_text("release", encoding="utf-8")
        stdout, stderr = process.communicate(timeout=10)
        record = json.loads(stdout)

        assert process.returncode != 0, stderr
        assert record["certification_eligible"] is False


@pytest.mark.parametrize("race_kind", ("rename", "symlink"))
def test_publication_parent_directory_swap_fails_closed(race_kind):
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        evidence = tmp / "evidence"
        evidence.mkdir()
        source = tmp / "source.txt"
        source.write_text("immutable evidence\n", encoding="utf-8")
        target = evidence / "answer.txt"
        barrier = tmp / "barrier"
        env = fabric_free_env()
        env["CF_DISPATCH_TEST_BARRIER_DIR"] = str(barrier)
        env["CF_DISPATCH_TEST_BARRIER_MATCH"] = str(target)
        process = subprocess.Popen(
            [
                sys.executable, str(PUBLISH_HELPER), "--root", str(tmp),
                "publish", str(target), str(source),
            ],
            cwd=str(tmp), env=env, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        deadline = time.monotonic() + 10
        ready = []
        while time.monotonic() < deadline:
            ready = list(barrier.glob("*.publish.ready"))
            if ready:
                break
            time.sleep(0.01)
        assert ready, "publication barrier was not reached"
        moved = tmp / "evidence.moved"
        evidence.rename(moved)
        if race_kind == "symlink":
            evidence.symlink_to(tmp, target_is_directory=True)
        else:
            evidence.mkdir()
        release = ready[0].with_name(ready[0].name.replace(".ready", ".release"))
        release.write_text("release", encoding="utf-8")
        _stdout, stderr = process.communicate(timeout=10)

        assert process.returncode != 0, stderr


def test_publisher_refuses_to_replace_existing_evidence():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        target = tmp / "answer.txt"
        source = tmp / "source.txt"
        target.write_text("original\n", encoding="utf-8")
        source.write_text("replacement\n", encoding="utf-8")

        result = subprocess.run(
            [
                sys.executable, str(PUBLISH_HELPER), "--root", str(tmp),
                "publish", str(target), str(source),
            ],
            cwd=str(tmp), text=True, capture_output=True,
        )

        assert result.returncode != 0
        assert target.read_text(encoding="utf-8") == "original\n"


def test_publisher_rejects_symlinked_parent_below_the_bound_run_root():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        run_dir = tmp / "run"
        run_dir.mkdir()
        outside = tmp / "outside"
        outside.mkdir()
        (run_dir / "answers").symlink_to(outside, target_is_directory=True)
        source = run_dir / "source.txt"
        source.write_text("immutable evidence\n", encoding="utf-8")
        target = run_dir / "answers" / "answer.txt"

        result = subprocess.run(
            [
                sys.executable, str(PUBLISH_HELPER), "--root", str(run_dir),
                "publish", str(target), str(source),
            ],
            cwd=str(run_dir), text=True, capture_output=True,
        )

        assert result.returncode != 0
        assert not (outside / "answer.txt").exists()


@pytest.mark.parametrize("command", ("identity", "digest"))
def test_publisher_rejects_fifo_without_blocking(command):
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        fifo = tmp / "evidence.fifo"
        os.mkfifo(fifo)
        arguments = [
            sys.executable, str(PUBLISH_HELPER), "--root", str(tmp), command, str(fifo),
        ]
        if command == "identity":
            arguments.append(str(tmp / "other.txt"))

        result = subprocess.run(
            arguments,
            cwd=str(tmp), text=True, capture_output=True, timeout=2,
        )

        assert result.returncode != 0
        assert "not a regular file" in result.stderr


def test_publisher_accepts_an_unresolved_symlink_alias_for_the_run_root():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        real_run_dir = tmp / "private" / "run"
        real_run_dir.mkdir(parents=True)
        alias_run_dir = tmp / "var"
        alias_run_dir.symlink_to(real_run_dir, target_is_directory=True)
        evidence = real_run_dir / "answer.txt"
        evidence.write_text("immutable evidence\n", encoding="utf-8")
        alias_evidence = alias_run_dir / evidence.name

        result = subprocess.run(
            [
                sys.executable, str(PUBLISH_HELPER), "--root", str(alias_run_dir),
                "digest", str(alias_evidence),
            ],
            cwd=str(tmp), text=True, capture_output=True,
        )

        assert result.returncode == 0, result.stderr
        assert result.stdout.startswith("sha256:")


@pytest.mark.parametrize("escaped_option", ("out", "terminal-artifact", "receipt"))
def test_dispatch_rejects_evidence_paths_outside_explicit_evidence_root(escaped_option):
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        evidence_root = tmp / "run"
        evidence_root.mkdir()
        outside = tmp / "outside"
        outside.mkdir()
        paths = {
            "out": evidence_root / "answer.txt",
            "terminal-artifact": evidence_root / "answer.terminal.json",
            "receipt": evidence_root / "answer.route.json",
        }
        paths[escaped_option] = outside / paths[escaped_option].name

        result = subprocess.run(
            [
                str(SCRIPT), "--tool", "codex", "--orchestrator-family", "anthropic",
                "--evidence-root", str(evidence_root), "--out", str(paths["out"]),
                "--terminal-artifact", str(paths["terminal-artifact"]),
                "--receipt", str(paths["receipt"]), "--prompt", "Review",
            ],
            cwd=str(tmp), env=fabric_free_env(), text=True, capture_output=True,
        )

        assert result.returncode != 0
        assert "evidence path escapes evidence root" in result.stderr
        assert not paths[escaped_option].exists()


def test_dispatch_accepts_explicit_root_with_receipt_and_default_output_in_different_trees():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        run_dir = tmp / "run"
        receipts = run_dir / "receipts"
        temp_root = run_dir / "temporary"
        receipts.mkdir(parents=True)
        temp_root.mkdir()
        receipt = receipts / "dispatch.json"
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "claude", "#!/usr/bin/env bash\ncat >/dev/null\necho OK\n")
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        env["TMPDIR"] = str(temp_root)

        result = subprocess.run(
            [
                str(SCRIPT), "--tool", "claude", "--orchestrator-family", "codex",
                "--evidence-root", str(run_dir), "--receipt", str(receipt),
                "--prompt", "Review",
            ],
            cwd=str(run_dir), env=env, text=True, capture_output=True,
        )

        assert result.returncode == 0, result.stderr
        assert receipt.is_file()


def test_chain_failed_then_success_preserves_attempt_evidence_and_summary():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        counter = tmp / "counter"
        write_executable(
            bin_dir / "claude",
            "#!/usr/bin/env bash\n"
            f"count=0\n[ -f '{counter}' ] && count=$(cat '{counter}')\n"
            f"count=$((count + 1))\nprintf '%s' \"$count\" > '{counter}'\n"
            "cat >/dev/null\n"
            "if [ \"$count\" -eq 1 ]; then\n"
            "  echo 'first provider failed' >&2\n"
            "  exit 9\n"
            "fi\n"
            "echo 'second provider succeeded'\n",
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        answer = tmp / "review.txt"
        terminal = tmp / "review.terminal.json"
        receipt = tmp / "review.route.json"
        result = subprocess.run(
            [
                str(SCRIPT), "--chain", "claude claude",
                "--orchestrator-family", "codex", "--task-id", "chain-1",
                "--receipt", str(receipt), "--out", str(answer),
                "--terminal-artifact", str(terminal), "--prompt", "Review",
                "--evidence-root", str(tmp),
            ],
            cwd=str(tmp), env=env, text=True, capture_output=True,
        )

        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr
        assert receipt.is_file()
        assert json.loads(receipt.read_text(encoding="utf-8")) == record
        attempts = record["chain"]["attempts"]
        assert len(attempts) == 2
        assert attempts[0]["status"] != "ok"
        assert attempts[1]["status"] == "ok"
        assert len({item["output_path"] for item in attempts}) == 2
        assert len({item["terminal_artifact_path"] for item in attempts}) == 2
        assert len({item["receipt_path"] for item in attempts}) == 2
        for item in attempts:
            assert Path(item["output_path"]).is_file()
            assert Path(item["terminal_artifact_path"]).is_file()
            assert Path(item["receipt_path"]).is_file()
        selected = record["chain"]["selected_success"]
        assert selected["attempt_id"] == attempts[1]["attempt_id"]
        assert selected["receipt_path"] == attempts[1]["receipt_path"]
        assert not answer.exists()
        assert not terminal.exists()


def test_receipt_write_failure_is_explicitly_non_successful():
    stub = """\
        #!/usr/bin/env bash
        cat >/dev/null
        printf 'OK\n'
    """
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "claude", stub)
        out = tmp / "answer.txt"
        receipt = tmp / "missing" / "dispatch.json"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{PRODUCT_ROOT / 'scripts'}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT), "--tool", "claude", "--orchestrator-family", "codex",
                "--task-id", "task-1", "--attempt-id", "attempt-1", "--receipt", str(receipt),
                "--out", str(out), "--prompt", "Reply exactly OK",
                "--evidence-root", str(tmp),
            ],
            cwd=str(tmp), env=env, text=True, capture_output=True,
        )

        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "receipt_write_error"
        assert record["terminal_observed"] is False
        assert record["certification_eligible"] is False
        assert "cannot write dispatcher receipt" in result.stderr


def _digest(path: Path) -> str:
    import hashlib

    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def test_claude_fallback_runs_after_oauth_safe_mode_model_failure():
    stub = """\
        #!/usr/bin/env bash
        if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
          echo '{"loggedIn":true}'
          exit 0
        fi
        model=""; safe=0; bare=0
        while [ $# -gt 0 ]; do
          case "$1" in
            --model) model="$2"; shift 2 ;;
            --safe-mode) safe=1; shift ;;
            --bare) bare=1; shift ;;
            *) shift ;;
          esac
        done
        cat >/dev/null
        if [ "$bare" = 1 ]; then echo "Not logged in" >&2; exit 1; fi
        if [ "$safe" = 1 ] && [ "$model" = "fable" ]; then echo "model fable is not available" >&2; exit 1; fi
        if [ "$safe" = 1 ] && [ "$model" = "opus" ]; then echo "SAFE OPUS"; exit 0; fi
        exit 9
    """
    result, record, output = run_dispatch_with_stub(stub, role="other-primary")
    assert result.returncode == 0, result.stderr
    assert record["resolved_model"] == "opus"
    assert record["read_only_guarantee"] == "oauth_safe_mode"
    assert output.strip() == "SAFE OPUS"


def test_help_exits_cleanly():
    result = subprocess.run(
        [str(SCRIPT), "--help"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 0
    assert "Gemini/Agy execution belongs to Agent Fabric" in result.stdout
    assert "--doctor" in result.stdout


def test_doctor_exits_cleanly():
    result = subprocess.run(
        [str(SCRIPT), "--doctor"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 0
    assert "cf_dispatch doctor" in result.stdout
    assert "PATH=" in result.stdout
    assert "agy=" not in result.stdout


def test_missing_option_value_is_clean_error():
    result = subprocess.run(
        [str(SCRIPT), "--tool"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 2
    assert "missing value for --tool" in result.stderr
    assert "unbound variable" not in result.stderr


def test_missing_prompt_file_is_clean_error():
    result = subprocess.run(
        [str(SCRIPT), "--tool", "claude", "--orchestrator-family", "codex", "--prompt-file", "/no/such/file", "--evidence-root", str(Path.cwd())],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 2
    assert "cannot read prompt file: /no/such/file" in result.stderr


def test_claude_oauth_fallback_after_bare_auth_failure():
    stub = """\
        #!/usr/bin/env bash
        if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
          echo '{"loggedIn":true,"authMethod":"claude.ai"}'
          exit 0
        fi
        for arg in "$@"; do
          if [ "$arg" = "--bare" ]; then
            echo "Not logged in · Please run /login" >&2
            exit 1
          fi
        done
        cat >/dev/null
        echo "OK"
    """
    result, record, output = run_dispatch_with_stub(stub)
    assert result.returncode == 0, result.stderr
    assert record["status"] == "ok"
    assert record["tool"] == "claude"
    assert record["read_only_guarantee"] == "oauth_safe_mode"
    assert output.strip() == "OK"


def test_claude_oauth_fallback_uses_verifier_system_prompt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        args_file = tmp / "claude.args"
        write_executable(
            bin_dir / "claude",
            f"""\
            #!/usr/bin/env bash
            if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
              echo '{{"loggedIn":true,"authMethod":"claude.ai"}}'
              exit 0
            fi
            printf '%s\\n' "$@" >> {args_file}
            printf 'CLAUDE_CODE_DISABLE_WORKFLOWS=%s\\n' "$CLAUDE_CODE_DISABLE_WORKFLOWS" >> {args_file}
            for arg in "$@"; do
              if [ "$arg" = "--bare" ]; then
                echo "Not logged in · Please run /login" >&2
                exit 1
              fi
            done
            cat >/dev/null
            echo "OK"
            """,
        )
        out = tmp / "out.txt"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "claude",
                "--orchestrator-family",
                "codex",
                "--out",
                str(out),
                "--prompt",
                "Reply exactly OK",
                "--evidence-root",
                str(tmp),
            ],
            cwd=str(tmp),
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr
        assert record["status"] == "ok"
        args = args_file.read_text(encoding="utf-8")
        assert "--system-prompt" in args
        assert "--disable-slash-commands" in args
        assert "non-interactive cross-family verifier" in args
        assert "launch subagents" in args
        assert "CLAUDE_CODE_DISABLE_WORKFLOWS=1" in args
        assert "Read,Grep,Glob" in args.splitlines()
        assert "Bash" not in args.splitlines()
        assert "Edit" not in args.splitlines()
        assert out.read_text(encoding="utf-8").strip() == "OK"


def test_removed_agy_direct_route_fails_closed_with_schema():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [str(SCRIPT), "--tool", "agy", "--model", "gemini-test", "--orchestrator-family", "codex", "--prompt", "Reply exactly OK", "--evidence-root", os.environ.get("TMPDIR", "/tmp")],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "unknown_tool"
        assert record["read_only_guarantee"] == "none"


def test_default_failure_retains_only_the_declared_output_tempfile():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        temp_root = tmp / "tmp"
        temp_root.mkdir()
        env = fabric_free_env()
        env["TMPDIR"] = str(temp_root)
        result = subprocess.run(
            [str(SCRIPT), "--tool", "kiro", "--orchestrator-family", "codex", "--prompt", "Review", "--evidence-root", str(temp_root)],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        output = Path(record["output_path"])
        assert result.returncode != 0
        assert output.exists()
        assert sorted(path.resolve() for path in temp_root.iterdir()) == sorted(
            [output.resolve(), Path(record["terminal_artifact_path"]).resolve()]
        )
        assert Path(record["terminal_artifact_path"]).is_file()
        output.unlink()


def test_orchestrator_family_is_required():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [str(SCRIPT), "--tool", "claude", "--prompt", "Reply exactly OK", "--evidence-root", os.environ.get("TMPDIR", "/tmp")],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "orchestrator_family_required"
        assert record["cross_family"] is False


def test_evidence_root_is_required():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td).resolve()
        env = fabric_free_env()
        env["TMPDIR"] = str(tmp)
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "claude",
                "--orchestrator-family",
                "codex",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Reply exactly OK",
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "evidence_root_required"
        assert record["certification_eligible"] is False


def test_same_family_cli_is_forbidden_when_family_declared():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "codex",
                "--prompt",
                "Reply exactly OK",
                "--evidence-root",
                os.environ.get("TMPDIR", "/tmp"),
            ],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "same_family_forbidden"
        assert record["read_only_guarantee"] == "none"
        assert record["cross_family"] is False


def test_cursor_model_provider_prevents_disguised_same_family_review():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(bin_dir / "cursor-agent", "#!/usr/bin/env bash\necho OK\n")
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "cursor",
                "--model",
                "gpt-5.6-sol",
                "--orchestrator-family",
                "openai",
                "--prompt",
                "Review",
                "--evidence-root",
                os.environ.get("TMPDIR", "/tmp"),
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["provider_family"] == "openai"
        assert record["status"] == "same_family_forbidden"
        assert record["cross_family"] is False


def test_cursor_distinct_model_records_adapter_and_provider_family():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        args_file = tmp / "cursor.args"
        write_executable(
            bin_dir / "cursor-agent",
            f"#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" > {args_file}\necho OK\n",
        )
        out = tmp / "out.txt"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "cursor",
                "--model",
                "cursor-grok-4.5-high",
                "--orchestrator-family",
                "openai",
                "--out",
                str(out),
                "--prompt",
                "Review",
                "--evidence-root",
                str(tmp),
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode == 0
        assert record["adapter"] == "cursor"
        assert record["provider_family"] == "xai"
        assert record["endpoint_provider"] == "cursor"
        assert record["model_family"] == "xai"
        assert record["resolved_model"] == "cursor-grok-4.5-high"
        assert record["certification_eligible"] is True
        assert record["cross_family"] is True
        cursor_args = args_file.read_text(encoding="utf-8").splitlines()
        assert "--trust" in cursor_args
        assert "--sandbox" in cursor_args
        assert "enabled" in cursor_args
        assert "--mode" in cursor_args
        assert cursor_args[cursor_args.index("--mode") + 1] == "ask"


def test_explicit_output_path_preserves_adapter_failure_diagnostics():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(
            bin_dir / "cursor-agent",
            "#!/usr/bin/env bash\necho 'simulated adapter failure' >&2\nexit 9\n",
        )
        out = tmp / "review.txt"
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "cursor",
                "--model",
                "cursor-grok-4.5-high",
                "--orchestrator-family",
                "openai",
                "--out",
                str(out),
                "--prompt",
                "Review",
                "--evidence-root",
                str(tmp),
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "error"
        assert record["output_path"] == str(out)
        assert "simulated adapter failure" in out.read_text(encoding="utf-8")


def test_unwritable_output_path_cannot_certify_success():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(
            bin_dir / "codex",
            """#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{"models":[{"slug":"gpt-5.6-sol","supported_reasoning_levels":[{"effort":"high"},{"effort":"max"},{"effort":"ultra"}]}]}'
              exit 0
            fi
            echo OK
            """,
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [str(SCRIPT), "--tool", "codex", "--orchestrator-family", "anthropic", "--out", str(tmp / "missing" / "out.txt"), "--prompt", "Review", "--evidence-root", str(tmp)],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "terminal_artifact_write_error"
        assert record["certification_eligible"] is False
        assert record["output_path"] == ""


def test_resolved_role_effort_reaches_codex_adapter_and_receipt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        args_file = tmp / "codex.args"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{{"models":[{{"slug":"gpt-5.6-sol","supported_reasoning_levels":[{{"effort":"high"}},{{"effort":"xhigh"}}]}}]}}'
              exit 0
            fi
            printf '%s\\n' "$@" > {args_file}
            cat >/dev/null
            echo OK
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "anthropic",
                "--role",
                "critical-review",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Review",
                "--evidence-root",
                str(tmp),
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr
        assert record["requested_effort"] == "max"
        assert record["effort"] == "xhigh"
        assert record["effort_capability_source"] == "runtime-model-catalog"
        assert record["resolved_model"] == ""
        assert record["catalog_model"] == "gpt-5.6-sol"
        assert record["model_selection"] == "account-default"
        args = args_file.read_text(encoding="utf-8").splitlines()
        assert "-m" not in args
        assert "model_reasoning_effort=xhigh" in args


def test_codex_capability_discovery_failure_blocks_execution_with_receipt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        invoked = tmp / "codex.exec-invoked"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              echo "capability discovery unavailable" >&2
              exit 23
            fi
            touch {invoked}
            exit 9
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "anthropic",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Review",
                "--evidence-root",
                str(tmp),
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "capability_discovery_failed"
        assert record["effort_capability_source"] == "runtime-discovery-failed"
        assert record["certification_eligible"] is False
        assert record["read_only_guarantee"] == "none"
        assert not invoked.exists()


def test_mixed_malformed_codex_capabilities_block_execution_with_receipt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        invoked = tmp / "codex.exec-invoked"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{{"models":[{{"display_name":"missing slug"}},{{"slug":"gpt-5.6-terra","supported_reasoning_levels":[{{"effort":"high"}}]}}]}}'
              exit 0
            fi
            touch {invoked}
            exit 9
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "anthropic",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Review",
                "--evidence-root",
                str(tmp),
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "capability_discovery_failed"
        assert record["certification_eligible"] is False
        assert not invoked.exists()


def test_unrelated_codex_model_without_efforts_blocks_execution_with_receipt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        invoked = tmp / "codex.exec-invoked"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{{"models":[{{"slug":"gpt-5.6-terra","supported_reasoning_levels":[]}}]}}'
              exit 0
            fi
            touch {invoked}
            exit 9
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "anthropic",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Review",
                "--evidence-root",
                str(tmp),
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "capability_discovery_failed"
        assert record["certification_eligible"] is False
        assert not invoked.exists()


def test_duplicate_codex_discovery_member_blocks_execution_with_receipt():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        invoked = tmp / "codex.exec-invoked"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{{"models":[{{"slug":"gpt-5.6-sol","supported_reasoning_levels":[{{"effort":"high","effort":"max"}}]}}]}}'
              exit 0
            fi
            touch {invoked}
            exit 9
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--orchestrator-family",
                "anthropic",
                "--out",
                str(tmp / "out.txt"),
                "--prompt",
                "Review",
                "--evidence-root",
                str(tmp),
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "capability_discovery_failed"
        assert record["certification_eligible"] is False
        assert not invoked.exists()


def test_codex_explicit_model_rejection_never_reports_it_as_resolved():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        invoked = tmp / "codex.invoked"
        write_executable(
            bin_dir / "codex",
            f'''#!/usr/bin/env bash
            if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
              printf '%s\n' '{{"models":[{{"slug":"gpt-5.6-sol","supported_reasoning_levels":[{{"effort":"high"}}]}}]}}'
              exit 0
            fi
            touch {invoked}
            exit 9
            ''',
        )
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "codex",
                "--model",
                "gpt-5.6-sol",
                "--orchestrator-family",
                "anthropic",
                "--prompt",
                "Review",
                "--evidence-root",
                os.environ.get("TMPDIR", "/tmp"),
            ],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "adapter_account_default_only"
        assert record["resolved_model"] == ""
        assert record["requested_model"] == "gpt-5.6-sol"
        assert record["catalog_model"] == "gpt-5.6-sol"
        assert record["model_selection"] == "account-default"
        assert record["identity_source"] == "account-default"
        assert not invoked.exists()


def test_interrupted_dispatch_cleans_internal_tempfiles():
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        bin_dir = tmp / "bin"
        temp_root = tmp / "tmp"
        bin_dir.mkdir()
        temp_root.mkdir()
        write_executable(bin_dir / "codex", "#!/usr/bin/env bash\nsleep 10\n")
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:{env['PATH']}"
        env["TMPDIR"] = str(temp_root)
        proc = subprocess.Popen(
            [str(SCRIPT), "--tool", "codex", "--orchestrator-family", "anthropic", "--out", str(tmp / "out.txt"), "--prompt", "Review", "--evidence-root", str(tmp)],
            cwd=td,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
        for _ in range(20):
            if any(temp_root.iterdir()):
                break
            time.sleep(0.05)
        os.killpg(proc.pid, signal.SIGTERM)
        proc.communicate(timeout=5)
        assert list(temp_root.iterdir()) == []


def test_broker_adapter_requires_resolvable_provider_family():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "cursor",
                "--orchestrator-family",
                "openai",
                "--prompt",
                "Review",
                "--evidence-root",
                os.environ.get("TMPDIR", "/tmp"),
            ],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert record["status"] == "model_required_for_broker"
        assert record["cross_family"] is False


def test_manual_provider_override_is_not_supported():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "claude",
                "--provider-family",
                "google",
                "--orchestrator-family",
                "anthropic",
                "--prompt",
                "Review",
                "--evidence-root",
                str(Path(td)),
            ],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        assert result.returncode == 2
        assert "unknown arg: --provider-family" in result.stderr


def test_invalid_orchestrator_family_fails_closed():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [
                str(SCRIPT),
                "--tool",
                "claude",
                "--orchestrator-family",
                "Claude",
                "--prompt",
                "Reply exactly OK",
                "--evidence-root",
                os.environ.get("TMPDIR", "/tmp"),
            ],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["status"] == "invalid_orchestrator_family"
        assert record["cross_family"] is False


def test_successful_output_with_auth_words_stays_ok():
    stub = """\
        #!/usr/bin/env bash
        cat >/dev/null
        echo "The string Not logged in appears in the artifact under review."
    """
    result, record, output = run_dispatch_with_stub(stub)
    assert result.returncode == 0, result.stderr
    assert record["status"] == "ok"
    assert output.strip() == "The string Not logged in appears in the artifact under review."


def test_chain_all_failed_uses_dispatch_schema():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [
                str(SCRIPT),
                "--chain",
                "kiro copilot",
                "--orchestrator-family",
                "codex",
                "--prompt",
                "Reply exactly OK",
                "--evidence-root",
                os.environ.get("TMPDIR", "/tmp"),
            ],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode != 0
        assert DISPATCH_SCHEMA <= set(record)
        assert record["tool"] == "chain"
        assert record["status"] == "all_failed"
        assert record["read_only_guarantee"] == "none"


def test_run_dir_init_force_flag_only_creates_final_gate():
    with tempfile.TemporaryDirectory() as td:
        result = subprocess.run(
            [str(RUN_DIR_SCRIPT), "--force"],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        assert result.returncode == 0, result.stderr
        run_dir = Path(result.stdout.strip())
        if not run_dir.is_absolute():
            run_dir = Path(td) / run_dir
        assert (run_dir / "FINAL_GATE.md").exists()
        receipt = json.loads((run_dir / "RUN_RECEIPT.json").read_text(encoding="utf-8"))
        assert receipt["status"] == "active"
        assert receipt["retention_policy"] == "capsule-plus-referenced-evidence"
        assert (run_dir / "traces" / "README.md").exists()
        gate = (run_dir / "FINAL_GATE.md").read_text(encoding="utf-8")
        for row in REQUIRED_GATE_ROWS:
            assert row in gate


def test_run_dir_init_force_does_not_clobber_existing_manifest():
    with tempfile.TemporaryDirectory() as td:
        run_dir = Path(td) / "existing"
        run_dir.mkdir()
        manifest = run_dir / "MANIFEST.md"
        manifest.write_text("KEEP\\n", encoding="utf-8")
        result = subprocess.run(
            [str(RUN_DIR_SCRIPT), str(run_dir), "--force"],
            cwd=td,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        assert result.returncode == 0, result.stderr
        assert manifest.read_text(encoding="utf-8") == "KEEP\\n"
        assert (run_dir / "FINAL_GATE.md").exists()
        assert (run_dir / "RUN_RECEIPT.json").exists()
        gate = (run_dir / "FINAL_GATE.md").read_text(encoding="utf-8")
        for row in REQUIRED_GATE_ROWS:
            assert row in gate


def test_non_git_fallback_routes_via_product_root_model_route():
    # From a non-git copy of the product tree, with no provenant on PATH,
    # dispatch must fall back to <product root>/scripts/model_route.py.
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        product = tmp / "product"
        shutil.copytree(
            HERE.parent / "scripts",
            product / "skills" / "orchestrate" / "scripts",
        )
        (product / "skills" / "_shared").mkdir(parents=True)
        shutil.copy2(
            PRODUCT_ROOT / "skills" / "_shared" / "no_follow.py",
            product / "skills" / "_shared" / "no_follow.py",
        )
        shutil.copytree(PRODUCT_ROOT / "config", product / "config")
        (product / "scripts").mkdir()
        for name in (
            "model_route.py",
            "model_route_catalog.py",
            "model_route_preferences.py",
        ):
            shutil.copy2(PRODUCT_ROOT / "scripts" / name, product / "scripts" / name)
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        write_executable(
            bin_dir / "claude",
            """\
            #!/usr/bin/env bash
            cat >/dev/null
            echo "OK"
            """,
        )
        write_executable(bin_dir / "python3", f'#!/bin/sh\nexec {sys.executable} "$@"\n')
        env = fabric_free_env()
        env["PATH"] = f"{bin_dir}:/usr/bin:/bin"
        # cf_dispatch.sh appends $HOME/.local/bin and $HOME/bin to PATH;
        # point HOME at the sandbox so an installed provenant cannot leak in.
        env["HOME"] = str(tmp)
        out = tmp / "out.txt"
        result = subprocess.run(
            [
                str(product / "skills" / "orchestrate" / "scripts" / "cf_dispatch.sh"),
                "--tool",
                "claude",
                "--orchestrator-family",
                "codex",
                "--role",
                "reviewer",
                "--out",
                str(out),
                "--prompt",
                "Reply exactly OK",
                "--evidence-root",
                str(tmp),
            ],
            cwd=str(tmp),
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        record = json.loads(result.stdout)
        assert result.returncode == 0, result.stderr
        assert record["status"] == "ok"
        assert record["resolved_model"]
        assert out.read_text(encoding="utf-8").strip() == "OK"


if __name__ == "__main__":
    test_help_exits_cleanly()
    test_doctor_exits_cleanly()
    test_missing_option_value_is_clean_error()
    test_missing_prompt_file_is_clean_error()
    test_claude_oauth_fallback_after_bare_auth_failure()
    test_claude_oauth_fallback_uses_verifier_system_prompt()
    test_removed_agy_direct_route_fails_closed_with_schema()
    test_orchestrator_family_is_required()
    test_same_family_cli_is_forbidden_when_family_declared()
    test_invalid_orchestrator_family_fails_closed()
    test_successful_output_with_auth_words_stays_ok()
    test_chain_all_failed_uses_dispatch_schema()
    test_run_dir_init_force_flag_only_creates_final_gate()
    test_run_dir_init_force_does_not_clobber_existing_manifest()
    print("cf_dispatch behaviour tests: PASS")
