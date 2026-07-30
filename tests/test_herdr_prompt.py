import json
import os
from pathlib import Path
import stat
import subprocess


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "orchestrate" / "scripts" / "herdr_prompt.sh"


def test_prompt_helper_delegates_to_fabric_without_contacting_herdr(tmp_path):
    fabric_log = tmp_path / "fabric.log"
    herdr_log = tmp_path / "herdr.log"
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake_provenant = bin_dir / "provenant"
    fake_provenant.write_text(
        "#!/bin/sh\n"
        f"printf '%s\\n' \"$*\" >> {fabric_log}\n"
        "printf '%s\\n' '{\"status\":\"terminal\"}'\n"
    )
    fake_provenant.chmod(fake_provenant.stat().st_mode | stat.S_IXUSR)
    fake = bin_dir / "herdr"
    fake.write_text(
        "#!/bin/sh\n"
        f"printf '%s\\n' \"$*\" >> {herdr_log}\n"
        "exit 99\n"
    )
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{env['PATH']}"
    result = subprocess.run(
        [
            str(SCRIPT),
            "review-claude",
            "--fire-and-forget",
            "--action-id",
            "herdr-steer-action-17",
            "--pane-ref",
            "w9:p3",
            "--task-ref",
            "task-review-17",
            "--expected-revision",
            "2",
            "--prompt",
            "Steer task-review-17: pause after the current check",
        ],
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 0, result.stderr
    assert fabric_log.read_text().splitlines() == [
        "fabric herdr steer review-claude --fire-and-forget --action-id "
        "herdr-steer-action-17 --pane-ref w9:p3 --task-ref task-review-17 "
        "--expected-revision 2 --prompt Steer task-review-17: pause after the current check"
    ]
    assert not herdr_log.exists()


def test_prompt_helper_propagates_fabric_validation_failure(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake_provenant = bin_dir / "provenant"
    fake_provenant.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' 'Herdr steering requires --fire-and-forget' >&2\n"
        "exit 1\n"
    )
    fake_provenant.chmod(fake_provenant.stat().st_mode | stat.S_IXUSR)
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{env['PATH']}"
    result = subprocess.run(
        [str(SCRIPT), "review-claude", "--prompt", "Review and report findings"],
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 1
    assert "--fire-and-forget" in result.stderr


def test_prompt_helper_propagates_fabric_unavailable_without_fallback(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake_provenant = bin_dir / "provenant"
    fake_provenant.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' '{\"status\":\"unavailable\",\"integration\":\"agent-fabric\",\"reason\":\"unavailable\"}'\n"
        "exit 1\n"
    )
    fake_provenant.chmod(fake_provenant.stat().st_mode | stat.S_IXUSR)
    herdr_log = tmp_path / "herdr.log"
    fake_herdr = bin_dir / "herdr"
    fake_herdr.write_text(
        "#!/bin/sh\n"
        f"printf '%s\\n' \"$*\" >> {herdr_log}\n"
        "exit 0\n"
    )
    fake_herdr.chmod(fake_herdr.stat().st_mode | stat.S_IXUSR)
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{env['PATH']}"
    result = subprocess.run(
        [
            str(SCRIPT),
            "review-claude",
            "--fire-and-forget",
            "--action-id",
            "herdr-steer-action-18",
            "--pane-ref",
            "w9:p3",
            "--task-ref",
            "task-review-18",
            "--expected-revision",
            "1",
            "--prompt",
            "Pause after the current check",
        ],
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 1
    assert '"status":"unavailable"' in result.stdout
    assert not herdr_log.exists()


def test_prompt_helper_delegates_help_to_fabric(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake_provenant = bin_dir / "provenant"
    fake_provenant.write_text(
        "#!/bin/sh\n"
        "printf '%s\\n' \"$*\"\n"
    )
    fake_provenant.chmod(fake_provenant.stat().st_mode | stat.S_IXUSR)
    env = os.environ.copy()
    env["PATH"] = f"{bin_dir}:{env['PATH']}"
    result = subprocess.run(
        [str(SCRIPT), "--help"],
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert result.returncode == 0
    assert result.stdout == "fabric herdr steer --help\n"


def test_real_fabric_client_fails_closed_when_daemon_is_unavailable(tmp_path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    herdr_log = tmp_path / "herdr.log"
    fake_herdr = bin_dir / "herdr"
    fake_herdr.write_text(
        "#!/bin/sh\n"
        f"printf '%s\\n' \"$*\" >> {herdr_log}\n"
        "exit 0\n"
    )
    fake_herdr.chmod(fake_herdr.stat().st_mode | stat.S_IXUSR)
    env = os.environ.copy()
    env["AGENT_FABRIC_PRODUCT_ROOT"] = str(ROOT)
    env["AGENT_FABRIC_SOCKET_PATH"] = str(tmp_path / "missing-fabric.sock")
    env["AGENT_FABRIC_CAPABILITY"] = "afb_" + "A" * 43
    env["PATH"] = f"{ROOT / 'scripts'}:{bin_dir}:{env['PATH']}"

    result = subprocess.run(
        [
            str(SCRIPT),
            "review-claude",
            "--fire-and-forget",
            "--action-id",
            "herdr-steer-action-19",
            "--pane-ref",
            "w9:p3",
            "--task-ref",
            "task-review-19",
            "--expected-revision",
            "1",
            "--prompt",
            "Pause after the current check.",
        ],
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    assert result.returncode == 1
    payload = json.loads(result.stdout.strip())
    assert payload["status"] == "unavailable"
    assert payload["integration"] == "agent-fabric"
    # The reason names the failed check; the trailing syscall code varies by
    # platform (e.g. ENOENT, ECONNREFUSED, EINVAL).
    assert payload["reason"].startswith("daemon connection check failed: ")
    assert not herdr_log.exists()
