from pathlib import Path
import stat
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
CHECKER = ROOT / "skills" / "orchestrate" / "evals" / "check_herdr_cli.py"
REFERENCE = ROOT / "skills" / "orchestrate" / "references" / "herdr-panes.md"

ROOT_HELP = """\
herdr agent <subcommand>         Agent helpers
herdr pane <subcommand>          Pane helpers
herdr wait <subcommand>          Blocking wait helpers
herdr integration <subcommand>   Integration helpers
herdr status [server|client]     Show status
herdr api <subcommand>           API metadata
  --version, -V                  Print version and exit
  --help, -h                     Show this help
"""
AGENT_HELP = """\
herdr agent list
herdr agent get <target>
herdr agent read <target>
herdr agent send <target> <text>
herdr agent start <name> -- <argv...>
herdr agent explain <target> [--json]
herdr agent wait <target> --status <idle|working|blocked|unknown> [--timeout MS]
"""
PANE_HELP = """\
herdr pane layout [--pane ID|--current]
herdr pane read <pane_id> [--source visible|recent|recent-unwrapped] [--lines N]
herdr pane process-info [--pane ID|--current]
herdr pane run <pane_id> <command>
"""
WAIT_HELP = """\
herdr wait output <pane_id> --match <text>
herdr wait agent-status <pane_id> --status <idle|working|blocked|done|unknown> [--timeout MS]
"""
INTEGRATION_HELP = """\
herdr integration install pi
herdr integration status [--outdated-only]
"""
STATUS_HELP = """\
herdr status server [--json]
herdr status client [--json]
"""
API_HELP = """\
herdr api schema [--json | --output PATH]
"""


def _fake_herdr(
    tmp_path: Path,
    *,
    version: str = "fixture-version",
    agent_help: str = AGENT_HELP,
    pane_help: str = PANE_HELP,
    wait_help: str = WAIT_HELP,
    integration_help: str = INTEGRATION_HELP,
) -> tuple[Path, Path]:
    fake = tmp_path / "herdr"
    log = tmp_path / "calls.log"
    responses = {
        "--version": f"herdr {version}\n",
        "--help": ROOT_HELP,
        "agent --help": agent_help,
        "pane --help": pane_help,
        "wait --help": wait_help,
        "integration --help": integration_help,
        "status --help": STATUS_HELP,
        "api --help": API_HELP,
    }
    fake.write_text(
        "#!/usr/bin/env python3\n"
        "import pathlib, sys\n"
        f"log = pathlib.Path({str(log)!r})\n"
        "key = ' '.join(sys.argv[1:])\n"
        "with log.open('a') as stream: stream.write(key + '\\n')\n"
        f"responses = {responses!r}\n"
        "if key not in responses:\n"
        "    print('unexpected command: ' + key, file=sys.stderr)\n"
        "    raise SystemExit(91)\n"
        "print(responses[key], end='')\n"
    )
    fake.chmod(fake.stat().st_mode | stat.S_IXUSR)
    return fake, log


def _run(binary: Path, *extra: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            sys.executable,
            str(CHECKER),
            "--binary",
            str(binary),
            "--reference",
            str(REFERENCE),
            *extra,
        ],
        capture_output=True,
        text=True,
    )


def test_cli_gate_uses_only_observed_version_and_capability_help_without_a_live_session(tmp_path: Path) -> None:
    fake, log = _fake_herdr(tmp_path)
    result = _run(fake)

    assert result.returncode == 0, result.stdout + result.stderr
    assert "HERDR CLI CHECK: PASS" in result.stdout
    assert log.read_text().splitlines() == [
        "--version",
        "--help",
        "agent --help",
        "pane --help",
        "wait --help",
        "integration --help",
        "status --help",
        "api --help",
    ]


def test_cli_gate_skips_cleanly_when_herdr_is_unavailable(tmp_path: Path) -> None:
    result = _run(tmp_path / "missing-herdr")

    assert result.returncode == 0
    assert "HERDR CLI CHECK: SKIP" in result.stdout
    assert "binary not found" in result.stdout


def test_cli_gate_accepts_an_auto_updated_provider_version(tmp_path: Path) -> None:
    fake, _ = _fake_herdr(tmp_path, version="future-fixture-version")
    result = _run(fake)

    assert result.returncode == 0
    assert "HERDR CLI CHECK: PASS" in result.stdout
    assert "observed version future-fixture-version" in result.stdout


def test_cli_gate_distinguishes_agent_idle_from_pane_done_wait_statuses(tmp_path: Path) -> None:
    fake, _ = _fake_herdr(
        tmp_path,
        agent_help=AGENT_HELP.replace("blocked|unknown", "blocked|done|unknown"),
    )
    result = _run(fake)

    assert result.returncode == 1
    assert "agent wait statuses" in result.stdout
    assert "done" in result.stdout


def test_cli_gate_rejects_documented_command_drift(tmp_path: Path) -> None:
    fake, _ = _fake_herdr(tmp_path, agent_help=AGENT_HELP.replace("herdr agent read <target>\n", ""))
    result = _run(fake)

    assert result.returncode == 1
    assert "documented command is absent from help: herdr agent read" in result.stdout


def test_cli_gate_rejects_missing_documented_integration_install(tmp_path: Path) -> None:
    fake, _ = _fake_herdr(
        tmp_path,
        integration_help=INTEGRATION_HELP.replace("herdr integration install pi\n", ""),
    )
    result = _run(fake)

    assert result.returncode == 1
    assert (
        "documented command is absent from help: herdr integration install pi"
        in result.stdout
    )


def test_cli_gate_rejects_missing_documented_pane_run_shorthand(tmp_path: Path) -> None:
    fake, _ = _fake_herdr(
        tmp_path,
        pane_help=PANE_HELP.replace("herdr pane run <pane_id> <command>\n", ""),
    )
    result = _run(fake)

    assert result.returncode == 1
    assert "documented command is absent from help: herdr pane run" in result.stdout


def test_cli_gate_rejects_missing_documented_pane_observation_commands(
    tmp_path: Path,
) -> None:
    for index, command in enumerate(("herdr pane layout", "herdr pane read")):
        case = tmp_path / str(index)
        case.mkdir()
        fake, _ = _fake_herdr(
            case,
            pane_help=PANE_HELP.replace(
                next(line for line in PANE_HELP.splitlines() if line.startswith(command)) + "\n",
                "",
            ),
        )
        result = _run(fake)

        assert result.returncode == 1
        assert f"documented command is absent from help: {command}" in result.stdout


def test_cli_gate_rejects_a_documented_status_absent_from_help(tmp_path: Path) -> None:
    reference = tmp_path / "herdr-panes.md"
    reference.write_text(
        REFERENCE.read_text().replace(
            "A `blocked` agent needs input",
            "A `blocked` agent needs input; a `paused` agent is suspended",
        )
    )
    fake, _ = _fake_herdr(tmp_path)
    result = subprocess.run(
        [
            sys.executable,
            str(CHECKER),
            "--binary",
            str(fake),
            "--reference",
            str(reference),
        ],
        capture_output=True,
        text=True,
    )

    assert result.returncode == 1
    assert "documented status tokens are absent from help: ['paused']" in result.stdout


def test_installed_herdr_capabilities_pass_or_skip_cleanly() -> None:
    result = subprocess.run(
        [sys.executable, str(CHECKER)],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stdout + result.stderr
    assert "HERDR CLI CHECK: PASS" in result.stdout or "HERDR CLI CHECK: SKIP" in result.stdout
