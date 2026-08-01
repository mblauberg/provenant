from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[1]
VALIDATE = '"${AGENTS_HOME:-$HOME/.agents}/skills/deliver/scripts/validate_delivery.py"'
RECEIPT_AND_ARGS = '.agent-run/<id>/RUN.json --workspace-root "$PWD" --verify-hashes'


def read(path: str) -> str:
    return (ROOT / path).read_text()


def test_delivery_and_implementation_guidance_names_receipt_and_safe_root():
    for path in (
        "skills/deliver/SKILL.md",
        "skills/deliver/references/contract.md",
        "skills/implement/SKILL.md",
        "skills/implement/references/run-contract.md",
    ):
        source = read(path)
        assert VALIDATE in source, path
        assert RECEIPT_AND_ARGS in source, path


def test_readme_product_commands_follow_the_explicit_checkout():
    source = read("README.md")
    shell = "\n".join(re.findall(r"```sh\n(.*?)```", source, re.DOTALL))
    assert 'cd "<PRODUCT_ROOT>"' in shell
    assert "scripts/agent-fabric-warm" in shell
    assert "scripts/install-harness" in shell
    assert "AGENTS_HOME" not in shell
    assert "\nscripts/manage_installation.py" not in source


def test_managed_reconciliation_stays_documented_for_maintainers():
    # The operator detail the README used to carry. A maintainer reads this with the
    # checkout as cwd, so a relative script path is the correct form here.
    source = read("MAINTAINING.md")
    assert "`config/skill-renames.json`" in source
    # Not anchored to a closing backtick. `plan` requires `--target`, so pinning
    # the bare command forbade documenting the flag that makes it runnable, and
    # the invariant here is that the command stays documented, not that it stays
    # documented incompletely.
    assert "scripts/manage_installation.py plan" in source
    assert "--target" in source
    assert "reconcile" in source
    assert "Never claim or overwrite an unmanaged target." in source


def test_delivery_scenario_replay_command_is_portable():
    source = read("skills/deliver/references/contract.md")
    assert "provenant check" in source
    assert "${AGENTS_HOME:-$HOME/.agents}/scripts/validate_delivery_scenarios.py" not in source
