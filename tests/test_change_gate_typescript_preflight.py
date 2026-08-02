import subprocess
import sys
from pathlib import Path

import pytest

from scripts.change_gates import (
    DiffHunk,
    GateError,
    Mutant,
    gate_changed_line_mutation,
    gate_revert_probe,
    gate_right_reason_red,
)


def _commit_fixture(root):
    subprocess.run(["git", "-C", str(root), "init", "--quiet"], check=True)
    subprocess.run(["git", "-C", str(root), "add", "-A"], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(root),
            "-c",
            "user.name=Gate Test",
            "-c",
            "user.email=gate-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "base",
        ],
        check=True,
    )


def _typescript_fixture(tmp_path, *, build_body=None, with_node_modules=True):
    source = tmp_path / "source"
    source.mkdir()
    (source / "runtime" / "agent-fabric" / "scripts").mkdir(parents=True)
    (source / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs").write_text(
        "import { writeFile } from 'node:fs/promises';\n"
        "import { join } from 'node:path';\n"
        "await writeFile(join(process.argv[2], 'runtime/agent-fabric/.npm-ci-attestation'), 'base-writer\\n');\n",
        encoding="utf-8",
    )
    (source / "scripts").mkdir()
    build = source / "scripts" / "agent-fabric-protocol-build"
    build.write_text(
        "#!/bin/sh\n"
        "set -eu\n"
        "test -L \"$AGENTS_HOME/node_modules\"\n"
        "test \"$(cat \"$AGENTS_HOME/runtime/agent-fabric/.npm-ci-attestation\")\" = base-writer\n"
        "mkdir -p \"$AGENTS_HOME/runtime/agent-fabric-protocol/dist\"\n"
        "printf 'base-build\\n' > \"$AGENTS_HOME/protocol-build-marker\"\n"
        "printf 'base-dist\\n' > \"$AGENTS_HOME/runtime/agent-fabric-protocol/dist/index.js\"\n"
        "printf 'base-stamp\\n' > \"$AGENTS_HOME/runtime/agent-fabric-protocol/dist/.root-manifests.sha256\"\n"
        + (build_body or ""),
        encoding="utf-8",
    )
    build.chmod(0o755)
    (source / "tests").mkdir()
    (source / "tests" / "protocol.test.ts").write_text("base test\n", encoding="utf-8")
    (source / "tests" / "protocol_gate.py").write_text(
        "from pathlib import Path\n"
        "if Path('runtime/agent-fabric/.npm-ci-attestation').read_text() != 'base-writer\\n':\n"
        "    print('AGENT_FABRIC_PROTOCOL_BUILD_STALE: base attestation was not refreshed')\n"
        "    raise SystemExit(1)\n"
        "if Path('protocol-build-marker').read_text() != 'base-build\\n':\n"
        "    print('AGENT_FABRIC_PROTOCOL_BUILD_STALE: base protocol build was not run')\n"
        "    raise SystemExit(1)\n"
        "if Path('runtime/agent-fabric-protocol/dist/index.js').read_text() != 'base-dist\\n':\n"
        "    print('AGENT_FABRIC_PROTOCOL_BUILD_STALE: base dist was not produced')\n"
        "    raise SystemExit(1)\n"
        "raise AssertionError('intended TypeScript assertion')\n",
        encoding="utf-8",
    )
    _commit_fixture(source)

    if with_node_modules:
        (source / "node_modules").mkdir()
        (source / "node_modules" / "installed.marker").write_text("installed\n", encoding="utf-8")
    return source


def _typescript_freshness_fixture(tmp_path, dist_value):
    source = tmp_path / "source"
    source.mkdir()
    (source / ".gitignore").write_text(
        "node_modules/\n"
        "runtime/agent-fabric-protocol/dist/\n",
        encoding="utf-8",
    )
    (source / "runtime" / "agent-fabric" / "scripts").mkdir(parents=True)
    (source / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs").write_text(
        "import { writeFile } from 'node:fs/promises';\n"
        "import { join } from 'node:path';\n"
        "await writeFile(join(process.argv[2], 'runtime/agent-fabric/.npm-ci-attestation'), 'fresh\\n');\n",
        encoding="utf-8",
    )
    (source / "runtime" / "agent-fabric-protocol" / "src").mkdir(parents=True)
    source_file = source / "runtime" / "agent-fabric-protocol" / "src" / "index.ts"
    source_file.write_text("export const value = 'old';\n", encoding="utf-8")
    (source / "scripts").mkdir()
    build = source / "scripts" / "agent-fabric-protocol-build"
    build.write_text(
        "#!/bin/sh\n"
        "set -eu\n"
        "test -L \"$AGENTS_HOME/node_modules\"\n"
        "mkdir -p \"$AGENTS_HOME/runtime/agent-fabric-protocol/dist\"\n"
        "cp \"$AGENTS_HOME/runtime/agent-fabric-protocol/src/index.ts\" "
        "\"$AGENTS_HOME/runtime/agent-fabric-protocol/dist/index.js\"\n",
        encoding="utf-8",
    )
    build.chmod(0o755)
    (source / "tests").mkdir()
    (source / "tests" / "protocol.test.ts").write_text("base test\n", encoding="utf-8")
    (source / "tests" / "expected.ts").write_text(
        "export const value = 'new';\n", encoding="utf-8"
    )
    _commit_fixture(source)
    (source / "node_modules").mkdir()
    dist = source / "runtime" / "agent-fabric-protocol" / "dist" / "index.js"
    dist.parent.mkdir(parents=True)
    dist.write_text(f"export const value = '{dist_value}';\n", encoding="utf-8")
    source_file.write_text("export const value = 'new';\n", encoding="utf-8")
    hunk = DiffHunk(
        path="runtime/agent-fabric-protocol/src/index.ts",
        header=(),
        hunk_header="@@ -1,1 +1,1 @@",
        body=("-export const value = 'old';", "+export const value = 'new';"),
        old_start=1,
        old_count=1,
        new_start=1,
        new_count=1,
        old_lines=("export const value = 'old';",),
        new_lines=("export const value = 'new';",),
    )
    return source, hunk


def _typescript_dist_command():
    return (
        f'{sys.executable} -c "from pathlib import Path; '
        "assert Path('runtime/agent-fabric-protocol/dist/index.js').read_text().strip() "
        "== Path('tests/expected.ts').read_text().strip()\" {test}"
    )


def _typescript_commands():
    return {
        "py": f'{sys.executable} -c "raise SystemExit(0)" {{test}}',
        "ts": _typescript_dist_command(),
    }


def test_typescript_revert_probe_refreshes_dist_after_reversing_a_hunk(tmp_path, capsys):
    source, hunk = _typescript_freshness_fixture(tmp_path, "new")

    result = gate_revert_probe(
        source,
        [hunk],
        _typescript_commands(),
        ["tests/protocol.test.ts"],
        tmp_path / "scratch",
    )

    assert result == 0, capsys.readouterr().out


def test_typescript_mutation_baseline_and_mutants_use_fresh_dist(tmp_path, capsys):
    source, _ = _typescript_freshness_fixture(tmp_path, "old")
    mutant = Mutant(
        path="runtime/agent-fabric-protocol/src/index.ts",
        line=1,
        before="export const value = 'new';",
        after="export const value = 'old';",
        description="current value reverted",
    )

    result = gate_changed_line_mutation(
        source,
        [mutant],
        _typescript_commands(),
        ["tests/protocol.test.ts"],
        tmp_path / "scratch",
        "crucial",
    )

    assert result == 0, capsys.readouterr().out


def test_typescript_mutation_rebuilds_dist_after_each_mutant(tmp_path, capsys):
    source, _ = _typescript_freshness_fixture(tmp_path, "new")
    mutant = Mutant(
        path="runtime/agent-fabric-protocol/src/index.ts",
        line=1,
        before="export const value = 'new';",
        after="export const value = 'old';",
        description="current value reverted",
    )

    result = gate_changed_line_mutation(
        source,
        [mutant],
        _typescript_commands(),
        ["tests/protocol.test.ts"],
        tmp_path / "scratch",
        "crucial",
    )

    assert result == 0, capsys.readouterr().out


def test_typescript_right_reason_red_prepares_base_with_its_own_writer_and_build(
    tmp_path, capsys
):
    source = _typescript_fixture(tmp_path)
    writer = source / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs"
    writer.write_text(
        "import { writeFile } from 'node:fs/promises';\n"
        "import { join } from 'node:path';\n"
        "await writeFile(join(process.argv[2], 'runtime/agent-fabric/.npm-ci-attestation'), 'current-writer\\n');\n",
        encoding="utf-8",
    )
    build = source / "scripts" / "agent-fabric-protocol-build"
    build.write_text(
        "#!/bin/sh\n"
        "set -eu\n"
        "printf 'current-build\\n' > \"$AGENTS_HOME/protocol-build-marker\"\n"
        "exit 1\n",
        encoding="utf-8",
    )
    build.chmod(0o755)
    source_attestation = source / "runtime" / "agent-fabric" / ".npm-ci-attestation"
    source_attestation.write_bytes(b"source-attestation\n")
    source_dist = source / "runtime" / "agent-fabric-protocol" / "dist" / "index.js"
    source_dist.parent.mkdir(parents=True)
    source_dist.write_bytes(b"source-dist\n")
    source_stamp = source_dist.parent / ".root-manifests.sha256"
    source_stamp.write_bytes(b"source-stamp\n")
    source_dist_before = {
        path.name: path.read_bytes() for path in source_dist.parent.iterdir()
    }

    result = gate_right_reason_red(
        source,
        "HEAD",
        {
            "py": f'{sys.executable} -c "raise SystemExit(1)" {{test}}',
            "ts": f'{sys.executable} tests/protocol_gate.py {{test}}',
        },
        ["tests/protocol.test.ts"],
        tmp_path / "scratch",
    )
    output = capsys.readouterr().out

    assert result == 0, output
    assert "classification=assertion-failure" in output
    assert source_attestation.read_bytes() == b"source-attestation\n"
    assert {
        path.name: path.read_bytes() for path in source_dist.parent.iterdir()
    } == source_dist_before
    assert not list((tmp_path / "scratch").glob("gate-*"))


def test_python_right_reason_red_skips_typescript_scratch_preflight(tmp_path, capsys):
    source = tmp_path / "source"
    source.mkdir()
    (source / "scripts").mkdir()
    build = source / "scripts" / "agent-fabric-protocol-build"
    build.write_text(
        "#!/bin/sh\n"
        "printf 'build must not run\\n' >&2\n"
        "exit 91\n",
        encoding="utf-8",
    )
    build.chmod(0o755)
    (source / "runtime" / "agent-fabric" / "scripts").mkdir(parents=True)
    (source / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs").write_text(
        "throw new Error('writer must not run');\n", encoding="utf-8"
    )
    (source / "tests").mkdir()
    (source / "tests" / "python_test.py").write_text("base test\n", encoding="utf-8")
    _commit_fixture(source)
    (source / "tests" / "python_test.py").write_text("current test\n", encoding="utf-8")

    result = gate_right_reason_red(
        source,
        "HEAD",
        {
            "py": f'{sys.executable} -c "raise AssertionError(\'python assertion\')" {{test}}',
            "ts": f'{sys.executable} -c "raise SystemExit(91)" {{test}}',
        },
        ["tests/python_test.py"],
        tmp_path / "scratch",
    )

    assert result == 0
    assert "classification=assertion-failure" in capsys.readouterr().out


def test_typescript_right_reason_red_reports_exact_missing_install_repair(tmp_path):
    source = _typescript_fixture(tmp_path, with_node_modules=False)
    ran = tmp_path / "test-ran"
    command = (
        f'{sys.executable} -c "from pathlib import Path; '
        f'Path({str(ran)!r}).write_text(\'ran\'); raise AssertionError(\'must not run\')" {{test}}'
    )

    with pytest.raises(
        GateError,
        match=r"^CHANGE_GATES_TS_PREREQUISITE: source node_modules is missing; "
        r"rerun: scripts/install-agent-fabric-dependencies$",
    ):
        gate_right_reason_red(
            source,
            "HEAD",
            {"py": command, "ts": command},
            ["tests/protocol.test.ts"],
            tmp_path / "scratch",
        )

    assert not ran.exists()


def test_typescript_right_reason_red_reports_typed_protocol_build_failure(tmp_path):
    source = _typescript_fixture(tmp_path, build_body="exit 7\n")
    ran = tmp_path / "test-ran"
    command = (
        f'{sys.executable} -c "from pathlib import Path; '
        f'Path({str(ran)!r}).write_text(\'ran\'); raise AssertionError(\'must not run\')" {{test}}'
    )

    with pytest.raises(GateError, match=r"^CHANGE_GATES_TS_BUILD: .*returncode=7"):
        gate_right_reason_red(
            source,
            "HEAD",
            {"py": command, "ts": command},
            ["tests/protocol.test.ts"],
            tmp_path / "scratch",
        )

    assert not ran.exists()


def test_typescript_scratch_preflight_rejects_base_symlink_escape(tmp_path):
    source = tmp_path / "source"
    outside = source / "outside"
    source.mkdir()
    outside.mkdir()
    (source / "runtime").mkdir()
    (source / "runtime" / "agent-fabric").symlink_to("../outside", target_is_directory=True)
    (source / "scripts").mkdir()
    build = source / "scripts" / "agent-fabric-protocol-build"
    build.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    build.chmod(0o755)
    (source / "tests").mkdir()
    (source / "tests" / "protocol.test.ts").write_text("base test\n", encoding="utf-8")
    _commit_fixture(source)
    (source / "runtime" / "agent-fabric").unlink()
    (source / "runtime" / "agent-fabric" / "scripts").mkdir(parents=True)
    (source / "runtime" / "agent-fabric" / "scripts" / "write-npm-ci-attestation.mjs").write_text(
        "throw new Error('current writer');\n", encoding="utf-8"
    )
    (source / "node_modules").mkdir()

    with pytest.raises(GateError, match=r"^CHANGE_GATES_TS_PREREQUISITE: .*contains a symlink"):
        gate_right_reason_red(
            source,
            "HEAD",
            {
                "py": f'{sys.executable} -c "raise SystemExit(1)" {{test}}',
                "ts": f'{sys.executable} -c "raise AssertionError(\'must not run\')" {{test}}',
            },
            ["tests/protocol.test.ts"],
            tmp_path / "scratch",
        )

    assert not (outside / ".npm-ci-attestation").exists()
