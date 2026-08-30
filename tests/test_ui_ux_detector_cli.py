import json
import os
from pathlib import Path
import shutil
import subprocess


ROOT = Path(__file__).resolve().parents[1]
DETECT = ROOT / "skills" / "ui-ux-design" / "scripts" / "detect.mjs"
DETECTOR = ROOT / "runtime" / "ui-evidence" / "detector"


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


def test_detector_directory_scan_skips_symlinked_source_files(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    (project / "clean.tsx").write_text("export const Label = () => <span>Safe</span>;\n")
    outside = tmp_path / "outside.tsx"
    outside.write_text('<h1 class="bg-clip-text bg-gradient-to-r">Outside</h1>\n')
    (project / "leak.tsx").symlink_to(outside)

    result = _run_detect("--json", "--fast", str(project))

    assert result.returncode == 0
    assert json.loads(result.stdout) == []


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
    assert result.stdout.startswith(f'Usage: node "{DETECT}"')
    assert "impeccable detect" not in result.stdout
    assert "--fast" in result.stdout
    assert "--json" in result.stdout
    assert "URLs" in result.stdout
    assert result.stderr == ""


def test_detector_explicit_product_root_is_authoritative(tmp_path: Path) -> None:
    env = {
        **os.environ,
        "AGENT_FABRIC_PRODUCT_ROOT": str(tmp_path / "missing-product"),
    }
    env.pop("AGENTS_HOME", None)

    result = _run_detect("--help", env=env)

    assert result.returncode == 1
    assert result.stdout == ""
    assert "runtime/ui-evidence" in result.stderr
    assert str(tmp_path / "missing-product") in result.stderr


def _write_stub_runtime(product_root: Path, label: str) -> None:
    runtime = product_root / "runtime" / "ui-evidence"
    runtime.mkdir(parents=True)
    (runtime / "detect.mjs").write_text(
        "export async function detectCli() {"
        f"process.stdout.write({json.dumps(label)});"
        "}\n"
    )


def test_detector_product_root_precedes_agents_home(tmp_path: Path) -> None:
    selected = tmp_path / "selected"
    ignored = tmp_path / "ignored"
    _write_stub_runtime(selected, "selected")
    _write_stub_runtime(ignored, "ignored")
    env = {
        **os.environ,
        "AGENT_FABRIC_PRODUCT_ROOT": str(selected),
        "AGENTS_HOME": str(ignored),
    }

    result = _run_detect("--help", env=env)

    assert result.returncode == 0
    assert result.stdout == "selected"
    assert result.stderr == ""


def test_detector_accepts_agents_home_as_product_root(tmp_path: Path) -> None:
    product = tmp_path / "product"
    _write_stub_runtime(product, "agents-home")
    env = {**os.environ, "AGENTS_HOME": str(product)}
    env.pop("AGENT_FABRIC_PRODUCT_ROOT", None)

    result = _run_detect("--help", env=env)

    assert result.returncode == 0
    assert result.stdout == "agents-home"
    assert result.stderr == ""


def test_detector_rejects_relative_configured_product_root() -> None:
    env = {**os.environ, "AGENT_FABRIC_PRODUCT_ROOT": "relative-product"}
    env.pop("AGENTS_HOME", None)

    result = _run_detect("--help", env=env)

    assert result.returncode == 1
    assert result.stdout == ""
    assert "must be an absolute product root" in result.stderr


def test_detector_source_checkout_fallback_works_from_unrelated_cwd(
    tmp_path: Path,
) -> None:
    env = dict(os.environ)
    env.pop("AGENT_FABRIC_PRODUCT_ROOT", None)
    env.pop("AGENTS_HOME", None)

    result = _run_detect("--help", env=env, cwd=tmp_path)

    assert result.returncode == 0
    assert result.stdout.startswith(f'Usage: node "{DETECT}"')
    assert result.stderr == ""


def test_detector_source_checkout_fallback_works_through_installed_skill_symlink(
    tmp_path: Path,
) -> None:
    linked_skill = tmp_path / "skills" / "ui-ux-design"
    linked_skill.parent.mkdir(parents=True)
    linked_skill.symlink_to(DETECT.parent.parent, target_is_directory=True)
    linked_detect = linked_skill / "scripts" / "detect.mjs"
    env = dict(os.environ)
    env.pop("AGENT_FABRIC_PRODUCT_ROOT", None)
    env.pop("AGENTS_HOME", None)

    result = subprocess.run(
        ["node", str(linked_detect), "--help"],
        check=False,
        capture_output=True,
        text=True,
        cwd=tmp_path,
        env=env,
    )

    assert result.returncode == 0
    assert result.stdout.startswith(f'Usage: node "{linked_detect}"')
    assert result.stderr == ""


def test_detector_does_not_resolve_runtime_from_target_cwd(tmp_path: Path) -> None:
    installed_root = tmp_path / "installed"
    installed_scripts = installed_root / "skills" / "ui-ux-design" / "scripts"
    installed_scripts.mkdir(parents=True)
    shutil.copy2(DETECT, installed_scripts / "detect.mjs")
    shutil.copy2(
        DETECT.parent / "ui-evidence-paths.mjs",
        installed_scripts / "ui-evidence-paths.mjs",
    )
    target = tmp_path / "target"
    old_runtime = target / "node_modules" / "impeccable" / "cli" / "engine"
    old_runtime.mkdir(parents=True)
    (old_runtime / "detect-antipatterns.mjs").write_text(
        "export async function detectCli() { process.stdout.write('cwd-runtime'); }\n"
    )
    env = dict(os.environ)
    env.pop("AGENT_FABRIC_PRODUCT_ROOT", None)
    env.pop("AGENTS_HOME", None)

    result = subprocess.run(
        ["node", str(installed_scripts / "detect.mjs"), "--help"],
        check=False,
        capture_output=True,
        text=True,
        cwd=target,
        env=env,
    )

    assert result.returncode == 1
    assert result.stdout == ""
    assert "runtime/ui-evidence" in result.stderr
    assert "cwd-runtime" not in result.stdout


def test_detector_runtime_is_not_duplicated_in_the_skill() -> None:
    skill_detector = DETECT.parent / "detector"

    assert not skill_detector.exists()
    assert (DETECTOR / "detect-antipatterns-browser.js").is_file()
    assert len(DETECT.read_text().splitlines()) <= 16
    assert "ui-evidence-paths.mjs" in DETECT.read_text()


def test_detector_runtime_entry_is_directly_runnable() -> None:
    runtime_entry = ROOT / "runtime" / "ui-evidence" / "detect.mjs"

    result = subprocess.run(
        ["node", str(runtime_entry), "--help"],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0
    assert result.stdout.startswith(f'Usage: node "{runtime_entry}"')
    assert result.stderr == ""


def test_detector_html_requires_the_static_engine_unless_fast_is_explicit(
    tmp_path: Path,
) -> None:
    source = tmp_path / "clean.html"
    source.write_text("<main><h1>Account</h1></main>\n")

    result = _run_detect("--json", str(source))

    assert result.returncode == 3
    assert json.loads(result.stdout) == {
        "status": "incomplete",
        "findings": [],
        "errors": [
            {
                "target": str(source),
                "code": "engine_unavailable",
                "message": (
                    "Static HTML engine unavailable: parser modules are missing; "
                    "rerun with --fast for regex-only scanning"
                ),
            }
        ],
    }
    assert result.stderr == ""

    fast = _run_detect("--json", "--fast", str(source))
    assert fast.returncode == 0
    assert json.loads(fast.stdout) == []
    assert fast.stderr == ""


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
        DETECTOR
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
        DETECTOR
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


def test_direct_and_pooled_url_detection_share_readiness_defaults_and_overrides() -> None:
    module_url = (
        DETECTOR
        / "engines"
        / "browser"
        / "detect-url.mjs"
    ).as_uri()
    script = (
        f"import {{ createBrowserDetector, detectUrl }} from {json.dumps(module_url)};"
        "const observations=[];"
        "globalThis.setTimeout=(resolve,ms)=>{observations.push({kind:'settle',ms});resolve()};"
        "const browser={newPage:async()=>({"
        "setViewport:async()=>{},"
        "goto:async(_url,options)=>observations.push({kind:'goto',options}),"
        "evaluate:async()=>[],close:async()=>{}}),close:async()=>{}};"
        "await detectUrl('http://example.invalid',{browser,visualContrast:false});"
        "const pooled=await createBrowserDetector({browser});"
        "await pooled.detectUrl('http://example.invalid',{visualContrast:false});"
        "await detectUrl('http://example.invalid',{browser,visualContrast:false,waitUntil:'domcontentloaded',settleMs:0});"
        "await pooled.detectUrl('http://example.invalid',{visualContrast:false,waitUntil:'networkidle2',settleMs:0});"
        "process.stdout.write(JSON.stringify(observations));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    observations = json.loads(result.stdout)
    assert [item["options"]["waitUntil"] for item in observations if item["kind"] == "goto"] == [
        "load",
        "load",
        "domcontentloaded",
        "networkidle2",
    ]
    assert [item["ms"] for item in observations if item["kind"] == "settle"] == [250, 250]


def test_url_detection_waits_for_delayed_client_render_before_scanning() -> None:
    module_url = (
        DETECTOR
        / "engines"
        / "browser"
        / "detect-url.mjs"
    ).as_uri()
    script = (
        f"import {{ detectUrl }} from {json.dumps(module_url)};"
        "let ready=false;let functionCalls=0;"
        "globalThis.setTimeout=(resolve,ms)=>{if(ms>=175)ready=true;resolve()};"
        "const page={setViewport:async()=>{},goto:async()=>{},close:async()=>{},"
        "evaluate:async(input)=>{"
        "if(typeof input==='string')return;"
        "functionCalls++;if(functionCalls===1)return;"
        "return ready?[{selector:'main',findings:[{type:'nested-cards',detail:'late UI'}]}]:[];}};"
        "const browser={newPage:async()=>page,close:async()=>{}};"
        "const findings=await detectUrl('http://example.invalid',{browser,visualContrast:false});"
        "process.stdout.write(JSON.stringify(findings));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)[0]["antipattern"] == "nested-cards"


def test_framework_probe_does_not_follow_redirects_off_origin() -> None:
    module_url = (
        DETECTOR
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


def test_framework_probe_follows_bounded_same_origin_redirects() -> None:
    module_url = (
        DETECTOR
        / "node"
        / "file-system.mjs"
    ).as_uri()
    script = (
        "import http from 'node:http';"
        f"import {{ isPortListening }} from {json.dumps(module_url)};"
        "const paths=[];"
        "const origin=http.createServer((req,res)=>{"
        "paths.push(req.url);"
        "if(req.url==='/'){res.writeHead(302,{location:'/app'});res.end();return;}"
        "if(req.url==='/app'){res.writeHead(307,{location:'/ready'});res.end();return;}"
        "res.end('<script type=module src=\"/@vite/client\"></script>');"
        "});"
        "await new Promise(resolve=>origin.listen(0,'127.0.0.1',resolve));"
        "const result=await isPortListening(origin.address().port,{body:/@vite\\/client/});"
        "await new Promise(resolve=>origin.close(resolve));"
        "process.stdout.write(JSON.stringify({result,paths}));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == {
        "result": {"listening": True, "matched": True},
        "paths": ["/", "/app", "/ready"],
    }
