from pathlib import Path
import json
import re
import subprocess


ROOT = Path(__file__).resolve().parents[1]
VALIDATE = '"$(provenant root)/skills/deliver/scripts/validate_delivery.py"'
RECEIPT_AND_ARGS = '.agent-run/<id>/RUN.json --workspace-root "$PWD" --verify-hashes'
RECEIPT_INIT = '"$(provenant root)/skills/deliver/scripts/delivery_receipt.py" init'
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


def test_root_derived_workflow_paths_are_shell_quoted():
    polish = read("workflows/codebase-polish.js")
    implement = read("workflows/implement-run.js")

    assert 'run_dir_init.sh" "<root>/${runDirHintSuffix}"' in polish
    assert '--prompt-file "${runDir}/crossfamily/' in polish
    assert '--out "${runDir}/crossfamily/' in polish
    assert '> "${runDir}/crossfamily/' in polish
    assert 'run_dir_init.sh" "<abs run-dir>"' in implement
    assert 'mkdir -p "<abs run-dir>/patches"' in implement
    assert '--out "${runDir}/crossfamily/<name>.txt"' in implement


def test_checkpoint_workflow_quotes_json_arguments_for_shell_apostrophes():
    source = read("workflows/implement-run.js")
    assert "function shellQuote(value)" in source
    assert 'replaceAll("\'", "\'\\\\\'\'")' in source
    assert "--in-flight-json ${shellQuote(JSON.stringify(inFlight))}" in source
    assert "--artifact-paths-json ${shellQuote(JSON.stringify(artifactPaths))}" in source

    function = re.search(
        r"function shellQuote\(value\) \{\n  return .*\n\}", source
    )
    assert function is not None
    payload = json.dumps(["/tmp/path with spaces", "O'Brien"])
    rendered = subprocess.run(
        [
            "node",
            "-e",
            function.group(0)
            + f"\nprocess.stdout.write(shellQuote({json.dumps(payload)}));",
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    assert rendered.returncode == 0, rendered.stderr
    assert rendered.stdout == "'" + payload.replace("'", "'\\''") + "'"
