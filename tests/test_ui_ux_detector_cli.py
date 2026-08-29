import json
import os
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[1]
DETECT = ROOT / "skills" / "ui-ux-design" / "scripts" / "detect.mjs"


def _run_detect(
    *args: str,
    input_text: str | None = None,
    env: dict[str, str] | None = None,
    cwd: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["node", str(DETECT), *args],
        check=False,
        capture_output=True,
        text=True,
        input=input_text,
        env=env,
        cwd=cwd,
    )


def test_detector_json_clean_file_is_an_empty_findings_list(tmp_path: Path) -> None:
    source = tmp_path / "clean.tsx"
    source.write_text("export function Label() { return <span>Account</span>; }\n")

    result = _run_detect("--json", str(source))

    assert result.returncode == 0
    assert json.loads(result.stdout) == []
    assert result.stderr == ""


def test_detector_json_finding_is_a_nonempty_findings_list() -> None:
    result = _run_detect(
        "--json",
        input_text='<h1 class="bg-clip-text text-transparent bg-gradient-to-r">Hello</h1>',
    )

    assert result.returncode == 2
    findings = json.loads(result.stdout)
    assert isinstance(findings, list) and findings
    gradient = next(finding for finding in findings if finding["antipattern"] == "gradient-text")
    assert gradient == {
        "antipattern": "gradient-text",
        "name": "Gradient text",
        "description": "Gradient text is decorative rather than meaningful — a common AI tell, especially on headings and metrics. Use solid colors for text.",
        "severity": "warning",
        "file": "<stdin>",
        "line": 1,
        "snippet": "bg-clip-text + bg-gradient",
    }
    assert result.stderr == ""


def test_detector_help_is_stable_and_side_effect_free() -> None:
    result = _run_detect("--help")

    assert result.returncode == 0
    assert "--fast" in result.stdout
    assert "--json" in result.stdout
    assert "URLs" in result.stdout
    assert result.stderr == ""


def test_detector_accepts_flags_before_or_after_detect_subcommand(tmp_path: Path) -> None:
    source = tmp_path / "clean.tsx"
    source.write_text("export const Label = () => <span>Account</span>;\n")

    for args in (("--json", "detect", str(source)), ("detect", "--json", str(source))):
        result = _run_detect(*args)
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout) == []
        assert result.stderr == ""


def test_detector_preserves_a_single_positional_target_named_detect(tmp_path: Path) -> None:
    source = tmp_path / "detect"
    source.write_text('<h1 class="bg-clip-text bg-gradient-to-r">Hello</h1>\n')

    result = _run_detect("--json", "detect", cwd=tmp_path)

    assert result.returncode == 2
    findings = json.loads(result.stdout)
    assert any(finding["antipattern"] == "gradient-text" for finding in findings)
    assert result.stderr == ""


def test_detector_json_all_target_failure_is_not_a_clean_result(tmp_path: Path) -> None:
    result = _run_detect(
        "--json",
        str(tmp_path / "missing-one.html"),
        str(tmp_path / "missing-two.css"),
    )

    assert result.returncode == 1
    assert json.loads(result.stdout) == {
        "status": "incomplete",
        "findings": [],
        "errors": [
            {
                "target": str(tmp_path / "missing-one.html"),
                "code": "target_unavailable",
                "message": f"Cannot access {tmp_path / 'missing-one.html'}",
            },
            {
                "target": str(tmp_path / "missing-two.css"),
                "code": "target_unavailable",
                "message": f"Cannot access {tmp_path / 'missing-two.css'}",
            },
        ],
    }
    assert result.stderr == ""


def test_detector_json_distinguishes_unavailable_browser_engine_deterministically() -> None:
    env = {**os.environ, "IMPECCABLE_BROWSER_ENGINE": "unavailable"}
    result = _run_detect("--json", "https://example.com", env=env)

    assert result.returncode == 3
    assert json.loads(result.stdout) == {
        "status": "incomplete",
        "findings": [],
        "errors": [
            {
                "target": "https://example.com",
                "code": "engine_unavailable",
                "message": "Browser engine unavailable: install puppeteer to scan URLs",
            }
        ],
    }
    assert result.stderr == ""


def test_detector_json_reports_each_url_when_shared_browser_engine_is_unavailable() -> None:
    env = {**os.environ, "IMPECCABLE_BROWSER_ENGINE": "unavailable"}
    result = _run_detect(
        "--json",
        "https://one.example",
        "https://two.example",
        env=env,
    )

    assert result.returncode == 3
    payload = json.loads(result.stdout)
    assert payload["status"] == "incomplete"
    assert payload["findings"] == []
    assert [error["target"] for error in payload["errors"]] == [
        "https://one.example",
        "https://two.example",
    ]
    assert {error["code"] for error in payload["errors"]} == {"engine_unavailable"}
    assert result.stderr == ""


def test_detector_json_turns_graph_and_file_read_errors_into_one_incomplete_result(
    tmp_path: Path,
) -> None:
    source = tmp_path / "clean.tsx"
    source.write_text("export const Label = () => <span>Account</span>;\n")
    (tmp_path / "broken.tsx").symlink_to(tmp_path / "missing.tsx")

    result = _run_detect("--json", str(tmp_path))

    assert result.returncode == 1
    payload = json.loads(result.stdout)
    assert payload["status"] == "incomplete"
    assert payload["findings"] == []
    assert payload["errors"]
    assert {error["code"] for error in payload["errors"]} <= {"graph_read_failed", "scan_failed"}
    assert all("stack" not in error for error in payload["errors"])
    assert result.stdout.count('"status"') == 1
    assert result.stderr == ""


def test_detector_json_reports_an_unreadable_root_directory(tmp_path: Path) -> None:
    source = tmp_path / "locked"
    source.mkdir()
    (source / "hidden.tsx").write_text("export const Hidden = () => <div />;\n")
    source.chmod(0)
    try:
        result = _run_detect("--json", str(source))
    finally:
        source.chmod(0o700)

    assert result.returncode == 1
    assert json.loads(result.stdout) == {
        "status": "incomplete",
        "findings": [],
        "errors": [
            {
                "target": str(source),
                "code": "directory_read_failed",
                "message": f"Unable to read directory {source}",
            }
        ],
    }
    assert result.stderr == ""


def test_detector_json_reports_an_unreadable_nested_directory(tmp_path: Path) -> None:
    source = tmp_path / "project"
    locked = source / "nested"
    locked.mkdir(parents=True)
    (locked / "hidden.tsx").write_text("export const Hidden = () => <div />;\n")
    locked.chmod(0)
    try:
        result = _run_detect("--json", str(source))
    finally:
        locked.chmod(0o700)

    assert result.returncode == 1
    payload = json.loads(result.stdout)
    assert payload["status"] == "incomplete"
    assert payload["findings"] == []
    assert payload["errors"] == [
        {
            "target": "nested",
            "code": "directory_read_failed",
            "message": "Unable to read directory nested",
        }
    ]
    assert result.stderr == ""


def test_detector_json_structures_stdin_wrapper_scan_failures(tmp_path: Path) -> None:
    source = tmp_path / "broken.html"
    source.mkdir()
    wrapper = json.dumps({"tool_input": {"file_path": str(source)}})

    result = _run_detect("--json", input_text=wrapper)

    assert result.returncode == 1
    payload = json.loads(result.stdout)
    assert payload["status"] == "incomplete"
    assert payload["findings"] == []
    assert payload["errors"] == [
        {
            "target": str(source),
            "code": "scan_failed",
            "message": f"Unable to scan stdin wrapper file {source}",
        }
    ]
    assert result.stderr == ""


def test_detector_closes_an_owned_browser_when_new_page_fails(tmp_path: Path) -> None:
    marker = tmp_path / "closed"
    module_url = (
        ROOT
        / "skills"
        / "ui-ux-design"
        / "scripts"
        / "detector"
        / "engines"
        / "browser"
        / "detect-url.mjs"
    ).as_uri()
    script = (
        f"import {{ detectUrl }} from {json.dumps(module_url)};"
        "const marker=process.argv[1];"
        "const browser={newPage:async()=>{throw new Error('new-page-failed')},"
        "close:async()=>{await import('node:fs').then(({writeFileSync})=>writeFileSync(marker,'closed'))}};"
        "try{await detectUrl('http://example.invalid',{launchBrowser:async()=>browser})}catch{}"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(marker)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert marker.read_text() == "closed"


def test_browser_detector_accepts_injected_browser_or_launcher_without_puppeteer() -> None:
    module_url = (
        ROOT
        / "skills"
        / "ui-ux-design"
        / "scripts"
        / "detector"
        / "engines"
        / "browser"
        / "detect-url.mjs"
    ).as_uri()
    script = (
        f"import {{ createBrowserDetector }} from {json.dumps(module_url)};"
        "const external={close:async()=>{throw new Error('must-not-close')}};"
        "const supplied=await createBrowserDetector({browser:external});"
        "await supplied.close();"
        "let launched=0;let closed=0;"
        "const owned=await createBrowserDetector({launchBrowser:async()=>{launched++;return {close:async()=>{closed++}}}});"
        "await owned.close();"
        "process.stdout.write(JSON.stringify({same:supplied.browser===external,launched,closed}));"
    )
    env = {**os.environ, "IMPECCABLE_BROWSER_ENGINE": "unavailable"}

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        check=False,
        capture_output=True,
        text=True,
        env=env,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == {"same": True, "launched": 1, "closed": 1}


def test_framework_probe_does_not_follow_redirects_off_origin() -> None:
    module_url = (
        ROOT
        / "skills"
        / "ui-ux-design"
        / "scripts"
        / "detector"
        / "node"
        / "file-system.mjs"
    ).as_uri()
    script = (
        "import http from 'node:http';"
        f"import {{ isPortListening }} from {json.dumps(module_url)};"
        "let offOriginHits=0;"
        "const destination=http.createServer((req,res)=>{offOriginHits++;res.end('OFF_ORIGIN')});"
        "await new Promise(resolve=>destination.listen(0,'127.0.0.1',resolve));"
        "const destinationPort=destination.address().port;"
        "const origin=http.createServer((req,res)=>{res.writeHead(302,{location:`http://127.0.0.1:${destinationPort}/elsewhere`});res.end()});"
        "await new Promise(resolve=>origin.listen(0,'127.0.0.1',resolve));"
        "const result=await isPortListening(origin.address().port,{body:/OFF_ORIGIN/});"
        "await new Promise(resolve=>origin.close(resolve));"
        "await new Promise(resolve=>destination.close(resolve));"
        "process.stdout.write(JSON.stringify({result,offOriginHits}));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == {
        "result": {"listening": True, "matched": False},
        "offOriginHits": 0,
    }
