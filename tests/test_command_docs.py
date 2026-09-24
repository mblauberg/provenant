from pathlib import Path
import json
import re
import subprocess


ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text()


def test_checkpoint_workflow_quotes_json_arguments_for_shell_apostrophes():
    source = read("workflows/implement-run.js")
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
    round_trip = subprocess.run(
        ["sh", "-c", f"set -- {rendered.stdout}; printf '%s' \"$1\""],
        text=True,
        capture_output=True,
        check=False,
    )
    assert round_trip.returncode == 0, round_trip.stderr
    assert round_trip.stdout == payload
