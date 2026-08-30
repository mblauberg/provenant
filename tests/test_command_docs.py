from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[1]
VALIDATE = '"${AGENTS_HOME:-$HOME/.agents}/skills/deliver/scripts/validate_delivery.py"'
RECEIPT_AND_ARGS = '.agent-run/<id>/RUN.json --workspace-root "$PWD" --verify-hashes'
RECEIPT_INIT = '"${AGENTS_HOME:-$HOME/.agents}/skills/deliver/scripts/delivery_receipt.py" init'
REQUIRED_INIT_FLAGS = {
    "--run-dir",
    "--run-id",
    "--profile",
    "--chair-family",
    "--risk-assessment",
    "--intent",
    "--authority",
}


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

    deliver = read("skills/deliver/SKILL.md")
    assert RECEIPT_INIT in deliver
    init_blocks = [
        block
        for block in re.findall(r"```sh\n(.*?)```", deliver, re.DOTALL)
        if RECEIPT_INIT in block
    ]
    assert len(init_blocks) == 1
    assert REQUIRED_INIT_FLAGS <= set(re.findall(r"--[a-z-]+", init_blocks[0]))
    implement_contract = read("skills/implement/references/run-contract.md")
    assert "complete `init` command in `deliver`" in implement_contract
    assert "../../deliver/references/" not in implement_contract


def test_readme_product_commands_follow_the_explicit_checkout():
    source = read("README.md")
    shell = "\n".join(re.findall(r"```sh\n(.*?)```", source, re.DOTALL))
    assert 'cd "<PRODUCT_ROOT>"' in shell
    assert "npm ci" in shell
    assert "scripts/install-harness" in shell
    assert "AGENTS_HOME" not in shell
    assert "\nscripts/manage_installation.py" not in source


def test_managed_reconciliation_stays_documented_for_maintainers():
    # The operator detail the README used to carry. A maintainer reads this with the
    # checkout as cwd, so a relative script path is the correct form here.
    source = read("MAINTAINING.md")
    assert (
        "Record current skill ownership in the managed installation manifest."
        in source
    )
    assert "Record skill ownership and supersession" not in source
    # Not anchored to a closing backtick. `plan` requires `--target`, so pinning
    # the bare command forbade documenting the flag that makes it runnable, and
    # the invariant here is that the command stays documented, not that it stays
    # documented incompletely.
    assert "scripts/manage_installation.py plan" in source
    assert "--target" in source
    assert "reconcile" in source
    assert "current ownership record" in source
    assert "Never claim or overwrite an unmanaged target." in source


def test_installer_adr_does_not_advertise_retired_rename_reconciliation():
    source = read("docs/adr/0019-installed-file-class-ownership.md")
    assert "Rename reconciliation was retired by #647" in source
    assert "preserves unmanaged targets" in source
    assert "applies declared renames" not in source


def test_lifecycle_spec_uses_current_manifest_contract():
    source = read("docs/specs/harness/lifecycle.md")
    normalised = " ".join(source.split())
    assert (
        "Use a versioned installation manifest containing skill name, source "
        "digest, installed target and current ownership."
        in normalised
    )
    assert "supersession history" not in source


def test_adr_index_marks_daemon_era_task_completion_decision_superseded():
    source = read("docs/adr/README.md")
    row = next(line for line in source.splitlines() if "[0015](" in line)
    assert "Superseded by ADR 0020" in row
