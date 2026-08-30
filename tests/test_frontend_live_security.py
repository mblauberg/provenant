import base64
import json
import http.client
import os
from pathlib import Path
import socket
import subprocess
import time
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen

import pytest


ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "skills" / "ui-ux-design" / "scripts"
INJECT = SCRIPTS / "live-inject.mjs"
SERVER = SCRIPTS / "live-server.mjs"
WRAP = SCRIPTS / "live-wrap.mjs"
ACCEPT = SCRIPTS / "live-accept.mjs"
RESUME = SCRIPTS / "live-resume.mjs"
STATUS = SCRIPTS / "live-status.mjs"
TOKEN = "11111111-1111-4111-8111-111111111111"


def _write_config(project: Path, files: list[str]) -> None:
    config_dir = project / ".impeccable" / "live"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "config.json").write_text(
        json.dumps(
            {
                "files": files,
                "insertBefore": "</body>",
                "commentSyntax": "html",
            }
        )
    )


def _run_inject(project: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["node", str(INJECT), *args],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
    )


def _run_completion_type_probe(
    project: Path, event_type: str, accept_result: dict[str, object]
) -> subprocess.CompletedProcess[str]:
    module_url = (SCRIPTS / "live-completion.mjs").as_uri()
    script = (
        f"import {{ completionTypeForAcceptResult }} from {json.dumps(module_url)};"
        f"process.stdout.write(completionTypeForAcceptResult({json.dumps(event_type)},"
        f"{json.dumps(accept_result)}));"
    )
    return subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
    )


def _run_poll_reply_payload_probe(project: Path) -> subprocess.CompletedProcess[str]:
    module_url = (SCRIPTS / "live-poll.mjs").as_uri()
    script = (
        f"import {{ buildPollReplyPayload }} from {json.dumps(module_url)};"
        "process.stdout.write(JSON.stringify(buildPollReplyPayload('agent-secret',"
        "{id:'deadbeef',type:'done',leaseToken:'lease-secret'})));"
    )
    return subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
    )


def _run_wrap(project: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [
            "node",
            str(WRAP),
            "--id",
            "security-test",
            "--count",
            "1",
            "--query",
            "target",
            *args,
        ],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
    )


def _run_accept(project: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["node", str(ACCEPT), "--id", "security-test", *args],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
    )


@pytest.mark.parametrize(
    ("extra_args", "expected_error"),
    [
        (["--id", "../escape", "--count", "3"], "invalid_session_id"),
        (["--id", "a" * 129, "--count", "3"], "invalid_session_id"),
        (["--id", "security-test", "--count", "0"], "invalid_variant_count"),
        (["--id", "security-test", "--count", "9"], "invalid_variant_count"),
        (["--id", "security-test", "--count", "2junk"], "invalid_variant_count"),
    ],
)
def test_live_wrap_rejects_malformed_identity_or_count_before_writing(
    tmp_path: Path, extra_args: list[str], expected_error: str,
) -> None:
    page = tmp_path / "index.html"
    original = b'<section class="target">original</section>\n'
    page.write_bytes(original)

    result = subprocess.run(
        [
            "node",
            str(WRAP),
            *extra_args,
            "--classes",
            "target",
            "--file",
            "index.html",
        ],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode != 0
    assert json.loads(result.stderr)["error"] == expected_error
    assert page.read_bytes() == original


@pytest.mark.parametrize(
    ("args", "expected_error"),
    [
        (["--id", "../escape", "--discard"], "invalid_session_id"),
        (["--id", "a" * 129, "--discard"], "invalid_session_id"),
        (["--id", "security-test", "--variant", "0"], "invalid_variant_id"),
        (["--id", "security-test", "--variant", "9"], "invalid_variant_id"),
        (["--id", "security-test", "--variant", "1junk"], "invalid_variant_id"),
    ],
)
def test_live_accept_rejects_malformed_identity_or_variant_before_writing(
    tmp_path: Path, args: list[str], expected_error: str,
) -> None:
    page = tmp_path / "index.html"
    page.write_text('<section class="target">original</section>\n')
    wrapped = _run_wrap(tmp_path, "--file", "index.html")
    assert wrapped.returncode == 0, wrapped.stderr
    original = page.read_bytes()

    result = subprocess.run(
        ["node", str(ACCEPT), *args],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode != 0
    assert json.loads(result.stderr)["error"] == expected_error
    assert page.read_bytes() == original


def _write_wrapped_session(project: Path, *, with_variant: bool) -> Path:
    page = project / "index.html"
    page.write_text('<html><body><section class="target">original</section></body></html>\n')
    wrapped = _run_wrap(project, "--file", "index.html")
    assert wrapped.returncode == 0, wrapped.stderr
    if with_variant:
        marker = "<!-- Variants: insert below this line -->"
        page.write_text(
            page.read_text().replace(
                marker,
                marker
                + '\n  <div data-impeccable-variant="1"><section class="target">accepted</section></div>',
            )
        )
    return page


def _run_contained_source_probe(project: Path, body: str) -> subprocess.CompletedProcess[str]:
    module_url = (SCRIPTS / "contained-source.mjs").as_uri()
    script = (
        f"import {{ readContainedSource, replaceContainedSources }} from {json.dumps(module_url)};"
        + body
    )
    return subprocess.run(
        ["node", "--input-type=module", "-e", script, str(project)],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
    )


def test_contained_source_batch_cleans_staged_files_if_later_staging_fails(tmp_path: Path) -> None:
    first = tmp_path / "first.html"
    second = tmp_path / "second.html"
    first.write_text("first")
    second.write_text("second")
    result = _run_contained_source_probe(
        tmp_path,
        "const root=process.argv[1];"
        "const first=readContainedSource(root,'first.html',{relativeOnly:true});"
        "const second=readContainedSource(root,'second.html',{relativeOnly:true});"
        "const bad={toString(){throw new Error('forced-stage-failure')}};"
        "try{replaceContainedSources([{snapshot:first,content:'changed'},{snapshot:second,content:bad}]);}"
        "catch(error){process.stdout.write(JSON.stringify({code:error.code}));process.exit(7)}",
    )

    assert result.returncode == 7
    assert json.loads(result.stdout)["code"] == "source_replace_failed"
    assert first.read_text() == "first"
    assert second.read_text() == "second"
    assert not list(tmp_path.glob(".*.tmp"))


def test_contained_source_batch_rolls_back_prior_replacements_on_commit_failure(tmp_path: Path) -> None:
    first = tmp_path / "first.html"
    second = tmp_path / "second.html"
    first.write_text("first")
    second.write_text("second")
    result = _run_contained_source_probe(
        tmp_path,
        "const root=process.argv[1];"
        "const first=readContainedSource(root,'first.html',{relativeOnly:true});"
        "const second=readContainedSource(root,'second.html',{relativeOnly:true});"
        "try{replaceContainedSources([{snapshot:first,content:'changed-first'},"
        "{snapshot:second,content:'changed-second'}],"
        "{beforeReplace({index}){if(index===1)throw new Error('forced-commit-failure')}});}"
        "catch(error){process.stdout.write(JSON.stringify({code:error.code}));process.exit(7)}",
    )

    assert result.returncode == 7
    assert json.loads(result.stdout)["code"] == "source_replace_failed"
    assert first.read_text() == "first"
    assert second.read_text() == "second"
    assert not list(tmp_path.glob(".*.tmp"))


def test_contained_source_rejects_same_inode_content_change_after_snapshot(tmp_path: Path) -> None:
    source = tmp_path / "page.html"
    source.write_text("first")
    inode = source.stat().st_ino
    result = _run_contained_source_probe(
        tmp_path,
        "const fs=(await import('node:fs')).default;"
        "const root=process.argv[1];"
        "const snapshot=readContainedSource(root,'page.html',{relativeOnly:true});"
        "fs.writeFileSync(snapshot.path,'other');"
        "try{replaceContainedSources([{snapshot,content:'replacement'}]);}"
        "catch(error){process.stdout.write(JSON.stringify({code:error.code}));process.exit(7)}",
    )

    assert result.returncode == 7
    assert json.loads(result.stdout)["code"] == "source_path_changed"
    assert source.stat().st_ino == inode
    assert source.read_text() == "other"


def test_contained_source_parent_swap_after_open_never_redirects_write(tmp_path: Path) -> None:
    parent = tmp_path / "components"
    parent.mkdir()
    source = parent / "page.html"
    source.write_text("original")
    result = _run_contained_source_probe(
        tmp_path,
        "const fs=(await import('node:fs')).default;"
        "const path=(await import('node:path')).default;"
        "const root=process.argv[1];"
        "const snapshot=readContainedSource(root,'components/page.html',{relativeOnly:true});"
        "try{replaceContainedSources([{snapshot,content:'replacement'}],{afterOpen(){"
        "fs.renameSync(path.join(root,'components'),path.join(root,'parked'));"
        "fs.mkdirSync(path.join(root,'components'));"
        "fs.writeFileSync(path.join(root,'components/page.html'),'attacker');"
        "}});}catch(error){process.stdout.write(JSON.stringify({code:error.code}));process.exit(7)}",
    )

    assert result.returncode == 7
    assert json.loads(result.stdout)["code"] == "source_replace_failed"
    assert (tmp_path / "parked" / "page.html").read_text() == "original"
    assert (tmp_path / "components" / "page.html").read_text() == "attacker"


def test_contained_source_rejects_a_file_with_no_write_bits(tmp_path: Path) -> None:
    source = tmp_path / "page.html"
    source.write_text("original")
    source.chmod(0o444)
    result = _run_contained_source_probe(
        tmp_path,
        "const root=process.argv[1];"
        "const snapshot=readContainedSource(root,'page.html',{relativeOnly:true});"
        "try{replaceContainedSources([{snapshot,content:'replacement'}]);}"
        "catch(error){process.stdout.write(JSON.stringify({code:error.code}));process.exit(7)}",
    )

    assert result.returncode == 7
    assert json.loads(result.stdout)["code"] == "source_not_writable"
    assert source.read_text() == "original"
    assert source.stat().st_mode & 0o777 == 0o444


def test_contained_source_rolls_back_the_current_descriptor_after_write_failure(
    tmp_path: Path,
) -> None:
    first = tmp_path / "first.html"
    second = tmp_path / "second.html"
    first.write_text("first")
    second.write_text("second")
    result = _run_contained_source_probe(
        tmp_path,
        "const root=process.argv[1];"
        "const first=readContainedSource(root,'first.html',{relativeOnly:true});"
        "const second=readContainedSource(root,'second.html',{relativeOnly:true});"
        "try{replaceContainedSources([{snapshot:first,content:'changed-first'},"
        "{snapshot:second,content:'changed-second'}],"
        "{afterWrite({index}){if(index===1)throw new Error('forced-after-write-failure')}});}"
        "catch(error){process.stdout.write(JSON.stringify({code:error.code}));process.exit(7)}",
    )

    assert result.returncode == 7
    assert json.loads(result.stdout)["code"] == "source_replace_failed"
    assert first.read_text() == "first"
    assert second.read_text() == "second"


def test_contained_source_rechecks_later_descriptor_immediately_before_write(
    tmp_path: Path,
) -> None:
    first = tmp_path / "first.html"
    second = tmp_path / "second.html"
    first.write_text("first")
    second.write_text("second")
    result = _run_contained_source_probe(
        tmp_path,
        "const fs=(await import('node:fs')).default;"
        "const root=process.argv[1];"
        "const first=readContainedSource(root,'first.html',{relativeOnly:true});"
        "const second=readContainedSource(root,'second.html',{relativeOnly:true});"
        "try{replaceContainedSources([{snapshot:first,content:'changed-first'},"
        "{snapshot:second,content:'changed-second'}],"
        "{beforeReplace({index}){if(index===1)fs.writeFileSync(second.path,'concurrent')}});}"
        "catch(error){process.stdout.write(JSON.stringify({code:error.code}));process.exit(7)}",
    )

    assert result.returncode == 7
    assert json.loads(result.stdout)["code"] == "source_replace_failed"
    assert first.read_text() == "first"
    assert second.read_text() == "concurrent"


def test_contained_source_rechecks_hard_links_immediately_before_write(
    tmp_path: Path,
) -> None:
    first = tmp_path / "first.html"
    second = tmp_path / "second.html"
    first.write_text("first")
    second.write_text("second")
    result = _run_contained_source_probe(
        tmp_path,
        "const fs=(await import('node:fs')).default;"
        "const path=(await import('node:path')).default;"
        "const root=process.argv[1];"
        "const first=readContainedSource(root,'first.html',{relativeOnly:true});"
        "const second=readContainedSource(root,'second.html',{relativeOnly:true});"
        "try{replaceContainedSources([{snapshot:first,content:'changed-first'},"
        "{snapshot:second,content:'changed-second'}],"
        "{beforeReplace({index}){if(index===1)fs.linkSync(second.path,path.join(root,'late-link.html'))}});}"
        "catch(error){process.stdout.write(JSON.stringify({code:error.code}));process.exit(7)}",
    )

    assert result.returncode == 7
    assert json.loads(result.stdout)["code"] == "source_replace_failed"
    assert first.read_text() == "first"
    assert second.read_text() == "second"
    assert (tmp_path / "late-link.html").read_text() == "second"


def test_contained_source_success_preserves_inode_owner_mode_and_xattrs(tmp_path: Path) -> None:
    source = tmp_path / "page.html"
    source.write_text("original")
    source.chmod(0o664)
    xattr_name = "user.impeccable-test"
    xattr_supported = False
    if hasattr(os, "setxattr"):
        try:
            os.setxattr(source, xattr_name, b"kept")
            xattr_supported = True
        except OSError:
            pass
    before = source.stat()
    result = _run_contained_source_probe(
        tmp_path,
        "const root=process.argv[1];"
        "const snapshot=readContainedSource(root,'page.html',{relativeOnly:true});"
        "replaceContainedSources([{snapshot,content:'replacement'}]);",
    )

    assert result.returncode == 0, result.stderr
    after = source.stat()
    assert source.read_text() == "replacement"
    assert after.st_ino == before.st_ino
    assert after.st_uid == before.st_uid
    assert after.st_gid == before.st_gid
    assert after.st_mode & 0o7777 == before.st_mode & 0o7777
    if xattr_supported:
        assert os.getxattr(source, xattr_name) == b"kept"


def test_live_wrap_does_not_execute_shell_syntax_from_project_filenames(
    tmp_path: Path,
) -> None:
    marker = tmp_path / "SHELL_INJECTION_MARKER"
    page = tmp_path / "$(touch SHELL_INJECTION_MARKER).html"
    page.write_text('<html><body><section class="target">safe</section></body></html>\n')

    result = subprocess.run(
        [
            "node",
            str(SCRIPTS / "live-wrap.mjs"),
            "--id",
            "security-test",
            "--count",
            "1",
            "--query",
            "target",
        ],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert payload["file"] == page.name
    assert payload["startLine"] >= 1
    assert payload["endLine"] >= payload["startLine"]
    assert "impeccable-variants-start security-test" in page.read_text()
    assert not marker.exists()


def test_live_inject_insert_after_preserves_the_first_post_anchor_byte(
    tmp_path: Path,
) -> None:
    page = tmp_path / "index.html"
    page.write_text("<html><head><meta charset=\"utf-8\"></head></html>\n")
    config_dir = tmp_path / ".impeccable" / "live"
    config_dir.mkdir(parents=True)
    (config_dir / "config.json").write_text(
        json.dumps(
            {
                "files": [page.name],
                "insertAfter": "<head>",
                "commentSyntax": "html",
            }
        )
    )

    result = _run_inject(tmp_path, "--port", "43117", "--token", TOKEN)

    assert result.returncode == 0, result.stderr
    assert '<meta charset="utf-8">' in page.read_text()


def test_failed_accept_result_remains_recoverable_as_an_error(tmp_path: Path) -> None:
    result = _run_completion_type_probe(
        tmp_path,
        "accept",
        {"handled": False, "error": "session_structure_invalid"},
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == "error"


def test_live_poll_replies_echo_the_exact_lease_token(tmp_path: Path) -> None:
    completed = _run_poll_reply_payload_probe(tmp_path)

    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout) == {
        "token": "agent-secret",
        "id": "deadbeef",
        "type": "done",
        "leaseToken": "lease-secret",
    }


def test_agent_error_keeps_the_pending_accept_event_recoverable(tmp_path: Path) -> None:
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        "store.appendEvent({id:'security-test',type:'accept',variantId:'1'});"
        "store.appendEvent({id:'security-test',type:'agent_error',message:'not handled'});"
        "process.stdout.write(JSON.stringify(store.getSnapshot('security-test')));"
    )
    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    snapshot = json.loads(result.stdout)
    assert snapshot["phase"] == "agent_error"
    assert snapshot["pendingEvent"]["type"] == "accept"
    assert snapshot["pendingEvent"]["variantId"] == "1"


def test_restored_generation_requires_an_explicit_browser_retry_event(
    tmp_path: Path,
) -> None:
    session_url = (SCRIPTS / "live-browser-session.js").as_uri()
    script = (
        f"await import({json.dumps(session_url)});"
        "const api=globalThis.__IMPECCABLE_LIVE_SESSION__;"
        "const event=api.buildRetryGenerationEvent({"
        "id:'deadbeef',"
        "intent:{action:'polish',count:3,pageUrl:'/work',"
        "element:{outerHTML:'<main>safe</main>'},"
        "freeformPrompt:'Keep the hierarchy',"
        "comments:[{x:12,y:18,text:'Align this'}],"
        "strokes:[{points:[[1,2],[3,4]]}],screenshotPath:'/tmp/stale.png'}});"
        "process.stdout.write(JSON.stringify(event));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == {
        "type": "generate",
        "id": "deadbeef",
        "action": "polish",
        "freeformPrompt": "Keep the hierarchy",
        "count": 3,
        "pageUrl": "/work",
        "element": {"outerHTML": "<main>safe</main>"},
        "comments": [{"x": 12, "y": 18, "text": "Align this"}],
        "strokes": [{"points": [[1, 2], [3, 4]]}],
    }


def test_design_panel_tab_keys_choose_the_next_roving_tab(tmp_path: Path) -> None:
    session_url = (SCRIPTS / "live-browser-session.js").as_uri()
    cases = [
        ["ArrowRight", 0, 2],
        ["ArrowRight", 1, 2],
        ["ArrowLeft", 0, 2],
        ["ArrowLeft", 1, 2],
        ["Home", 1, 2],
        ["End", 0, 2],
        ["Enter", 0, 2],
    ]
    script = (
        f"await import({json.dumps(session_url)});"
        "const api=globalThis.__IMPECCABLE_LIVE_SESSION__;"
        f"const cases={json.dumps(cases)};"
        "process.stdout.write(JSON.stringify(cases.map((args) => "
        "api.nextRovingTabIndex(...args))));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == [1, 0, 1, 0, 0, 1, None]


def test_design_markdown_links_allow_only_safe_targets(tmp_path: Path) -> None:
    session_url = (SCRIPTS / "live-browser-session.js").as_uri()
    hrefs = [
        "https://example.test/docs",
        "http://example.test",
        "mailto:hello@example.test",
        "tel:+61700000000",
        "/docs/setup",
        "../design.md",
        "#tokens",
        "javascript:alert(1)",
        "java\tscript:alert(1)",
        "data:text/html,<script>alert(1)</script>",
        "file:///tmp/private",
    ]
    script = (
        f"await import({json.dumps(session_url)});"
        "const api=globalThis.__IMPECCABLE_LIVE_SESSION__;"
        f"const hrefs={json.dumps(hrefs)};"
        "process.stdout.write(JSON.stringify(hrefs.map(api.safeMarkdownHref)));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == [
        *hrefs[:7],
        None,
        None,
        None,
        None,
    ]


def test_status_summarises_retained_project_journals_at_the_server_boundary(
    tmp_path: Path,
) -> None:
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    seeded_prompt = "PROJECT_JOURNAL_PROMPT_MUST_NOT_ESCAPE"
    seeded_markup = "<main>PROJECT_JOURNAL_MARKUP_MUST_NOT_ESCAPE</main>"
    script = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "createLiveSessionStore({cwd:process.argv[1]}).appendEvent({"
        "id:'deadbeef',type:'generate',action:'polish',count:1,"
        f"freeformPrompt:{json.dumps(seeded_prompt)},"
        f"element:{{outerHTML:{json.dumps(seeded_markup)}}}}});"
    )
    seeded = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert seeded.returncode == 0, seeded.stderr

    with LiveServer(tmp_path) as server:
        status, _, body = _request(
            f"{server.base_url}/status?token={quote(server.token)}"
        )

    assert status == 200
    assert seeded_prompt not in body
    assert seeded_markup not in body
    payload = json.loads(body)
    assert payload["activeSessions"] == [
        {
            "id": "deadbeef",
            "phase": "generate_requested",
            "revision": 0,
            "hasPendingEvent": True,
            "pendingEventType": "generate",
        }
    ]


@pytest.mark.parametrize("variant_id", ["0", "9", "999"])
def test_live_server_rejects_out_of_range_accept_before_journal_or_queue(
    tmp_path: Path, variant_id: str,
) -> None:
    with LiveServer(tmp_path) as server:
        status, _, body = _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(
                {
                    "token": server.token,
                    "type": "accept",
                    "id": "deadbeef",
                    "variantId": variant_id,
                }
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert status == 400, body

        status, _, body = _request(
            f"{server.base_url}/status?token={quote(server.token)}"
        )
        assert status == 200
        payload = json.loads(body)
        assert payload["pendingEvents"] == []
        assert payload["activeSessions"] == []
        assert not (
            tmp_path / ".impeccable" / "live" / "sessions" / "deadbeef.jsonl"
        ).exists()


def test_live_mutation_help_discloses_project_relative_existing_file_boundary(
    tmp_path: Path,
) -> None:
    wrap = _run_wrap(tmp_path, "--help")
    inject = _run_inject(tmp_path, "--help")
    assert wrap.returncode == 0
    assert inject.returncode == 0
    for output in (wrap.stdout, inject.stdout):
        lowered = output.lower()
        assert "project-relative" in lowered
        assert "existing" in lowered
        assert "regular file" in lowered
    assert wrap.stdout.startswith("Usage: node live-wrap.mjs")

    live_help = subprocess.run(
        ["node", str(SCRIPTS / "live.mjs"), "--help"],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )
    assert live_help.returncode == 0
    config_contract = live_help.stdout.split("On config_missing, prints:", 1)[1].split(
        "The agent should then:", 1
    )[0]
    assert "path" in config_contract
    assert "configPath" not in config_contract
    assert "hint" not in config_contract


@pytest.mark.parametrize("path_kind", ["absolute", "traversal", "symlink"])
def test_live_wrap_rejects_targets_outside_project_before_any_write(
    tmp_path: Path, path_kind: str
) -> None:
    outside = tmp_path.parent / f"{tmp_path.name}-{path_kind}-outside.html"
    original = '<html><body><section class="target">outside</section></body></html>\n'
    outside.write_text(original)

    if path_kind == "absolute":
        target = str(outside)
    elif path_kind == "traversal":
        target = f"../{outside.name}"
    else:
        link = tmp_path / "escape.html"
        link.symlink_to(outside)
        target = link.name

    result = _run_wrap(tmp_path, "--file", target)

    assert result.returncode != 0
    assert json.loads(result.stderr)["error"] == "source_path_outside_project"
    assert outside.read_text() == original


@pytest.mark.parametrize("explicit", [True, False])
def test_live_wrap_rejects_hard_linked_source_before_any_write(
    tmp_path: Path, explicit: bool
) -> None:
    outside = tmp_path.parent / f"{tmp_path.name}-wrap-hardlink.html"
    original = '<html><body><section class="target">outside</section></body></html>\n'
    outside.write_text(original)
    inside = tmp_path / "index.html"
    os.link(outside, inside)

    result = _run_wrap(tmp_path, *(["--file", "index.html"] if explicit else []))

    assert result.returncode != 0
    assert outside.read_text() == original
    assert inside.read_text() == original


@pytest.mark.parametrize("mode", ["insert", "remove"])
def test_live_inject_rejects_hard_linked_source_for_every_mutation_mode(
    tmp_path: Path, mode: str
) -> None:
    page = tmp_path / "index.html"
    original = "<html><body>safe</body></html>\n"
    page.write_text(original)
    _write_config(tmp_path, ["index.html"])
    if mode == "remove":
        inserted = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)
        assert inserted.returncode == 0, inserted.stderr
    outside = tmp_path.parent / f"{tmp_path.name}-inject-{mode}-hardlink.html"
    os.link(page, outside)
    outside_before = outside.read_bytes()

    args = ["--remove"] if mode == "remove" else ["--port", "8400", "--token", TOKEN]
    result = _run_inject(tmp_path, *args)

    assert result.returncode != 0
    assert outside.read_bytes() == outside_before
    assert page.read_bytes() == outside_before


@pytest.mark.parametrize("mode", ["accept", "discard"])
def test_live_accept_rejects_hard_linked_source_for_every_mutation_mode(
    tmp_path: Path, mode: str
) -> None:
    page = _write_wrapped_session(tmp_path, with_variant=mode == "accept")
    outside = tmp_path.parent / f"{tmp_path.name}-accept-{mode}-hardlink.html"
    os.link(page, outside)
    outside_before = outside.read_bytes()

    args = ["--variant", "1"] if mode == "accept" else ["--discard"]
    result = _run_accept(tmp_path, *args)

    assert result.returncode != 0
    assert outside.read_bytes() == outside_before
    assert page.read_bytes() == outside_before


def test_live_entrypoint_does_not_execute_shell_syntax_from_server_state(
    tmp_path: Path,
) -> None:
    marker = tmp_path / "SHELL_INJECTION_MARKER"
    page = tmp_path / "index.html"
    page.write_text("<html><body>safe</body></html>\n")
    _write_config(tmp_path, ["index.html"])
    server_path = tmp_path / ".impeccable" / "live" / "server.json"
    server_path.write_text(
        json.dumps(
            {
                "pid": os.getpid(),
                "port": 8400,
                "token": "$(touch SHELL_INJECTION_MARKER)",
            }
        )
    )

    result = subprocess.run(
        ["node", str(SCRIPTS / "live.mjs")],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode != 0
    payload = json.loads(result.stdout)
    assert payload["error"] == "server_start_failed"
    assert payload["diagnostic"]["classification"] == "unrelated_pid_record"
    assert json.loads(server_path.read_text())["pid"] == os.getpid()
    assert not marker.exists()


def test_live_server_record_is_atomically_replaced_with_private_permissions(
    tmp_path: Path,
) -> None:
    live_dir = tmp_path / ".impeccable" / "live"
    live_dir.mkdir(parents=True)
    server_path = live_dir / "server.json"
    server_path.write_text(json.dumps({"pid": 1, "port": 1, "token": "old"}))
    server_path.chmod(0o644)
    original_inode = server_path.stat().st_ino
    module_url = (SCRIPTS / "impeccable-paths.mjs").as_uri()
    script = (
        f"import {{ writeLiveServerInfo }} from {json.dumps(module_url)};"
        "writeLiveServerInfo(process.argv[1], {pid: 2, port: 8400, token: 'new'});"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(server_path.read_text()) == {"pid": 2, "port": 8400, "token": "new"}
    assert not list(live_dir.glob(".server.json.*.tmp"))
    if os.name != "nt":
        assert server_path.stat().st_mode & 0o777 == 0o600
        assert server_path.stat().st_ino != original_inode


@pytest.mark.parametrize("escaping_path", ["../outside.html", "/tmp/outside.html"])
def test_live_inject_rejects_ancestor_and_absolute_targets_before_any_write(
    tmp_path: Path, escaping_path: str
) -> None:
    safe = tmp_path / "safe.html"
    original = "<html><body>safe</body></html>\n"
    safe.write_text(original)
    _write_config(tmp_path, ["safe.html", escaping_path])

    result = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert result.returncode != 0
    assert safe.read_text() == original
    assert "impeccable-live-start" not in safe.read_text()


def test_live_inject_rejects_symlink_escape_before_any_write(tmp_path: Path) -> None:
    safe = tmp_path / "safe.html"
    original = "<html><body>safe</body></html>\n"
    safe.write_text(original)
    outside = tmp_path.parent / f"{tmp_path.name}-outside.html"
    outside.write_text("<html><body>outside</body></html>\n")
    (tmp_path / "escape.html").symlink_to(outside)
    _write_config(tmp_path, ["safe.html", "escape.html"])

    result = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert result.returncode != 0
    assert safe.read_text() == original
    assert outside.read_text() == "<html><body>outside</body></html>\n"


def test_live_inject_preserves_authorised_insert_and_remove_workflow(tmp_path: Path) -> None:
    page = tmp_path / "index.html"
    original = "<html><body>safe</body></html>\n"
    page.write_text(original)
    _write_config(tmp_path, ["index.html"])

    inserted = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)
    assert inserted.returncode == 0, inserted.stderr
    assert f'http://127.0.0.1:8400/live.js?token={TOKEN}' in page.read_text()

    removed = _run_inject(tmp_path, "--remove")
    assert removed.returncode == 0, removed.stderr
    assert page.read_text() == original


def test_live_inject_remove_cleans_existing_targets_when_one_was_deleted(tmp_path: Path) -> None:
    first = tmp_path / "first.html"
    second = tmp_path / "second.html"
    original = "<html><body>safe</body></html>\n"
    first.write_text(original)
    second.write_text(original)
    _write_config(tmp_path, [first.name, second.name])

    inserted = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)
    assert inserted.returncode == 0, inserted.stderr
    second.unlink()

    removed = _run_inject(tmp_path, "--remove")

    assert removed.returncode == 0, removed.stderr
    assert first.read_text() == original
    assert json.loads(removed.stdout)["results"][1] == {
        "file": second.name,
        "removed": False,
        "note": "file missing",
    }


@pytest.mark.parametrize(
    ("filename", "source", "anchor"),
    [
        (
            "App.tsx",
            "const demo = '<head>';\nexport default () => <html><head><title>Real</title></head></html>;\n",
            "<head>",
        ),
        (
            "Component.svelte",
            "{condition ? `<main>` : ''}\n<main>Real</main>\n",
            "<main>",
        ),
        (
            "Page.astro",
            "---\nconst demo = `<head>`;\n---\n<html><head><title>Real</title></head></html>\n",
            "<head>",
        ),
    ],
)
def test_live_inject_ignores_framework_anchor_decoys(
    tmp_path: Path, filename: str, source: str, anchor: str
) -> None:
    page = tmp_path / filename
    page.write_text(source)
    config_dir = tmp_path / ".impeccable" / "live"
    config_dir.mkdir(parents=True)
    (config_dir / "config.json").write_text(
        json.dumps(
            {
                "files": [filename],
                "insertAfter": anchor,
                "commentSyntax": "jsx" if filename.endswith(".tsx") else "html",
            }
        )
    )

    inserted = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert inserted.returncode == 0, inserted.stderr
    result = page.read_text()
    assert result.count("impeccable-live-start") == 1
    assert source.splitlines()[0] in result
    if filename.endswith(".astro"):
        assert "const demo = `<head>`;" in result


def test_live_inject_rejects_anchor_found_only_in_jsx_string(tmp_path: Path) -> None:
    page = tmp_path / "App.tsx"
    source = "const demo = '<head>';\nexport default () => <main>Real</main>;\n"
    page.write_text(source)
    config_dir = tmp_path / ".impeccable" / "live"
    config_dir.mkdir(parents=True)
    (config_dir / "config.json").write_text(
        json.dumps(
            {
                "files": [page.name],
                "insertAfter": "<head>",
                "commentSyntax": "jsx",
            }
        )
    )

    inserted = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert inserted.returncode != 0
    assert json.loads(inserted.stderr)["error"] == "mutation_preflight_failed"
    assert page.read_text() == source


@pytest.mark.parametrize(
    ("filename", "source", "decoy"),
    [
        (
            "App.tsx",
            "const demo = '<meta http-equiv=\"Content-Security-Policy\" content=\"default-src none\">';\n"
            "export default () => <html><head><meta httpEquiv=\"Content-Security-Policy\" content=\"default-src self\" /></head></html>;\n",
            "default-src none",
        ),
        (
            "index.html",
            "<html><head><script>const demo = '<meta http-equiv=\"Content-Security-Policy\" content=\"default-src none\">';</script>"
            "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src self\"></head><body></body></html>\n",
            "default-src none",
        ),
    ],
)
def test_live_inject_patches_only_executable_csp_meta(
    tmp_path: Path, filename: str, source: str, decoy: str
) -> None:
    page = tmp_path / filename
    page.write_text(source)
    config_dir = tmp_path / ".impeccable" / "live"
    config_dir.mkdir(parents=True)
    (config_dir / "config.json").write_text(
        json.dumps(
            {
                "files": [filename],
                "insertAfter": "<head>",
                "commentSyntax": "jsx" if filename.endswith(".tsx") else "html",
            }
        )
    )

    inserted = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert inserted.returncode == 0, inserted.stderr
    result = page.read_text()
    decoy_fragment = result.split(decoy, 1)[0]
    assert "data-impeccable-csp-original" not in decoy_fragment
    assert result.count("data-impeccable-csp-original") == 1


def test_live_inject_seeds_missing_directives_from_default_src(
    tmp_path: Path,
) -> None:
    module_url = INJECT.as_uri()
    policy = "default-src https: data:; style-src 'self'; script-src https://scripts.example"
    source = f'<meta http-equiv="Content-Security-Policy" content="{policy}">'
    script = (
        f"import {{ patchCspMeta }} from {json.dumps(module_url)};"
        f"process.stdout.write(patchCspMeta({json.dumps(source)},8400));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    patched = result.stdout
    assert "style-src 'self'" in patched
    assert "script-src https://scripts.example http://127.0.0.1:8400" in patched
    assert "connect-src https: data: http://127.0.0.1:8400" in patched
    assert "img-src https: data: blob:" in patched


def test_live_inject_csp_attributes_are_exact_and_round_trip(tmp_path: Path) -> None:
    module_url = INJECT.as_uri()
    decoy = '<meta data-http-equiv="Content-Security-Policy" content="default-src \'none\'">'
    source = (
        decoy
        + '<meta http-equiv="Content-Security-Policy" '
        + 'data-content="default-src \'none\'" data-note="content=\'decoy\'" '
        + 'content="default-src \'none\'">'
    )
    script = (
        f"import {{ patchCspMeta,revertCspMeta }} from {json.dumps(module_url)};"
        f"const source={json.dumps(source)};"
        "const patched=patchCspMeta(source,8400);"
        "process.stdout.write(JSON.stringify({patched,reverted:revertCspMeta(patched)}));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert payload["patched"].startswith(decoy)
    assert 'data-content="default-src \'none\'"' in payload["patched"]
    assert "script-src http://127.0.0.1:8400" in payload["patched"]
    assert payload["reverted"] == source


def test_live_inject_patches_the_effective_script_element_directive(tmp_path: Path) -> None:
    module_url = INJECT.as_uri()
    policy = "default-src 'none'; script-src https://workers.example; script-src-elem 'none'"
    source = f'<meta http-equiv="Content-Security-Policy" content="{policy}">'
    script = (
        f"import {{ patchCspMeta }} from {json.dumps(module_url)};"
        f"process.stdout.write(patchCspMeta({json.dumps(source)},8400));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert "script-src https://workers.example;" in result.stdout
    assert "script-src-elem http://127.0.0.1:8400" in result.stdout


def test_live_inject_strict_dynamic_fails_before_any_source_write(tmp_path: Path) -> None:
    safe = tmp_path / "safe.html"
    strict = tmp_path / "strict.html"
    safe_source = "<html><body>safe</body></html>\n"
    strict_source = (
        '<html><head><meta http-equiv="Content-Security-Policy" '
        'content="default-src none; script-src \'strict-dynamic\' \'nonce-live\'">'
        "</head><body>strict</body></html>\n"
    )
    safe.write_text(safe_source)
    strict.write_text(strict_source)
    _write_config(tmp_path, [safe.name, strict.name])

    result = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert result.returncode != 0
    assert json.loads(result.stderr)["error"] == "csp_strict_dynamic_unsupported"
    assert safe.read_text() == safe_source
    assert strict.read_text() == strict_source


def test_live_inject_remove_ignores_template_marker_decoy_before_real_block(
    tmp_path: Path,
) -> None:
    page = tmp_path / "App.tsx"
    config_dir = tmp_path / ".impeccable" / "live"
    config_dir.mkdir(parents=True)
    (config_dir / "config.json").write_text(
        json.dumps(
            {
                "files": [page.name],
                "insertAfter": "<main>",
                "commentSyntax": "jsx",
            }
        )
    )
    block = "\n".join(
        [
            "{/* impeccable-live-start */}",
            f'<script src="http://127.0.0.1:8400/live.js?token={TOKEN}"></script>',
            "{/* impeccable-live-end */}",
            "",
        ]
    )
    decoy = "const fixture = `\n" + block + "`;\n"
    page.write_text(decoy + block + "export const App = () => <main>safe</main>;\n")

    removed = _run_inject(tmp_path, "--remove")

    assert removed.returncode == 0, removed.stderr
    assert page.read_text().startswith(decoy)
    assert page.read_text().count("impeccable-live-start") == 1
    assert "export const App" in page.read_text()


def test_live_inject_remove_fails_closed_on_multiple_executable_blocks(
    tmp_path: Path,
) -> None:
    page = tmp_path / "index.html"
    _write_config(tmp_path, [page.name])
    block = "\n".join(
        [
            "<!-- impeccable-live-start -->",
            f'<script src="http://127.0.0.1:8400/live.js?token={TOKEN}"></script>',
            "<!-- impeccable-live-end -->",
            "",
        ]
    )
    source = block + "<main>safe</main>\n" + block
    page.write_text(source)

    removed = _run_inject(tmp_path, "--remove")

    assert removed.returncode != 0
    assert json.loads(removed.stderr)["error"] == "live_marker_ambiguous"
    assert page.read_text() == source


def test_live_inject_remove_handles_stale_syntax_and_html_comment_decoys(
    tmp_path: Path,
) -> None:
    page = tmp_path / "index.html"
    config_dir = tmp_path / ".impeccable" / "live"
    config_dir.mkdir(parents=True)
    (config_dir / "config.json").write_text(
        json.dumps(
            {
                "files": [page.name],
                "insertBefore": "</body>",
                "commentSyntax": "jsx",
            }
        )
    )
    block = "\n".join(
        [
            "<!-- impeccable-live-start -->",
            f'<script src="http://127.0.0.1:8400/live.js?token={TOKEN}"></script>',
            "<!-- impeccable-live-end -->",
            "",
        ]
    )
    prefix = "<!-- docs mention <script but do not open raw text -->\n"
    page.write_text(prefix + block + "<main>safe</main>\n")

    removed = _run_inject(tmp_path, "--remove")

    assert removed.returncode == 0, removed.stderr
    assert page.read_text() == prefix + "<main>safe</main>\n"


def test_live_inject_remove_ignores_framework_expression_template_decoy(
    tmp_path: Path,
) -> None:
    page = tmp_path / "Component.svelte"
    _write_config(tmp_path, [page.name])
    block = "\n".join(
        [
            "<!-- impeccable-live-start -->",
            f'<script src="http://127.0.0.1:8400/live.js?token={TOKEN}"></script>',
            "<!-- impeccable-live-end -->",
            "",
        ]
    )
    source = "{condition ? `\n" + block + "` : ''}\n<section>Real</section>\n"
    page.write_text(source)

    removed = _run_inject(tmp_path, "--remove")

    assert removed.returncode == 0, removed.stderr
    assert page.read_text() == source


def test_live_runtime_http_urls_use_one_ipv4_loopback_origin() -> None:
    runtime_files = [
        path
        for path in SCRIPTS.glob("live*")
        if path.is_file() and path.suffix in {".js", ".mjs"}
    ]
    for runtime_file in runtime_files:
        text = runtime_file.read_text()
        assert "http://localhost" not in text, runtime_file.name


def test_live_inject_hard_excludes_explicit_dependency_and_git_targets(
    tmp_path: Path,
) -> None:
    safe = tmp_path / "index.html"
    dependency = tmp_path / "node_modules" / "pkg" / "index.html"
    git_page = tmp_path / ".git" / "index.html"
    dependency.parent.mkdir(parents=True)
    git_page.parent.mkdir(parents=True)
    for page in (safe, dependency, git_page):
        page.write_text("<html><body>safe</body></html>\n")
    _write_config(
        tmp_path,
        ["index.html", "node_modules/pkg/index.html", ".git/index.html"],
    )

    result = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert result.returncode == 0, result.stderr
    assert "impeccable-live-start" in safe.read_text()
    assert "impeccable-live-start" not in dependency.read_text()
    assert "impeccable-live-start" not in git_page.read_text()


@pytest.mark.parametrize(
    "files",
    [
        ["src/**/*.html"],
        ["node_modules/pkg/index.html", ".git/index.html"],
    ],
)
def test_live_inject_fails_when_config_resolves_to_no_targets(
    tmp_path: Path, files: list[str]
) -> None:
    dependency = tmp_path / "node_modules" / "pkg" / "index.html"
    git_page = tmp_path / ".git" / "index.html"
    dependency.parent.mkdir(parents=True)
    git_page.parent.mkdir(parents=True)
    dependency.write_text("<html><body>dependency</body></html>\n")
    git_page.write_text("<html><body>git</body></html>\n")
    _write_config(tmp_path, files)

    result = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert result.returncode != 0
    assert json.loads(result.stderr)["error"] == "config_no_targets"
    assert "impeccable-live-start" not in dependency.read_text()
    assert "impeccable-live-start" not in git_page.read_text()


def test_csp_revert_treats_the_generated_marker_as_literal_text(tmp_path: Path) -> None:
    module_url = INJECT.as_uri()
    # This exact policy encodes to base64 containing '+', which is a regexp
    # quantifier and exposed the old dynamic-RegExp removal path.
    policy = "default-src 'self'; report-uri /?x=~"
    assert "+" in base64.b64encode(policy.encode()).decode()
    source = f'<meta http-equiv="Content-Security-Policy" content="{policy}">'
    script = (
        f"import {{ patchCspMeta, revertCspMeta }} from {json.dumps(module_url)};"
        f"const source={json.dumps(source)};"
        "const patched=patchCspMeta(source,8400);"
        "const reverted=revertCspMeta(patched);"
        "process.stdout.write(JSON.stringify({patched,reverted}));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert "data-impeccable-csp-original" in payload["patched"]
    assert payload["reverted"] == source


@pytest.mark.parametrize("marker", ["%%%", "YQ", "/w=="])
def test_live_inject_rejects_malformed_csp_markers_before_any_write(
    tmp_path: Path, marker: str
) -> None:
    good = tmp_path / "good.html"
    bad = tmp_path / "bad.html"
    good.write_text("<html><body>good</body></html>\n")
    bad.write_text(
        '<html><head><meta http-equiv="Content-Security-Policy" '
        f'content="default-src patched" data-impeccable-csp-original="{marker}">'
        "</head><body>bad</body></html>\n"
    )
    _write_config(tmp_path, ["good.html", "bad.html"])
    before = {path: path.read_bytes() for path in (good, bad)}

    result = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert result.returncode != 0
    assert json.loads(result.stderr)["error"] == "csp_marker_invalid"
    assert {path: path.read_bytes() for path in (good, bad)} == before


def test_live_wrap_detects_a_multiline_self_closing_jsx_opener(tmp_path: Path) -> None:
    module_url = WRAP.as_uri()
    script = (
        f"import {{ findClosingLine }} from {json.dumps(module_url)};"
        "const lines=['<Card','  className=\"target\"','/>','<p>after</p>'];"
        "process.stdout.write(String(findClosingLine(lines,0)));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == "2"


def test_live_wrap_scans_nested_multiline_jsx_without_treating_strings_as_tags(
    tmp_path: Path,
) -> None:
    module_url = WRAP.as_uri()
    lines = [
        "<Card",
        '  render={() => ({ label: ">" })}',
        '  preview={<Preview title={"prop > value"} />}',
        ">",
        '  {"</Card>"}',
        "  <Card",
        '    title="nested > value"',
        "  >nested</Card>",
        "</Card>",
        "<p>after</p>",
    ]
    script = (
        f"import {{ findClosingLine }} from {json.dumps(module_url)};"
        f"const lines={json.dumps(lines)};"
        "process.stdout.write(String(findClosingLine(lines,0)));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == "8"


def test_live_wrap_ignores_module_level_jsx_decoys_before_the_real_target(
    tmp_path: Path,
) -> None:
    page = tmp_path / "App.tsx"
    source = "\n".join(
        [
            "const typed = identity<string>(value);",
            "type Fn = <T>(value: T) => T;",
            "const quoted = '<Card className=\"target\">quoted</Card>';",
            "const templated = html`<Card className=\"target\">template</Card>`;",
            "// <Card className=\"target\">line comment</Card>",
            "/* <Card className=\"target\">block comment</Card> */",
            "export const App = () => (",
            "  <main>",
            '    <p>It\'s "ready"</p>',
            "    <Card className=\"target\">Real</Card>",
            "  </main>",
            ");",
            "",
        ]
    )
    page.write_text(source)

    wrapped = _run_wrap(tmp_path, "--file", page.name, "--tag", "Card")

    assert wrapped.returncode == 0, wrapped.stderr
    result = page.read_text()
    assert result.startswith(source.splitlines()[0] + "\n" + source.splitlines()[1] + "\n")
    assert "quoted</Card>';" in result
    assert "template</Card>`;" in result
    assert "data-impeccable-variant=\"original\"" in result
    assert "Real</Card>" in result


@pytest.mark.parametrize(
    ("filename", "prefix"),
    [
        (
            "Component.svelte",
            "{condition ? '<section class=\"target\">Example</section>' : ''}\n",
        ),
        (
            "Page.astro",
            "---\nconst docs = '<section class=\"target\">Example</section>';\n---\n",
        ),
        (
            "Multiline.svelte",
            "{condition ? `\n<section class=\"target\">Example</section>\n` : ''}\n",
        ),
        (
            "Multiline.astro",
            "---\nconst docs = `\n<section class=\"target\">Example</section>\n`;\n---\n",
        ),
        (
            "Shift.svelte",
            "<p>It's ready</p>\n{condition ? `'\n<section class=\"target\">Example</section>\n` : ''}\n",
        ),
        (
            "Contraction.svelte",
            "<p>It's ready</p>\n",
        ),
    ],
)
def test_live_wrap_ignores_framework_script_expression_decoys(
    tmp_path: Path, filename: str, prefix: str
) -> None:
    page = tmp_path / filename
    page.write_text(prefix + '<section class="target">Real</section>\n')

    wrapped = _run_wrap(tmp_path, "--file", page.name, "--tag", "section")

    assert wrapped.returncode == 0, wrapped.stderr
    result = page.read_text()
    assert result.startswith(prefix)
    if "Example</section>" in prefix:
        assert "Example</section>" in result
    assert "data-impeccable-variant=\"original\"" in result
    assert "Real</section>" in result


def test_live_wrap_ignores_html_raw_text_decoys_before_the_real_target(
    tmp_path: Path,
) -> None:
    page = tmp_path / "index.html"
    source = "\n".join(
        [
            '<script>const docs = \'<section class="target">Example</section>\';</script>',
            '<!-- <section class="target">comment</section> -->',
            '<section class="target">Real</section>',
            "",
        ]
    )
    page.write_text(source)

    wrapped = _run_wrap(tmp_path, "--file", page.name, "--tag", "section")

    assert wrapped.returncode == 0, wrapped.stderr
    result = page.read_text()
    assert result.startswith(source.splitlines()[0] + "\n")
    assert "Example</section>" in result
    assert "data-impeccable-variant=\"original\"" in result
    assert "Real</section>" in result


@pytest.mark.parametrize(
    ("lines", "start", "closing"),
    [
        (
            [
                "function Cards() {",
                "  return (",
                "    <section>",
                '      <div className="target">Target</div>',
                "    </section>",
                "  );",
                "}",
            ],
            3,
            3,
        ),
        (
            [
                "{items.map((item) => (",
                '  <div className="target">',
                "    {item.label}",
                "  </div>",
                "))}",
            ],
            1,
            3,
        ),
        (
            ['{condition && <Card className="target" />}'],
            0,
            0,
        ),
        (
            ['{items.map((item) => <Card className="target">{item.label}</Card>)}'],
            0,
            0,
        ),
        (
            ['<Card className="target">{identity<string>(value)}</Card>'],
            0,
            0,
        ),
        (
            [
                '<Card className="target">{render<Button>(value)}'
                '<Button>Save</Button></Card>'
            ],
            0,
            0,
        ),
        (
            ['<Card className="target">{items.map(<T,>(value: T) => value)}</Card>'],
            0,
            0,
        ),
        (
            ['<Card className="target">{value as Array<string>}</Card>'],
            0,
            0,
        ),
        (
            ['<Card>{condition && <Span>foo<Bar>bar</Bar></Span>}</Card>'],
            0,
            0,
        ),
        (
            ['<Card>{/}/.test(value) ? <A/> : <B/>}</Card>'],
            0,
            0,
        ),
        (
            ['<Card>{/<Broken>/.test(value) ? <A/> : <B/>}</Card>'],
            0,
            0,
        ),
        (
            ['<Card>{await /}/.test(value) ? <A/> : <B/>}</Card>'],
            0,
            0,
        ),
        (
            ['<Card>{typeof /}/ === "object" ? <A/> : <B/>}</Card>'],
            0,
            0,
        ),
        (
            ['<Card>{void /}/.test(value)}</Card>'],
            0,
            0,
        ),
        (
            ['<Card>{(() => { if (value) /}/.test(text); return <A/>; })()}</Card>'],
            0,
            0,
        ),
        (
            ['<Card>{(() => { while (value) /}/.test(text); return <A/>; })()}</Card>'],
            0,
            0,
        ),
        (
            ['<Card>{`outer ${`inner }`}`}</Card>'],
            0,
            0,
        ),
        (
            ['<Card>{`outer ${value ? `<A>` : `<B>`}`}</Card>'],
            0,
            0,
        ),
    ],
)
def test_live_wrap_stops_scanning_after_the_selected_jsx_subtree(
    tmp_path: Path, lines: list[str], start: int, closing: int
) -> None:
    module_url = WRAP.as_uri()
    script = (
        f"import {{ findClosingLine }} from {json.dumps(module_url)};"
        f"const lines={json.dumps(lines)};"
        f"process.stdout.write(String(findClosingLine(lines,{start})));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == str(closing)


@pytest.mark.parametrize("mode", ["accept", "discard"])
def test_live_wrap_and_completion_handle_a_target_inside_a_function_component(
    tmp_path: Path, mode: str
) -> None:
    page = tmp_path / "component.tsx"
    original = "\n".join(
        [
            "export function Cards() {",
            "  return (",
            "    <section>",
            '      <div className="target">original</div>',
            "    </section>",
            "  );",
            "}",
            "",
        ]
    )
    page.write_text(original)

    wrapped = _run_wrap(tmp_path, "--file", page.name)
    assert wrapped.returncode == 0, wrapped.stderr
    if mode == "accept":
        marker = "Variants: insert below this line"
        marker_line = next(line for line in page.read_text().splitlines() if marker in line)
        variant = (
            '\n      <div data-impeccable-variant="1">'
            '<div className="target">accepted</div></div>'
        )
        page.write_text(page.read_text().replace(marker_line, marker_line + variant))
        completed = _run_accept(tmp_path, "--variant", "1")
    else:
        completed = _run_accept(tmp_path, "--discard")

    assert completed.returncode == 0, completed.stderr
    result = page.read_text()
    assert "data-impeccable-variants" not in result
    assert result.endswith("  );\n}\n")
    if mode == "accept":
        assert "accepted" in result
    else:
        assert '<div className="target">original</div>' in result


def test_live_wrap_fails_closed_on_unbalanced_jsx(tmp_path: Path) -> None:
    module_url = WRAP.as_uri()
    script = (
        f"import {{ findClosingLine }} from {json.dumps(module_url)};"
        "try{findClosingLine(['<Card','  title={\"broken\"','>','</Card>'],0)}"
        "catch(error){process.stdout.write(error.code);process.exit(7)}"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 7
    assert result.stdout == "jsx_scan_unbalanced"


def test_live_wrap_does_not_stop_inside_an_unterminated_jsx_expression(
    tmp_path: Path,
) -> None:
    module_url = WRAP.as_uri()
    script = (
        f"import {{ findClosingLine }} from {json.dumps(module_url)};"
        "try{findClosingLine(['<Card>','  {value','</Card>','}'],0)}"
        "catch(error){process.stdout.write(error.code);process.exit(7)}"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 7
    assert result.stdout == "jsx_scan_unbalanced"


def test_live_wrap_requires_a_same_line_outer_jsx_expression_to_close(
    tmp_path: Path,
) -> None:
    module_url = WRAP.as_uri()
    script = (
        f"import {{ findClosingLine }} from {json.dumps(module_url)};"
        "try{findClosingLine(['{condition && <Card className=\"target\" />'],0)}"
        "catch(error){process.stdout.write(error.code);process.exit(7)}"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 7
    assert result.stdout == "jsx_scan_unbalanced"


@pytest.mark.parametrize(
    "source",
    [
        '<section class="target"><img src="hero.png"><br><input></section>',
        '<ul class="target"><li>one<li>two</ul>',
        '<img class="target" src="hero.png">',
        '<section class="target"><!-- <section> placeholder --><p>ok</p></section>',
        (
            '<section class="target">\n<script>\nconst x = "</section>";\n'
            '</script>\n<p>ok</p>\n</section>'
        ),
    ],
)
def test_live_wrap_accepts_valid_html_void_and_optional_end_tags(
    tmp_path: Path, source: str
) -> None:
    page = tmp_path / "index.html"
    page.write_text(source + "\n")

    wrapped = _run_wrap(tmp_path, "--file", page.name)

    assert wrapped.returncode == 0, wrapped.stderr
    assert "data-impeccable-variants" in page.read_text()


def test_live_wrap_ignores_closing_tag_text_inside_html_raw_text_elements(
    tmp_path: Path,
) -> None:
    module_url = WRAP.as_uri()
    lines = [
        '<section class="target">',
        '<script>',
        'const x = "</section>";',
        '</script>',
        '<p>inside</p>',
        '</section>',
    ]
    script = (
        f"import {{ findClosingLine }} from {json.dumps(module_url)};"
        f"const lines={json.dumps(lines)};"
        "process.stdout.write(String(findClosingLine(lines,0,{isJsx:false})));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout == "5"


@pytest.mark.parametrize(
    "source",
    [
        '<ul>\n  <li class="target">one\n  <li>two\n</ul>\n',
        (
            '<ul>\n  <li class="target">outer\n    <ul>\n'
            '      <li>inner\n    </ul>\n  </li>\n</ul>\n'
        ),
    ],
)
def test_live_wrap_fails_closed_for_selected_html_optional_end_tags(
    tmp_path: Path, source: str
) -> None:
    page = tmp_path / "index.html"
    page.write_text(source)

    wrapped = _run_wrap(tmp_path, "--file", page.name)

    assert wrapped.returncode == 1
    assert page.read_text() == source
    assert json.loads(wrapped.stderr)["error"] == "html_implicit_end_unsupported"


@pytest.mark.parametrize(
    "source",
    [
        '<ul>\n<li class="target">one</li>\n</ul>\n',
        '<div>\n<p class="target">text</p>\n</div>\n',
        '<table><tr>\n<td class="target">cell</td>\n</tr></table>\n',
    ],
)
def test_live_wrap_accepts_explicitly_closed_html_optional_end_tags(
    tmp_path: Path, source: str
) -> None:
    page = tmp_path / "index.html"
    page.write_text(source)

    wrapped = _run_wrap(tmp_path, "--file", page.name)

    assert wrapped.returncode == 0, wrapped.stderr
    assert "data-impeccable-variants" in page.read_text()


@pytest.mark.parametrize(
    "lines",
    [
        ['<div className="target"><span>broken</div></span>'],
        ['<div className="target">broken</div foo>'],
        ['<Card>{condition && <Span></Broken>}</Card>'],
        ['<Card>{condition && <Span>}</Card>'],
    ],
)
def test_live_wrap_fails_closed_on_malformed_jsx_tag_structure(
    tmp_path: Path, lines: list[str]
) -> None:
    module_url = WRAP.as_uri()
    script = (
        f"import {{ findClosingLine }} from {json.dumps(module_url)};"
        f"const lines={json.dumps(lines)};"
        "try{findClosingLine(lines,0)}"
        "catch(error){process.stdout.write(error.code);process.exit(7)}"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 7
    assert result.stdout == "jsx_scan_unbalanced"


def test_live_discard_ignores_marker_shaped_text_outside_the_session(
    tmp_path: Path,
) -> None:
    page = tmp_path / "index.html"
    page.write_text('<main><p class="target">original</p></main>\n')
    wrapped = _run_wrap(tmp_path, "--file", page.name)
    assert wrapped.returncode == 0, wrapped.stderr
    marker = "<!-- impeccable-variants-start security-test -->"
    decoy = "<p>impeccable-variants-start security-test</p>"
    page.write_text(page.read_text().replace(marker, decoy + "\n" + marker))

    completed = _run_accept(tmp_path, "--discard")

    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout)["handled"] is True
    result = page.read_text()
    assert decoy in result
    assert '<p class="target">original</p>' in result


def test_live_wrap_and_discard_preserve_inline_jsx_prefix_and_suffix(
    tmp_path: Path,
) -> None:
    page = tmp_path / "Component.tsx"
    source = 'export const A = () => <Card className="target">Alpha</Card>;\n'
    page.write_text(source)

    wrapped = _run_wrap(
        tmp_path,
        "--file",
        page.name,
        "--classes",
        "target",
        "--tag",
        "Card",
    )

    assert wrapped.returncode == 0, wrapped.stderr
    assert page.read_text().startswith("export const A = () => <div")
    assert page.read_text().rstrip().endswith("</div>;")

    discarded = _run_accept(tmp_path, "--discard")

    assert discarded.returncode == 0, discarded.stderr
    assert json.loads(discarded.stdout)["handled"] is True
    assert page.read_text() == source


def test_live_wrap_uses_text_to_select_one_target_across_files(tmp_path: Path) -> None:
    source_dir = tmp_path / "src"
    source_dir.mkdir()
    first = source_dir / "A.tsx"
    second = source_dir / "B.tsx"
    first_source = '<Card className="target">Alpha unique</Card>\n'
    second_source = '<Card className="target">Beta unique</Card>\n'
    first.write_text(first_source)
    second.write_text(second_source)

    wrapped = _run_wrap(
        tmp_path,
        "--classes",
        "target",
        "--tag",
        "Card",
        "--text",
        "Beta unique",
    )

    assert wrapped.returncode == 0, wrapped.stderr
    assert first.read_text() == first_source
    assert 'data-impeccable-variants="security-test"' in second.read_text()


def test_live_wrap_uses_short_visible_text_without_extra_ceremony(tmp_path: Path) -> None:
    source_dir = tmp_path / "src"
    source_dir.mkdir()
    save = source_dir / "Save.tsx"
    next_page = source_dir / "Next.tsx"
    save_source = '<Button className="target">Save</Button>\n'
    next_source = '<Button className="target">Next</Button>\n'
    save.write_text(save_source)
    next_page.write_text(next_source)

    wrapped = _run_wrap(
        tmp_path,
        "--classes",
        "target",
        "--tag",
        "Button",
        "--text",
        "Save",
    )

    assert wrapped.returncode == 0, wrapped.stderr
    assert 'data-impeccable-variants="security-test"' in save.read_text()
    assert next_page.read_text() == next_source


@pytest.mark.parametrize("cross_file", [False, True])
def test_live_wrap_text_disambiguates_same_line_targets(
    tmp_path: Path, cross_file: bool
) -> None:
    source_dir = tmp_path / "src"
    source_dir.mkdir()
    page = source_dir / "Buttons.tsx"
    source = (
        '<Button className="target">Cancel</Button>'
        '<Button className="target">Save</Button>\n'
    )
    page.write_text(source)
    args = [
        "--classes",
        "target",
        "--tag",
        "Button",
        "--text",
        "Save",
    ]
    if not cross_file:
        args.extend(["--file", "src/Buttons.tsx"])
    else:
        other = source_dir / "Other.tsx"
        other.write_text('<Button className="target">Delete</Button>\n')

    wrapped = _run_wrap(tmp_path, *args)

    assert wrapped.returncode == 0, wrapped.stderr
    result = page.read_text()
    assert '<Button className="target">Cancel</Button><div' in result
    assert '<Button className="target">Save</Button>' in result


def test_live_wrap_fails_closed_for_multiple_same_line_targets_without_text(
    tmp_path: Path,
) -> None:
    page = tmp_path / "Component.tsx"
    source = (
        '<><Card className="target">Alpha</Card>'
        '<Card className="target">Beta</Card></>\n'
    )
    page.write_text(source)

    wrapped = _run_wrap(
        tmp_path,
        "--file",
        page.name,
        "--classes",
        "target",
        "--tag",
        "Card",
    )

    assert wrapped.returncode != 0
    assert page.read_text() == source


@pytest.mark.parametrize("cross_file", [False, True])
def test_live_wrap_fails_closed_for_ambiguous_no_text_targets(
    tmp_path: Path, cross_file: bool
) -> None:
    source_dir = tmp_path / "src"
    source_dir.mkdir()
    first = source_dir / "A.tsx"
    second = source_dir / "B.tsx"
    if cross_file:
        first_source = '<Card className="target">Alpha</Card>\n'
        second_source = '<Card className="target">Beta</Card>\n'
        first.write_text(first_source)
        second.write_text(second_source)
        args = ["--classes", "target", "--tag", "Card"]
    else:
        first_source = (
            '<Card className="target">Alpha</Card>\n'
            '<Card className="target">Beta</Card>\n'
        )
        second_source = ""
        first.write_text(first_source)
        args = ["--file", "src/A.tsx", "--classes", "target", "--tag", "Card"]

    wrapped = _run_wrap(tmp_path, *args)

    assert wrapped.returncode != 0
    assert first.read_text() == first_source
    if cross_file:
        assert second.read_text() == second_source


def test_live_wrap_selects_exact_same_line_opener_and_preserves_siblings(
    tmp_path: Path,
) -> None:
    page = tmp_path / "Component.tsx"
    source = (
        'export const App = () => <section><Card className="target">A</Card>'
        '<span>keep</span></section>;\n'
    )
    page.write_text(source)

    wrapped = _run_wrap(
        tmp_path,
        "--file",
        page.name,
        "--classes",
        "target",
        "--tag",
        "Card",
    )

    assert wrapped.returncode == 0, wrapped.stderr
    assert "<section><div" in page.read_text()
    assert "<span>keep</span></section>;" in page.read_text()
    discarded = _run_accept(tmp_path, "--discard")
    assert discarded.returncode == 0, discarded.stderr
    assert page.read_text() == source


def test_live_wrap_skips_malformed_cross_file_candidate(tmp_path: Path) -> None:
    source_dir = tmp_path / "src"
    source_dir.mkdir()
    malformed = source_dir / "A.tsx"
    valid = source_dir / "B.tsx"
    malformed_source = '<Card className="target">Broken\n'
    valid_source = '<Card className="target">Valid destination</Card>\n'
    malformed.write_text(malformed_source)
    valid.write_text(valid_source)

    wrapped = _run_wrap(
        tmp_path,
        "--classes",
        "target",
        "--tag",
        "Card",
        "--text",
        "Valid destination",
    )

    assert wrapped.returncode == 0, wrapped.stderr
    assert malformed.read_text() == malformed_source
    assert 'data-impeccable-variants="security-test"' in valid.read_text()


def test_live_wrap_preserves_unique_id_query_priority(tmp_path: Path) -> None:
    source_dir = tmp_path / "src"
    source_dir.mkdir()
    target = source_dir / "Target.tsx"
    sibling = source_dir / "Sibling.tsx"
    target.write_text('<Card id="hero" className="target">Same text words</Card>\n')
    sibling_source = '<Card className="target">Same text words</Card>\n'
    sibling.write_text(sibling_source)

    wrapped = _run_wrap(
        tmp_path,
        "--element-id",
        "hero",
        "--classes",
        "target",
        "--tag",
        "Card",
        "--text",
        "Same text words",
    )

    assert wrapped.returncode == 0, wrapped.stderr
    assert 'data-impeccable-variants="security-test"' in target.read_text()
    assert sibling.read_text() == sibling_source


def test_live_wrap_and_discard_preserve_crlf_bytes(tmp_path: Path) -> None:
    page = tmp_path / "Component.tsx"
    source = (
        b"export const App = () => (\r\n"
        b"  <Card className=\"target\">Alpha</Card>\r\n"
        b");\r\n"
    )
    page.write_bytes(source)

    wrapped = _run_wrap(tmp_path, "--file", page.name)

    assert wrapped.returncode == 0, wrapped.stderr
    wrapped_bytes = page.read_bytes()
    assert b"\n" not in wrapped_bytes.replace(b"\r\n", b"")
    discarded = _run_accept(tmp_path, "--discard")
    assert discarded.returncode == 0, discarded.stderr
    assert page.read_bytes() == source


def test_live_accept_preserves_inline_jsx_prefix_and_suffix(tmp_path: Path) -> None:
    page = tmp_path / "Component.tsx"
    page.write_text('export const A = () => <Card className="target">Alpha</Card>;\n')
    wrapped = _run_wrap(tmp_path, "--file", page.name)
    assert wrapped.returncode == 0, wrapped.stderr
    marker_line = next(
        line for line in page.read_text().splitlines()
        if "Variants: insert below this line" in line
    )
    variant = (
        marker_line
        + '\n  <div data-impeccable-variant="1">'
        + '<Card className="target">Beta</Card></div>'
    )
    page.write_text(page.read_text().replace(marker_line, variant))

    accepted = _run_accept(tmp_path, "--variant", "1")

    assert accepted.returncode == 0, accepted.stderr
    assert json.loads(accepted.stdout)["handled"] is True
    assert page.read_text() == 'export const A = () => <Card className="target">Beta</Card>;\n'


@pytest.mark.parametrize("filename", ["index.html", "Component.tsx"])
def test_live_discard_preserves_legitimate_nested_style_element(
    tmp_path: Path, filename: str
) -> None:
    page = tmp_path / filename
    source = "\n".join(
        [
            '<section class="target">',
            "  <style>.target { color:red; }</style>",
            "  <p>Original</p>",
            "</section>",
            "",
        ]
    )
    page.write_text(source)
    wrapped = _run_wrap(tmp_path, "--file", filename)
    assert wrapped.returncode == 0, wrapped.stderr

    discarded = _run_accept(tmp_path, "--discard")

    assert discarded.returncode == 0, discarded.stderr
    assert json.loads(discarded.stdout)["handled"] is True
    assert page.read_text() == source


@pytest.mark.parametrize(
    ("old", "new"),
    [
        (
            'data-impeccable-variants="security-test"',
            'data-impeccable-variants="different-session"',
        ),
        (
            'data-impeccable-variant="original"',
            'data-impeccable-variant="missing-original"',
        ),
    ],
)
def test_live_discard_fails_closed_when_session_structure_is_not_bound(
    tmp_path: Path, old: str, new: str
) -> None:
    page = tmp_path / "component.tsx"
    page.write_text('<Card className="target">ORIGINAL_SECRET</Card>\n')
    wrapped = _run_wrap(tmp_path, "--file", page.name)
    assert wrapped.returncode == 0, wrapped.stderr
    malformed = page.read_text().replace(old, new, 1)
    page.write_text(malformed)

    completed = _run_accept(tmp_path, "--discard")

    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout)["handled"] is False
    assert page.read_text() == malformed
    assert "ORIGINAL_SECRET" in page.read_text()


@pytest.mark.parametrize(
    "opening",
    [
        "const fixture = `",
        "const fixture = html`",
        "const backtickPattern = /`/;\nconst fixture = `",
    ],
)
def test_live_discard_ignores_exact_session_scaffolds_inside_template_literals(
    tmp_path: Path, opening: str
) -> None:
    page = tmp_path / "component.tsx"
    source = "\n".join(
        [
            opening,
            '<div data-impeccable-variants="security-test">',
            "  {/* impeccable-variants-start security-test */}",
            '  <div data-impeccable-variant="original">',
            "    <span>literal content</span>",
            "  </div>",
            "  {/* impeccable-variants-end security-test */}",
            "</div>",
            "`;",
            "",
        ]
    )
    page.write_text(source)

    completed = _run_accept(tmp_path, "--discard")

    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout)["handled"] is False
    assert page.read_text() == source


def test_live_discard_ignores_scaffolds_inside_nested_template_literals(
    tmp_path: Path,
) -> None:
    page = tmp_path / "component.tsx"
    source = "\n".join(
        [
            "const fixture = `outer ${`inner",
            '<div data-impeccable-variants="security-test">',
            "  {/* impeccable-variants-start security-test */}",
            '  <div data-impeccable-variant="original">literal content</div>',
            "  {/* impeccable-variants-end security-test */}",
            "</div>",
            "`}`;",
            "",
        ]
    )
    page.write_text(source)

    completed = _run_accept(tmp_path, "--discard")

    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout)["handled"] is False
    assert page.read_text() == source


def test_live_discard_ignores_html_scaffold_inside_framework_script_template(
    tmp_path: Path,
) -> None:
    page = tmp_path / "Component.svelte"
    source = "\n".join(
        [
            "<script>",
            "const fixture = `",
            '<div data-impeccable-variants="security-test">',
            "  <!-- impeccable-variants-start security-test -->",
            '  <div data-impeccable-variant="original">',
            "    <span>literal content</span>",
            "  </div>",
            "  <!-- impeccable-variants-end security-test -->",
            "</div>",
            "`;",
            "</script>",
            '<section class="target">Real</section>',
            "",
        ]
    )
    page.write_text(source)

    completed = _run_accept(tmp_path, "--discard")

    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout)["handled"] is False
    assert page.read_text() == source


def test_live_discard_ignores_astro_frontmatter_template_delimiter_decoy(
    tmp_path: Path,
) -> None:
    page = tmp_path / "Page.astro"
    source = "\n".join(
        [
            "---",
            "const fixture = `",
            "---",
            '<div data-impeccable-variants="security-test">',
            "  <!-- impeccable-variants-start security-test -->",
            '  <div data-impeccable-variant="original"><span>literal</span></div>',
            "  <!-- impeccable-variants-end security-test -->",
            "</div>",
            "`;",
            "---",
            '<section class="target">Real</section>',
            "",
        ]
    )
    page.write_text(source)

    completed = _run_accept(tmp_path, "--discard")

    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout)["handled"] is False
    assert page.read_text() == source


@pytest.mark.parametrize("prefix", ['const tick = "`";\n', '// ` in a comment\n'])
def test_live_discard_finds_real_sessions_after_non_template_backticks(
    tmp_path: Path, prefix: str
) -> None:
    page = tmp_path / "component.tsx"
    page.write_text('<Card className="target">original</Card>\n')
    wrapped = _run_wrap(tmp_path, "--file", page.name)
    assert wrapped.returncode == 0, wrapped.stderr
    page.write_text(prefix + page.read_text())

    completed = _run_accept(tmp_path, "--discard")

    assert completed.returncode == 0, completed.stderr
    assert json.loads(completed.stdout)["handled"] is True
    assert page.read_text().startswith(prefix)
    assert '<Card className="target">original</Card>' in page.read_text()


def test_live_accept_preserves_jsx_that_contains_tag_shaped_string_content(
    tmp_path: Path,
) -> None:
    page = tmp_path / "component.tsx"
    page.write_text('<Card className="target">original</Card>\n<p>after</p>\n')
    wrapped = _run_wrap(tmp_path, "--file", page.name)
    assert wrapped.returncode == 0, wrapped.stderr
    text = page.read_text()
    marker_line = next(
        line for line in text.splitlines() if "Variants: insert below this line" in line
    )
    variant = "\n".join(
        [
            '  <div data-impeccable-variant="1">',
            "    <Card",
            '      preview={<div title=\"prop > value\">preview</div>}',
            "    >",
            '      {"</div>"}',
            '      <div title="nested > value">nested</div>',
            "    </Card>",
            "  </div>",
        ]
    )
    page.write_text(text.replace(marker_line, marker_line + "\n" + variant))

    accepted = _run_accept(tmp_path, "--variant", "1")

    assert accepted.returncode == 0, accepted.stderr
    result = page.read_text()
    assert '{"</div>"}' in result
    assert 'title="nested > value"' in result
    assert "<p>after</p>" in result
    assert "data-impeccable-variants" not in result


@pytest.mark.parametrize(
    ("filename", "source", "terminator"),
    [
        ("index.html", '<section class="target">original</section>', "-->"),
        ("component.tsx", '<section className="target">original</section>', "*/}"),
    ],
)
def test_live_accept_base64_encodes_param_values_inside_source_comments(
    tmp_path: Path, filename: str, source: str, terminator: str
) -> None:
    page = tmp_path / filename
    page.write_text(source + "\n")
    wrapped = _run_wrap(tmp_path, "--file", filename)
    assert wrapped.returncode == 0, wrapped.stderr
    text = page.read_text()
    marker = "Variants: insert below this line"
    marker_line = next(line for line in text.splitlines() if marker in line)
    indent = marker_line[: len(marker_line) - len(marker_line.lstrip())]
    style = (
        f'{indent}<style data-impeccable-css="security-test">\n'
        f"{indent}.target {{ color: red; }}\n"
        f"{indent}</style>\n"
        f'{indent}<div data-impeccable-variant="1">{source}</div>'
    )
    page.write_text(text.replace(marker_line, marker_line + "\n" + style))
    param_values = {"unsafe": terminator, "script": "</script>"}

    accepted = _run_accept(
        tmp_path,
        "--variant",
        "1",
        "--param-values",
        json.dumps(param_values),
    )

    assert accepted.returncode == 0, accepted.stderr
    param_line = next(
        line for line in page.read_text().splitlines() if "impeccable-param-values" in line
    )
    assert terminator not in param_line.removesuffix(terminator)
    encoded = param_line.split("base64:", 1)[1].split()[0]
    assert json.loads(base64.b64decode(encoded)) == param_values


def test_live_inject_preflights_every_anchor_before_changing_any_file(tmp_path: Path) -> None:
    good = tmp_path / "good.html"
    bad = tmp_path / "bad.html"
    good_original = "<html><body>good</body></html>\n"
    bad_original = "<html><main>no body anchor</main></html>\n"
    good.write_text(good_original)
    bad.write_text(bad_original)
    _write_config(tmp_path, ["good.html", "bad.html"])

    result = _run_inject(tmp_path, "--port", "8400", "--token", TOKEN)

    assert result.returncode != 0
    assert json.loads(result.stderr)["error"] == "mutation_preflight_failed"
    assert good.read_text() == good_original
    assert bad.read_text() == bad_original


def test_live_entrypoint_injects_private_server_token_without_logging_it(tmp_path: Path) -> None:
    page = tmp_path / "index.html"
    page.write_text("<html><body>safe</body></html>\n")
    _write_config(tmp_path, ["index.html"])
    try:
        result = subprocess.run(
            ["node", str(SCRIPTS / "live.mjs")],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
        )
        assert result.returncode == 0, result.stderr
        payload = json.loads(result.stdout)
        assert payload["ok"] is True
        assert "serverToken" not in payload
        private = json.loads((tmp_path / ".impeccable" / "live" / "server.json").read_text())
        token = private["token"]
        project_state = (tmp_path / ".impeccable" / "live" / "server.json").read_text()
        agent_state = Path(private["agentStatePath"])
        agent_token = json.loads(agent_state.read_text())["agentToken"]
        assert "agentToken" not in project_state
        assert not agent_state.is_relative_to(tmp_path)
        assert f"/live.js?token={token}" in page.read_text()
        assert agent_token not in page.read_text()
        assert token not in result.stdout
        assert token not in result.stderr
        assert agent_token not in result.stdout
        assert agent_token not in result.stderr
    finally:
        subprocess.run(
            ["node", str(SERVER), "stop"],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )


def test_live_entrypoint_emits_bounded_context_metadata_without_document_bodies(
    tmp_path: Path,
) -> None:
    page = tmp_path / "index.html"
    page.write_text("<html><body>safe</body></html>\n")
    _write_config(tmp_path, ["index.html"])
    product_sentinel = "PRIVATE_PRODUCT_BODY_" + "p" * 100_000
    design_sentinel = "PRIVATE_DESIGN_BODY_" + "d" * 100_000
    (tmp_path / "PRODUCT.md").write_text(f"# Product\n\n{product_sentinel}\n## Users\n")
    (tmp_path / "DESIGN.md").write_text(f"# Design\n\n{design_sentinel}\n## Components\n")
    try:
        result = subprocess.run(
            ["node", str(SCRIPTS / "live.mjs")],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
        )

        assert result.returncode == 0, result.stderr
        assert product_sentinel not in result.stdout
        assert design_sentinel not in result.stdout
        assert len(result.stdout) < 12_000
        payload = json.loads(result.stdout)
        assert "product" not in payload
        assert "design" not in payload
        assert payload["productPath"] == "PRODUCT.md"
        assert payload["designPath"] == "DESIGN.md"
        assert payload["productChars"] > 100_000
        assert payload["designChars"] > 100_000
        assert payload["productHeadings"] == [
            {"level": 1, "title": "Product"},
            {"level": 2, "title": "Users"},
        ]
        assert payload["designHeadings"] == [
            {"level": 1, "title": "Design"},
            {"level": 2, "title": "Components"},
        ]
        assert payload["metadataTruncation"]["productHeadings"]["total"] == 2
        assert payload["metadataTruncation"]["designHeadings"]["total"] == 2
    finally:
        subprocess.run(
            ["node", str(SERVER), "stop"],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )


def test_live_entrypoint_stops_only_the_server_it_started_when_injection_fails(
    tmp_path: Path,
) -> None:
    good = tmp_path / "good.html"
    bad = tmp_path / "bad.html"
    good.write_text("<html><body>good</body></html>\n")
    bad.write_text("<html><main>bad anchor</main></html>\n")
    _write_config(tmp_path, ["good.html", "bad.html"])

    result = subprocess.run(
        ["node", str(SCRIPTS / "live.mjs")],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
        timeout=15,
    )

    assert result.returncode != 0
    payload = json.loads(result.stdout)
    assert payload["error"] == "inject_failed"
    assert payload["serverCleanup"] == "stopped"
    state_path = tmp_path / ".impeccable" / "live" / "server.json"
    deadline = time.monotonic() + 5
    while state_path.exists() and time.monotonic() < deadline:
        time.sleep(0.05)
    assert not state_path.exists()
    assert "impeccable-live-start" not in good.read_text()


def test_live_entrypoint_does_not_stop_a_preexisting_server_when_injection_fails(
    tmp_path: Path,
) -> None:
    page = tmp_path / "index.html"
    page.write_text("<html><main>no body anchor</main></html>\n")
    _write_config(tmp_path, ["index.html"])

    with LiveServer(tmp_path) as server:
        result = subprocess.run(
            ["node", str(SCRIPTS / "live.mjs")],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
        )
        assert result.returncode != 0
        payload = json.loads(result.stdout)
        assert payload["serverCleanup"] == "not-owned"
        assert _request(f"{server.base_url}/health")[0] == 200


def test_live_server_foreground_logs_never_expose_its_bearer_token(tmp_path: Path) -> None:
    server = LiveServer(tmp_path)
    server.__enter__()
    token = server.token
    agent_token = server.agent_token
    server.close()
    assert server.process is not None
    stdout, stderr = server.process.communicate(timeout=3)
    assert token not in stdout
    assert token not in stderr
    assert agent_token not in stdout
    assert agent_token not in stderr


def _available_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


class LiveServer:
    def __init__(self, project: Path, *, env: dict[str, str] | None = None):
        self.project = project
        self.port = _available_port()
        self.process: subprocess.Popen[str] | None = None
        self.token = ""
        self.agent_token = ""
        self.env = env

    def __enter__(self):
        self.process = subprocess.Popen(
            ["node", str(SERVER), f"--port={self.port}"],
            cwd=self.project,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=self.env,
        )
        info_path = self.project / ".impeccable" / "live" / "server.json"
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                stdout, stderr = self.process.communicate()
                raise AssertionError(f"live server exited early\nstdout={stdout}\nstderr={stderr}")
            try:
                info = json.loads(info_path.read_text())
                self.token = info["token"]
                self.agent_token = json.loads(
                    Path(info["agentStatePath"]).read_text()
                )["agentToken"]
                if info["port"] == self.port:
                    return self
            except (FileNotFoundError, json.JSONDecodeError, KeyError):
                pass
            time.sleep(0.05)
        self.close()
        raise AssertionError("timed out waiting for live server")

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def close(self) -> None:
        if not self.process or self.process.poll() is not None:
            return
        try:
            _request(f"{self.base_url}/stop?token={quote(self.token)}")
        except (HTTPError, URLError, ConnectionError):
            pass
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            self.process.wait(timeout=3)

    def __exit__(self, _exc_type, _exc, _tb):
        self.close()


@pytest.mark.parametrize("body", [b"null", b"[]", b'"text"'])
@pytest.mark.parametrize("endpoint", ["events", "poll"])
def test_live_server_rejects_non_object_json_without_crashing(
    tmp_path: Path, endpoint: str, body: bytes,
) -> None:
    with LiveServer(tmp_path) as server:
        status, _, response = _request(
            f"{server.base_url}/{endpoint}",
            method="POST",
            body=body,
            extra_headers={"Content-Type": "application/json"},
        )
        assert status == 400
        assert "error" in json.loads(response)
        assert _request(f"{server.base_url}/health")[0] == 200


@pytest.mark.parametrize("port", ["0", "65536"])
def test_live_server_rejects_unusable_explicit_ports(
    tmp_path: Path, port: str
) -> None:
    process = subprocess.Popen(
        ["node", str(SERVER), f"--port={port}"],
        cwd=tmp_path,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 2
        while process.poll() is None and time.monotonic() < deadline:
            time.sleep(0.025)
        assert process.poll() is not None
        _, stderr = process.communicate(timeout=1)
        assert process.returncode != 0
        assert "invalid_port" in stderr
        assert not (tmp_path / ".impeccable" / "live" / "server.json").exists()
    finally:
        if process.poll() is None:
            process.terminate()
            process.wait(timeout=3)


def test_concurrent_background_launchers_only_claim_their_own_child(
    tmp_path: Path,
) -> None:
    launchers = [
        subprocess.Popen(
            ["node", str(SERVER), "--background"],
            cwd=tmp_path,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        for _ in range(2)
    ]
    results: list[tuple[int, str, str]] = []
    try:
        for launcher in launchers:
            stdout, stderr = launcher.communicate(timeout=15)
            results.append((launcher.returncode, stdout, stderr))
        successes = [result for result in results if result[0] == 0]
        failures = [result for result in results if result[0] != 0]
        assert len(successes) == 1, results
        assert len(failures) == 1, results
        public = json.loads(successes[0][1])
        private = json.loads(
            (tmp_path / ".impeccable" / "live" / "server.json").read_text()
        )
        assert private["pid"] == public["pid"]
        status, _, body = _request(
            f"http://127.0.0.1:{private['port']}/status?token={quote(private['token'])}"
        )
        assert status == 200
        assert json.loads(body)["pid"] == public["pid"]
    finally:
        subprocess.run(
            ["node", str(SERVER), "stop", "--keep-inject"],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
            timeout=5,
        )
        for launcher in launchers:
            if launcher.poll() is None:
                launcher.terminate()
                launcher.wait(timeout=3)


def _request(
    url: str,
    *,
    origin: str | None = None,
    method: str = "GET",
    body: bytes | None = None,
    extra_headers: dict[str, str] | None = None,
):
    headers = {"Origin": origin} if origin else {}
    headers.update(extra_headers or {})
    request = Request(url, headers=headers, method=method, data=body)
    try:
        with urlopen(request, timeout=3) as response:
            return response.status, response.headers, response.read().decode("utf-8")
    except HTTPError as error:
        return error.code, error.headers, error.read().decode("utf-8")


def _read_sse_messages(
    server: "LiveServer", count: int, *, last_event_id: str | None = None,
) -> tuple[list[dict[str, object]], str | None]:
    connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=1)
    headers = {"Last-Event-ID": last_event_id} if last_event_id is not None else {}
    connection.request(
        "GET", f"/events?token={quote(server.token)}", headers=headers,
    )
    response = connection.getresponse()
    assert response.status == 200
    messages: list[dict[str, object]] = []
    latest_event_id = last_event_id
    try:
        while len(messages) < count:
            line = response.fp.readline().decode("utf-8")
            if line.startswith("id: "):
                latest_event_id = line.removeprefix("id: ").strip()
            elif line.startswith("data: "):
                messages.append(json.loads(line.removeprefix("data: ")))
    finally:
        connection.close()
    return messages, latest_event_id


def test_live_script_requires_token_and_never_echoes_it_to_cross_origin_callers(
    tmp_path: Path,
) -> None:
    with LiveServer(tmp_path) as server:
        status, headers, body = _request(
            f"{server.base_url}/live.js", origin="https://attacker.example"
        )
        assert status == 401
        assert server.token not in body
        assert headers.get("Access-Control-Allow-Origin") is None

        status, headers, body = _request(
            f"{server.base_url}/live.js?token={quote(server.token)}",
            origin="https://attacker.example",
        )
        assert status == 200
        assert server.token not in body
        assert headers.get("Access-Control-Allow-Origin") is None
        assert "document.currentScript.src" in body
        assert "searchParams.get('token')" in body

        status, headers, _ = _request(
            f"{server.base_url}/events",
            origin="http://localhost:5173",
            method="OPTIONS",
        )
        assert status == 204
        assert headers.get("Access-Control-Allow-Origin") == "http://localhost:5173"
        assert headers.get("Access-Control-Allow-Origin") != "*"


def test_annotation_upload_refuses_an_existing_hard_link_without_changing_outside_bytes(
    tmp_path: Path,
) -> None:
    with LiveServer(tmp_path) as server:
        status, _, body = _request(
            f"{server.base_url}/annotation?token={quote(server.token)}&eventId=first",
            method="POST",
            body=b"first",
            extra_headers={"Content-Type": "image/png"},
        )
        assert status == 200
        first_path = Path(json.loads(body)["path"])
        session_dir = first_path.parent
        assert not session_dir.is_relative_to(tmp_path)
        assert session_dir.stat().st_mode & 0o777 == 0o700
        assert not (tmp_path / ".impeccable" / "live" / "annotations").exists()
        outside = tmp_path.parent / f"{tmp_path.name}-annotation-outside.png"
        outside.write_bytes(b"outside")
        os.link(outside, session_dir / "hard-link.png")

        status, _, body = _request(
            f"{server.base_url}/annotation?token={quote(server.token)}&eventId=hard-link",
            method="POST",
            body=b"replacement",
            extra_headers={"Content-Type": "image/png"},
        )

        assert status in {409, 500}
        assert "error" in json.loads(body)
        assert outside.read_bytes() == b"outside"


def test_live_status_identifies_the_authenticated_server_process(tmp_path: Path) -> None:
    with LiveServer(tmp_path) as server:
        event = {
            "token": server.token,
            "type": "generate",
            "id": "deadbeef",
            "action": "polish",
            "count": 1,
            "element": {"outerHTML": "<main>safe</main>"},
        }
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        status, _, body = _request(
            f"{server.base_url}/status?token={quote(server.token)}"
        )

        assert status == 200
        payload = json.loads(body)
        assert payload["pid"] == server.process.pid
        assert payload["port"] == server.port

        cli = subprocess.run(
            ["node", str(STATUS)],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
        )
        assert cli.returncode == 0, cli.stderr
        cli_payload = json.loads(cli.stdout)
        assert cli_payload["liveServer"]["pid"] == server.process.pid
        assert cli_payload["activeSessions"] == [
            {
                "id": "deadbeef",
                "phase": "generate_requested",
                "revision": 0,
                "hasPendingEvent": True,
                "pendingEventType": "generate",
            }
        ]


def test_live_server_does_not_replay_a_project_preseeded_journal(
    tmp_path: Path,
) -> None:
    sessions = tmp_path / ".impeccable" / "live" / "sessions"
    sessions.mkdir(parents=True)
    (sessions / "deadbeef.jsonl").write_text(
        json.dumps(
            {
                "seq": 1,
                "id": "deadbeef",
                "type": "discard",
                "ts": "2026-08-30T00:00:00.000Z",
                "event": {"id": "deadbeef", "type": "discard"},
            }
        )
        + "\n"
    )

    with LiveServer(tmp_path) as server:
        status = json.loads(
            _request(f"{server.base_url}/status?token={quote(server.token)}")[2]
        )

        assert status["pendingEvents"] == []


def _write_untrusted_pending_journal(project: Path) -> str:
    prompt_text = "PROJECT JOURNAL TEXT MUST NOT BECOME AN ACTION"
    sessions = project / ".impeccable" / "live" / "sessions"
    sessions.mkdir(parents=True, exist_ok=True)
    entries = [
        {
            "seq": 1,
            "id": "deadbeef",
            "type": "generate",
            "ts": "2026-08-30T00:00:00.000Z",
            "event": {
                "id": "deadbeef",
                "type": "generate",
                "action": "polish",
                "count": 1,
                "freeformPrompt": prompt_text,
                "element": {"outerHTML": "<main>untrusted</main>"},
            },
        },
        {
            "seq": 2,
            "id": "deadbeef",
            "type": "checkpoint",
            "ts": "2026-08-30T00:00:01.000Z",
            "event": {
                "id": "deadbeef",
                "type": "checkpoint",
                "revision": 7,
                "phase": prompt_text,
            },
        },
    ]
    (sessions / "deadbeef.jsonl").write_text(
        "\n".join(json.dumps(entry) for entry in entries) + "\n"
    )
    return prompt_text


def test_live_resume_reports_only_inert_advisory_session_metadata(
    tmp_path: Path,
) -> None:
    prompt_text = _write_untrusted_pending_journal(tmp_path)

    result = subprocess.run(
        ["node", str(RESUME), "--id", "deadbeef"],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert prompt_text not in result.stdout
    assert "outerHTML" not in result.stdout
    payload = json.loads(result.stdout)
    assert payload["authority"] == "advisory_untrusted"
    assert set(payload) == {"retained", "authority", "session", "instruction"}
    assert payload["session"] == {
        "id": "deadbeef",
        "phase": "unknown",
        "revision": 7,
        "hasPendingEvent": True,
        "pendingEventType": "generate",
    }
    assert "reissue" in payload["instruction"].lower()
    assert "authenticated browser" in payload["instruction"].lower()


def test_live_status_labels_retained_journals_untrusted_without_requeue_advice(
    tmp_path: Path,
) -> None:
    prompt_text = _write_untrusted_pending_journal(tmp_path)

    result = subprocess.run(
        ["node", str(STATUS)],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert prompt_text not in result.stdout
    assert "outerHTML" not in result.stdout
    payload = json.loads(result.stdout)
    assert payload["retainedSessionsAuthority"] == "advisory_untrusted"
    assert payload["activeSessions"] == [
        {
            "id": "deadbeef",
            "phase": "unknown",
            "revision": 7,
            "hasPendingEvent": True,
            "pendingEventType": "generate",
        }
    ]
    assert "requeue" not in payload["recoveryHint"].lower()
    assert "reissue" in payload["recoveryHint"].lower()
    assert "authenticated browser" in payload["recoveryHint"].lower()


def test_live_status_sanitises_a_spoofed_loopback_status_response(
    tmp_path: Path,
) -> None:
    live_dir = tmp_path / ".impeccable" / "live"
    live_dir.mkdir(parents=True)
    (live_dir / "server.json").write_text(
        json.dumps({"pid": 42, "port": 8400, "token": TOKEN})
    )
    sentinel = "SPOOFED_STATUS_TEXT_MUST_NOT_ESCAPE"
    status_url = STATUS.as_uri()
    script = (
        "globalThis.fetch=async()=>({ok:true,json:async()=>({"
        "status:{freeformPrompt:'" + sentinel + "'},"
        "pid:'" + sentinel + "',port:8400,"
        "connectedClients:{outerHTML:'" + sentinel + "'},"
        "pendingEvents:["
        "{id:'deadbeef',type:'generate',leased:true,leaseUntil:123,"
        "freeformPrompt:'" + sentinel + "'},"
        "{id:'bad id',type:'" + sentinel + "',nested:{outerHTML:'" + sentinel + "'}}],"
        "activeSessions:[{id:'deadbeef',phase:'generate_requested',"
        "pendingEvent:{freeformPrompt:'" + sentinel + "',"
        "element:{outerHTML:'" + sentinel + "'}}}]})});"
        f"const module=await import({json.dumps(status_url)});"
        "await module.statusCli();"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert sentinel not in result.stdout
    assert "outerHTML" not in result.stdout
    assert "freeformPrompt" not in result.stdout
    payload = json.loads(result.stdout)
    assert payload["activeSessions"] == []
    assert payload["liveServer"] == {
        "status": "unknown",
        "pid": None,
        "port": 8400,
        "connectedClients": None,
        "pendingEvents": [
            {
                "id": "deadbeef",
                "type": "generate",
                "leased": True,
                "leaseUntil": 123,
            }
        ],
    }


def _write_invalid_advisory_session_files(project: Path) -> None:
    sessions = project / ".impeccable" / "live" / "sessions"
    sessions.mkdir(parents=True, exist_ok=True)
    (sessions / "bad!.jsonl").write_text("{}\n")
    (sessions / "deadbeef.jsonl").write_text("{not-json}\n")


def test_live_status_skips_unsafe_and_malformed_advisory_session_files(
    tmp_path: Path,
) -> None:
    _write_invalid_advisory_session_files(tmp_path)

    result = subprocess.run(
        ["node", str(STATUS)],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["activeSessions"] == []


def test_server_status_skips_unsafe_and_malformed_advisory_session_files(
    tmp_path: Path,
) -> None:
    _write_invalid_advisory_session_files(tmp_path)

    with LiveServer(tmp_path) as server:
        status, _, body = _request(
            f"{server.base_url}/status?token={quote(server.token)}"
        )

    assert status == 200
    assert json.loads(body)["activeSessions"] == []


@pytest.mark.parametrize("change_unconfigured_source", [False, True])
def test_live_server_does_not_replay_pending_events_after_a_crash(
    tmp_path: Path, change_unconfigured_source: bool,
) -> None:
    page = tmp_path / "index.html"
    page.write_text("<html><body>safe</body></html>\n")
    component = tmp_path / "component.tsx"
    component.write_text("export const Card = () => <article>before</article>;\n")
    _write_config(tmp_path, ["index.html"])
    first = LiveServer(tmp_path)
    first.__enter__()
    try:
        event = {"token": first.token, "type": "discard", "id": "deadbeef"}
        assert _request(
            f"{first.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        assert first.process is not None
        first.process.kill()
        first.process.wait(timeout=3)
        if change_unconfigured_source:
            component.write_text("export const Card = () => <article>changed</article>;\n")

        with LiveServer(tmp_path) as recovered:
            status, _, body = _request(
                f"{recovered.base_url}/poll?token={quote(recovered.agent_token)}"
                "&timeout=1&leaseMs=1000"
            )
            assert status == 200
            assert json.loads(body)["type"] == "timeout"
    finally:
        first.close()


def test_live_event_token_never_reaches_poll_or_durable_journal(tmp_path: Path) -> None:
    event_id = "deadbeef"
    with LiveServer(tmp_path) as server:
        status, _, uploaded = _request(
            f"{server.base_url}/annotation?token={quote(server.token)}&eventId={event_id}",
            method="POST",
            body=b"\x89PNG\r\n\x1a\nprivate",
            extra_headers={"Content-Type": "image/png"},
        )
        assert status == 200, uploaded
        screenshot_path = json.loads(uploaded)["path"]
        event = {
            "token": server.token,
            "type": "generate",
            "id": event_id,
            "action": "polish",
            "count": 1,
            "element": {"outerHTML": "<main>safe</main>"},
            "screenshotPath": screenshot_path,
        }
        status, _, body = _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert status == 200, body

        status, _, polled = _request(
            f"{server.base_url}/poll?token={quote(server.agent_token)}&timeout=1000&leaseMs=1000"
        )
        assert status == 200
        polled_payload = json.loads(polled)
        assert polled_payload["id"] == event_id
        assert polled_payload["screenshotPath"] == screenshot_path
        assert server.token not in polled

        journal = tmp_path / ".impeccable" / "live" / "sessions" / f"{event_id}.jsonl"
        assert journal.exists()
        assert server.token not in journal.read_text()
        assert screenshot_path not in journal.read_text()


def test_live_generate_rejects_unbound_browser_supplied_screenshot_path(
    tmp_path: Path,
) -> None:
    outside = tmp_path / "unrelated.png"
    outside.write_bytes(b"\x89PNG\r\n\x1a\nsecret")
    with LiveServer(tmp_path) as server:
        event = {
            "token": server.token,
            "type": "generate",
            "id": "deadbeef",
            "action": "polish",
            "count": 1,
            "element": {"outerHTML": "<main>safe</main>"},
            "screenshotPath": str(outside),
        }
        status, _, body = _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )

        assert status == 400
        assert "not bound" in json.loads(body)["error"]
        status, _, polled = _request(
            f"{server.base_url}/poll?token={quote(server.agent_token)}&timeout=1&leaseMs=1000"
        )
        assert status == 200
        assert json.loads(polled)["type"] == "timeout"


def test_browser_credential_cannot_complete_work_and_error_does_not_starve_queue(
    tmp_path: Path,
) -> None:
    event_id = "deadbeef"
    with LiveServer(tmp_path) as server:
        event = {
            "token": server.token,
            "type": "accept",
            "id": event_id,
            "variantId": "1",
        }
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200

        def reply(
            token: str,
            event_type: str,
            lease_token: str | None = None,
            reply_id: str = event_id,
        ) -> tuple[int, object, str]:
            return _request(
                f"{server.base_url}/poll",
                method="POST",
                body=json.dumps(
                    {
                        "token": token,
                        "leaseToken": lease_token,
                        "id": reply_id,
                        "type": event_type,
                    }
                ).encode(),
                extra_headers={"Content-Type": "application/json"},
            )

        assert reply(server.token, "complete")[0] == 401
        assert reply(server.agent_token, "complete")[0] == 409

        poll_url = (
            f"{server.base_url}/poll?token={quote(server.agent_token)}"
            "&timeout=1000&leaseMs=1000"
        )
        first_lease = json.loads(_request(poll_url)[2])
        assert first_lease["id"] == event_id
        assert reply(server.agent_token, "erorr")[0] == 400
        assert reply(server.agent_token, "done", first_lease["leaseToken"])[0] == 409
        assert reply(server.agent_token, "error", first_lease["leaseToken"])[0] == 200
        next_id = "cafebabe"
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(
                {"token": server.token, "type": "discard", "id": next_id}
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        second_lease = json.loads(_request(poll_url)[2])
        assert second_lease["id"] == next_id
        assert second_lease["leaseToken"] != first_lease["leaseToken"]
        assert reply(server.agent_token, "complete", first_lease["leaseToken"])[0] == 409
        assert reply(
            server.agent_token,
            "discarded",
            second_lease["leaseToken"],
            next_id,
        )[0] == 200

        status = json.loads(
            _request(
                f"{server.base_url}/status?token={quote(server.token)}"
            )[2]
        )
        assert status["pendingEvents"] == []


@pytest.mark.parametrize(
    ("browser_event", "agent_reply"),
    [("accept", "complete"), ("discard", "discarded"), ("accept", "error")],
)
def test_live_server_replays_terminal_outcome_after_browser_reconnect(
    tmp_path: Path, browser_event: str, agent_reply: str,
) -> None:
    event_id = "deadbeef"
    with LiveServer(tmp_path) as server:
        event = {"token": server.token, "type": browser_event, "id": event_id}
        if browser_event == "accept":
            event["variantId"] = "1"
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        leased = json.loads(
            _request(
                f"{server.base_url}/poll?token={quote(server.agent_token)}"
                "&timeout=1000&leaseMs=1000"
            )[2]
        )
        assert leased["id"] == event_id
        initial, watermark = _read_sse_messages(server, 1)
        assert initial[0]["type"] == "connected"
        assert watermark is not None

        status, _, body = _request(
            f"{server.base_url}/poll",
            method="POST",
            body=json.dumps(
                {
                    "token": server.agent_token,
                    "leaseToken": leased["leaseToken"],
                    "id": event_id,
                    "type": agent_reply,
                }
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert status == 200, body

        reconnected, _ = _read_sse_messages(
            server, 2, last_event_id=watermark,
        )
        connected, replayed = reconnected
        assert connected["type"] == "connected"
        assert replayed == {"type": agent_reply, "id": event_id}


def test_live_server_signals_when_terminal_replay_history_has_expired(
    tmp_path: Path,
) -> None:
    with LiveServer(tmp_path) as server:
        poll_url = (
            f"{server.base_url}/poll?token={quote(server.agent_token)}"
            "&timeout=1000&leaseMs=1000"
        )
        for value in range(129):
            event_id = f"{value:08x}"
            assert _request(
                f"{server.base_url}/events",
                method="POST",
                body=json.dumps(
                    {"token": server.token, "type": "discard", "id": event_id}
                ).encode(),
                extra_headers={"Content-Type": "application/json"},
            )[0] == 200
            lease = json.loads(_request(poll_url)[2])
            assert _request(
                f"{server.base_url}/poll",
                method="POST",
                body=json.dumps(
                    {
                        "token": server.agent_token,
                        "leaseToken": lease["leaseToken"],
                        "id": event_id,
                        "type": "discarded",
                    }
                ).encode(),
                extra_headers={"Content-Type": "application/json"},
            )[0] == 200

        messages, watermark = _read_sse_messages(server, 130, last_event_id="0")

        assert messages[0] == {"type": "connected", "hasProjectContext": False}
        assert messages[1] == {"type": "discarded", "id": "00000001"}
        assert messages[-2] == {"type": "discarded", "id": "00000080"}
        assert messages[-1] == {"type": "replay_gap"}
        assert watermark == "129"


def test_expired_lease_cannot_override_a_competing_worker_lease(
    tmp_path: Path,
) -> None:
    event_id = "deadbeef"
    with LiveServer(tmp_path) as server:
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(
                {"token": server.token, "type": "discard", "id": event_id}
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        poll_url = (
            f"{server.base_url}/poll?token={quote(server.agent_token)}"
            "&timeout=1000&leaseMs=25"
        )
        first_lease = json.loads(_request(poll_url)[2])
        assert first_lease["id"] == event_id
        assert first_lease["leaseToken"]

        time.sleep(0.06)
        second_lease = json.loads(_request(poll_url)[2])
        assert second_lease["id"] == event_id
        assert second_lease["leaseToken"] != first_lease["leaseToken"]

        def acknowledge(lease_token: str) -> tuple[int, object, str]:
            return _request(
                f"{server.base_url}/poll",
                method="POST",
                body=json.dumps(
                    {
                        "token": server.agent_token,
                        "leaseToken": lease_token,
                        "id": event_id,
                        "type": "discarded",
                    }
                ).encode(),
                extra_headers={"Content-Type": "application/json"},
            )

        stale = acknowledge(first_lease["leaseToken"])
        assert stale[0] == 409
        assert json.loads(stale[2])["error"] == "No matching leased event"
        current = acknowledge(second_lease["leaseToken"])
        assert current[0] == 200, current[2]


def test_live_reply_persists_before_removing_the_leased_event(tmp_path: Path) -> None:
    event_id = "deadbeef"
    with LiveServer(tmp_path) as server:
        event = {
            "token": server.token,
            "type": "accept",
            "id": event_id,
            "variantId": "1",
        }
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        poll_url = (
            f"{server.base_url}/poll?token={quote(server.agent_token)}"
            "&timeout=1000&leaseMs=5000"
        )
        lease = json.loads(_request(poll_url)[2])
        assert lease["id"] == event_id

        sessions = tmp_path / ".impeccable" / "live" / "sessions"
        moved = sessions.with_name("sessions-moved")
        outside = tmp_path / "outside-state"
        outside.mkdir()
        sessions.rename(moved)
        sessions.symlink_to(outside, target_is_directory=True)
        try:
            rejected = _request(
                f"{server.base_url}/poll",
                method="POST",
                body=json.dumps(
                    {
                        "token": server.agent_token,
                        "leaseToken": lease["leaseToken"],
                        "id": event_id,
                        "type": "complete",
                    }
                ).encode(),
                extra_headers={"Content-Type": "application/json"},
            )
            assert rejected[0] == 500
            assert json.loads(rejected[2])["error"] == "session_store_append_failed"
            assert not list(outside.iterdir())
        finally:
            sessions.unlink()
            moved.rename(sessions)

        status = json.loads(
            _request(f"{server.base_url}/status?token={quote(server.token)}")[2]
        )
        assert status["pendingEvents"][0]["id"] == event_id
        completed = _request(
            f"{server.base_url}/poll",
            method="POST",
            body=json.dumps(
                {
                    "token": server.agent_token,
                    "leaseToken": lease["leaseToken"],
                    "id": event_id,
                    "type": "complete",
                }
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert completed[0] == 200, completed[2]


@pytest.mark.parametrize(
    ("completion_args", "expected_phase"),
    [([], "completed"), (["--error", "cleanup failed"], "agent_error")],
)
def test_carbonized_accept_can_finish_through_the_live_server(
    tmp_path: Path, completion_args: list[str], expected_phase: str,
) -> None:
    event_id = "deadbeef"
    with LiveServer(tmp_path) as server:
        event = {
            "token": server.token,
            "type": "accept",
            "id": event_id,
            "variantId": "1",
        }
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(event).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        poll_url = (
            f"{server.base_url}/poll?token={quote(server.agent_token)}"
            "&timeout=1000&leaseMs=1000"
        )
        lease = json.loads(_request(poll_url)[2])
        assert lease["id"] == event_id
        acknowledged = _request(
            f"{server.base_url}/poll",
            method="POST",
            body=json.dumps(
                {
                    "token": server.agent_token,
                    "leaseToken": lease["leaseToken"],
                    "id": event_id,
                    "type": "agent_done",
                    "data": {"carbonize": True},
                }
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert acknowledged[0] == 200, acknowledged[2]
        status = json.loads(
            _request(f"{server.base_url}/status?token={quote(server.token)}")[2]
        )
        assert status["pendingEvents"] == []
        assert status["activeSessions"][0]["phase"] == "carbonize_required"

        completed = subprocess.run(
            [
                "node",
                str(SCRIPTS / "live-complete.mjs"),
                "--id",
                event_id,
                *completion_args,
            ],
            cwd=tmp_path,
            check=False,
            capture_output=True,
            text=True,
        )
        assert completed.returncode == 0, completed.stderr
        assert json.loads(completed.stdout)["phase"] == expected_phase
        status = json.loads(
            _request(f"{server.base_url}/status?token={quote(server.token)}")[2]
        )
        assert status["pendingEvents"] == []
        if expected_phase == "completed":
            assert status["activeSessions"] == []
        else:
            assert status["activeSessions"][0]["phase"] == "agent_error"


def test_carbonized_accept_finishes_locally_when_stale_server_state_has_no_agent_state(
    tmp_path: Path,
) -> None:
    event_id = "deadbeef"
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    setup = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        f"store.appendEvent({{type:'agent_done',id:{json.dumps(event_id)},carbonize:true}});"
    )
    prepared = subprocess.run(
        ["node", "--input-type=module", "-e", setup, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert prepared.returncode == 0, prepared.stderr
    live_dir = tmp_path / ".impeccable" / "live"
    (live_dir / "server.json").write_text(
        json.dumps({"pid": 2_147_483_647, "port": 8400, "token": TOKEN})
    )

    completed = subprocess.run(
        ["node", str(SCRIPTS / "live-complete.mjs"), "--id", event_id],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert completed.returncode == 0, completed.stderr
    payload = json.loads(completed.stdout)
    assert payload["ok"] is True
    assert payload["phase"] == "completed"
    journal = live_dir / "sessions" / f"{event_id}.jsonl"
    assert json.loads(journal.read_text().splitlines()[-1])["type"] == "complete"


@pytest.mark.parametrize(
    ("setup_event", "completion_args"),
    [
        (None, []),
        ({"type": "generate", "id": "deadbeef", "count": 1}, []),
        ({"type": "agent_done", "id": "deadbeef", "carbonize": True}, ["--discarded"]),
    ],
)
def test_offline_completion_requires_an_existing_phase_compatible_journal(
    tmp_path: Path,
    setup_event: dict[str, object] | None,
    completion_args: list[str],
) -> None:
    if setup_event is not None:
        module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
        setup = (
            f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
            "const store=createLiveSessionStore({cwd:process.argv[1]});"
            f"store.appendEvent({json.dumps(setup_event)});"
        )
        prepared = subprocess.run(
            ["node", "--input-type=module", "-e", setup, str(tmp_path)],
            check=False,
            capture_output=True,
            text=True,
        )
        assert prepared.returncode == 0, prepared.stderr

    completed = subprocess.run(
        [
            "node",
            str(SCRIPTS / "live-complete.mjs"),
            "--id",
            "deadbeef",
            *completion_args,
        ],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert completed.returncode != 0
    assert json.loads(completed.stderr)["error"] == "live_completion_rejected"
    journal = tmp_path / ".impeccable" / "live" / "sessions" / "deadbeef.jsonl"
    if journal.exists():
        assert json.loads(journal.read_text().splitlines()[-1])["type"] == setup_event["type"]


def test_offline_completion_rejects_a_malformed_carbonize_journal(tmp_path: Path) -> None:
    event_id = "deadbeef"
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    setup = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        f"store.appendEvent({{type:'agent_done',id:{json.dumps(event_id)},carbonize:true}});"
    )
    prepared = subprocess.run(
        ["node", "--input-type=module", "-e", setup, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )
    assert prepared.returncode == 0, prepared.stderr
    journal = tmp_path / ".impeccable" / "live" / "sessions" / f"{event_id}.jsonl"
    journal.write_text(journal.read_text() + "{malformed\n")
    before = journal.read_text()

    completed = subprocess.run(
        ["node", str(SCRIPTS / "live-complete.mjs"), "--id", event_id],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert completed.returncode != 0
    assert json.loads(completed.stderr)["error"] == "live_completion_rejected"
    assert journal.read_text() == before


def test_live_complete_rejects_a_missing_agent_state_at_an_unsafe_path(
    tmp_path: Path,
) -> None:
    live_dir = tmp_path / ".impeccable" / "live"
    live_dir.mkdir(parents=True)
    unsafe_agent_path = tmp_path.parent / "untrusted-live-state" / "agent.json"
    (live_dir / "server.json").write_text(
        json.dumps(
            {
                "pid": 2_147_483_647,
                "port": 8400,
                "token": TOKEN,
                "agentStatePath": str(unsafe_agent_path),
            }
        )
    )

    completed = subprocess.run(
        ["node", str(SCRIPTS / "live-complete.mjs"), "--id", "deadbeef"],
        cwd=tmp_path,
        check=False,
        capture_output=True,
        text=True,
    )

    assert completed.returncode != 0
    assert json.loads(completed.stderr)["error"] == "live_completion_rejected"
    assert not (live_dir / "sessions" / "deadbeef.jsonl").exists()


def test_session_store_normalization_redacts_token_defensively(tmp_path: Path) -> None:
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1],sessionId:'deadbeef'});"
        "store.appendEvent({id:'deadbeef',type:'generate',token:'journal-secret',"
        "screenshotPath:'/tmp/transient.png'});"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    journal = tmp_path / ".impeccable" / "live" / "sessions" / "deadbeef.jsonl"
    assert "journal-secret" not in journal.read_text()
    assert "/tmp/transient.png" not in journal.read_text()


def test_session_store_recovery_never_restores_a_transient_screenshot_path(
    tmp_path: Path,
) -> None:
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const cwd=process.argv[1];"
        "createLiveSessionStore({cwd}).appendEvent({id:'deadbeef',type:'generate',"
        "action:'polish',count:1,screenshotPath:'/tmp/transient.png'});"
        "const recovered=createLiveSessionStore({cwd}).getSnapshot('deadbeef');"
        "process.stdout.write(JSON.stringify(recovered));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(tmp_path)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    recovered = json.loads(result.stdout)
    assert "screenshotPath" not in recovered["pendingEvent"]
    assert recovered["annotationArtifacts"] == []


@pytest.mark.parametrize(
    "query",
    [
        "timeout=0",
        "timeout=-1",
        "timeout=NaN",
        "timeout=1.5",
        "timeout=600001",
        "leaseMs=0",
        "leaseMs=-1",
        "leaseMs=NaN",
        "leaseMs=1.5",
        "leaseMs=600001",
    ],
)
def test_live_poll_rejects_malformed_timeout_and_lease_values(
    tmp_path: Path, query: str
) -> None:
    with LiveServer(tmp_path) as server:
        status, _, body = _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(
                {"token": server.token, "type": "discard", "id": "deadbeef"}
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )
        assert status == 200, body
        status, _, body = _request(
            f"{server.base_url}/poll?token={quote(server.agent_token)}&{query}"
        )

        assert status == 400
        assert json.loads(body)["error"] == "Invalid poll bounds"


def test_default_lease_covers_the_normal_ten_minute_agent_poll_window(
    tmp_path: Path,
) -> None:
    with LiveServer(tmp_path) as server:
        assert _request(
            f"{server.base_url}/events",
            method="POST",
            body=json.dumps(
                {"token": server.token, "type": "discard", "id": "deadbeef"}
            ).encode(),
            extra_headers={"Content-Type": "application/json"},
        )[0] == 200
        before_poll_ms = int(time.time() * 1000)
        assert _request(
            f"{server.base_url}/poll?token={quote(server.agent_token)}&timeout=1000"
        )[0] == 200
        status = json.loads(
            _request(f"{server.base_url}/status?token={quote(server.token)}")[2]
        )

        assert status["pendingEvents"][0]["leaseUntil"] >= before_poll_ms + 590_000


def test_design_sidecar_endpoint_rejects_a_symlink_instead_of_serving_it(
    tmp_path: Path,
) -> None:
    outside = tmp_path.parent / f"{tmp_path.name}-design-secret.json"
    outside.write_text(json.dumps({"schemaVersion": 2, "secret": "do-not-serve"}))
    sidecar = tmp_path / ".impeccable" / "design.json"
    sidecar.parent.mkdir(parents=True)
    sidecar.symlink_to(outside)

    with LiveServer(tmp_path) as server:
        status, _, body = _request(
            f"{server.base_url}/design-system.json?token={quote(server.token)}"
        )

        assert status == 200
        assert "do-not-serve" not in body
        payload = json.loads(body)
        assert payload["hasSidecar"] is False
        assert "sidecarError" in payload


def test_design_markdown_endpoint_rejects_a_project_symlink(tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    secret = tmp_path / "outside-design.md"
    secret.write_text("# do-not-serve\n")
    (project / "DESIGN.md").symlink_to(secret)

    with LiveServer(project) as server:
        status, _, body = _request(
            f"{server.base_url}/design-system/raw?token={quote(server.token)}"
        )

        assert status == 403
        assert "do-not-serve" not in body


def test_design_endpoints_allow_an_explicit_external_context_root(tmp_path: Path) -> None:
    project = tmp_path / "project"
    context = tmp_path / "authorised-context"
    project.mkdir()
    context.mkdir()
    design_md = "# Design system\n\n## Direction\nQuiet and useful.\n"
    design_json = {"schemaVersion": 2, "narrative": {"direction": "quiet"}}
    (context / "DESIGN.md").write_text(design_md)
    (context / "DESIGN.json").write_text(json.dumps(design_json))
    env = {**os.environ, "IMPECCABLE_CONTEXT_DIR": str(context)}

    with LiveServer(project, env=env) as server:
        raw_status, _, raw = _request(
            f"{server.base_url}/design-system/raw?token={quote(server.token)}"
        )
        json_status, _, body = _request(
            f"{server.base_url}/design-system.json?token={quote(server.token)}"
        )

        assert raw_status == 200
        assert raw == design_md
        assert json_status == 200
        payload = json.loads(body)
        assert payload["hasMd"] is True
        assert payload["hasSidecar"] is True
        assert payload["sidecar"] == design_json


@pytest.mark.parametrize("symlink_part", [".impeccable", "live", "sessions"])
def test_live_server_rejects_a_preexisting_symlinked_state_root(
    tmp_path: Path, symlink_part: str
) -> None:
    project = tmp_path / "project"
    outside = tmp_path / "outside-state"
    project.mkdir()
    outside.mkdir()
    if symlink_part == ".impeccable":
        (project / ".impeccable").symlink_to(outside, target_is_directory=True)
    elif symlink_part == "live":
        impeccable = project / ".impeccable"
        impeccable.mkdir()
        (impeccable / "live").symlink_to(outside, target_is_directory=True)
    else:
        live = project / ".impeccable" / "live"
        live.mkdir(parents=True)
        (live / "sessions").symlink_to(outside, target_is_directory=True)

    result = subprocess.run(
        ["node", str(SERVER), f"--port={_available_port()}"],
        cwd=project,
        check=False,
        capture_output=True,
        text=True,
        timeout=5,
    )

    assert result.returncode != 0
    assert "live_state_root_invalid" in result.stderr
    assert not (outside / "server.json").exists()
    assert not list(outside.glob("*.jsonl"))
    assert not list(outside.glob("*.snapshot.json"))


@pytest.mark.parametrize("link_kind", ["symlink", "hardlink"])
@pytest.mark.parametrize("suffix", [".jsonl", ".snapshot.json"])
def test_live_session_store_rejects_linked_state_files(
    tmp_path: Path, suffix: str, link_kind: str
) -> None:
    project = tmp_path / "project"
    sessions = project / ".impeccable" / "live" / "sessions"
    sessions.mkdir(parents=True)
    outside = tmp_path / "outside.txt"
    outside.write_text("outside-safe")
    state_file = sessions / f"security-test{suffix}"
    if link_kind == "symlink":
        state_file.symlink_to(outside)
    else:
        os.link(outside, state_file)
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        "store.appendEvent({id:'security-test',type:'generate'});"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(project)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode != 0
    assert outside.read_text() == "outside-safe"


def test_live_session_store_rejects_a_post_construction_parent_swap(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    outside = tmp_path / "outside-state"
    project.mkdir()
    outside.mkdir()
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        "import fs from 'node:fs';"
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        "fs.renameSync(store.rootDir,store.rootDir+'-moved');"
        "fs.symlinkSync(process.argv[2],store.rootDir,'dir');"
        "try{store.appendEvent({id:'deadbeef',type:'accept',variantId:'1'})}"
        "catch(error){process.stdout.write(error.code);process.exit(7)}"
    )

    result = subprocess.run(
        [
            "node",
            "--input-type=module",
            "-e",
            script,
            str(project),
            str(outside),
        ],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 7
    assert result.stdout == "live_state_root_invalid"
    assert not list(outside.iterdir())


def test_live_session_store_ignores_a_symlinked_legacy_parent(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    outside = tmp_path / "outside-state"
    project.mkdir()
    outside.mkdir()
    (outside / "deadbeef.jsonl").write_text(
        json.dumps({"seq": 1, "id": "deadbeef", "type": "accept"}) + "\n"
    )
    legacy = project / ".impeccable-live"
    legacy.mkdir()
    (legacy / "sessions").symlink_to(outside, target_is_directory=True)
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        "process.stdout.write(JSON.stringify(store.listActiveSessions()));"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(project)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout) == []
    assert not list((project / ".impeccable" / "live" / "sessions").iterdir())


def test_live_session_store_rejects_a_post_construction_legacy_parent_swap(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    legacy = project / ".impeccable-live" / "sessions"
    outside = tmp_path / "outside-state"
    legacy.mkdir(parents=True)
    outside.mkdir()
    (legacy / "deadbeef.jsonl").write_text("")
    (outside / "deadbeef.jsonl").write_text(
        json.dumps({"seq": 1, "id": "deadbeef", "type": "accept"}) + "\n"
    )
    module_url = (SCRIPTS / "live-session-store.mjs").as_uri()
    script = (
        "import fs from 'node:fs';"
        f"import {{ createLiveSessionStore }} from {json.dumps(module_url)};"
        "const store=createLiveSessionStore({cwd:process.argv[1]});"
        "fs.renameSync(store.legacyRootDir,store.legacyRootDir+'-moved');"
        "fs.symlinkSync(process.argv[2],store.legacyRootDir,'dir');"
        "try{store.getSnapshot('deadbeef')}"
        "catch(error){process.stdout.write(error.code);process.exit(7)}"
    )

    result = subprocess.run(
        ["node", "--input-type=module", "-e", script, str(project), str(outside)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 7
    assert result.stdout == "live_state_root_invalid"


def test_live_serves_the_checked_in_detector_and_screenshot_asset_contracts(
    tmp_path: Path,
) -> None:
    detector_path = SCRIPTS / "detector" / "detect-antipatterns-browser.js"
    screenshot_path = SCRIPTS / "modern-screenshot.umd.js"
    with LiveServer(tmp_path) as server:
        status, headers, detector = _request(f"{server.base_url}/detect.js")
        assert status == 200
        assert headers.get_content_type() == "application/javascript"
        assert detector == detector_path.read_text()

        status, headers, screenshot = _request(
            f"{server.base_url}/modern-screenshot.js"
        )
        assert status == 200
        assert headers.get_content_type() == "application/javascript"
        assert screenshot == screenshot_path.read_text()

    for asset in (detector_path, screenshot_path):
        checked = subprocess.run(
            ["node", "--check", str(asset)],
            check=False,
            capture_output=True,
            text=True,
        )
        assert checked.returncode == 0, checked.stderr


def test_source_endpoint_rejects_ancestor_sibling_prefix_and_symlink_escape(
    tmp_path: Path,
) -> None:
    inside = tmp_path / "inside..page.html"
    inside.write_text("inside")
    secret = tmp_path / ".env"
    secret.write_text("API_KEY=do-not-serve")
    disguised_secret = tmp_path / "disguised.html"
    disguised_secret.symlink_to(secret)
    unconfigured = tmp_path / "other.html"
    unconfigured.write_text("not configured")
    outside_dir = tmp_path.parent / f"{tmp_path.name}-sibling"
    outside_dir.mkdir(exist_ok=True)
    outside = outside_dir / "secret.html"
    outside.write_text("secret")
    (tmp_path / "escape.html").symlink_to(outside)
    _write_config(tmp_path, [inside.name, "disguised.html"])

    with LiveServer(tmp_path) as server:
        def source(path_value: str):
            return _request(
                f"{server.base_url}/source?token={quote(server.token)}&path={quote(path_value, safe='')}"
            )

        assert source(inside.name)[0:3:2] == (200, "inside")
        assert source(".env")[0] == 403
        assert source("other.html")[0] == 403
        assert source("disguised.html")[0] == 403
        assert source(f"../{outside_dir.name}/secret.html")[0] == 403
        assert source(str(outside))[0] == 400
        assert source("escape.html")[0] == 403


def test_authenticated_null_origin_is_supported_without_unauthenticated_cors(
    tmp_path: Path,
) -> None:
    page = tmp_path / "index.html"
    page.write_text("<html><body>page</body></html>")
    _write_config(tmp_path, ["index.html"])

    with LiveServer(tmp_path) as server:
        status, headers, body = _request(
            f"{server.base_url}/source?token={quote(server.token)}&path=index.html",
            origin="null",
        )
        assert status == 200
        assert body == page.read_text()
        assert headers.get("Access-Control-Allow-Origin") == "null"
        assert headers.get("Access-Control-Allow-Origin") != "*"

        status, headers, body = _request(
            f"{server.base_url}/source?token=wrong&path=index.html",
            origin="null",
        )
        assert status == 401
        assert body == "Unauthorized"
        assert headers.get("Access-Control-Allow-Origin") is None


def test_json_post_endpoints_reject_declared_and_streamed_oversize_bodies(
    tmp_path: Path,
) -> None:
    with LiveServer(tmp_path) as server:
        oversized = b'{"padding":"' + (b'x' * (300 * 1024)) + b'"}'
        status, _, body = _request(
            f"{server.base_url}/events",
            method="POST",
            body=oversized,
            extra_headers={"Content-Type": "application/json"},
        )
        assert status == 413
        assert json.loads(body)["error"] == "Payload too large"

        connection = http.client.HTTPConnection("127.0.0.1", server.port, timeout=3)
        try:
            chunks = iter([b'{"padding":"', b'y' * (300 * 1024), b'"}'])
            connection.request(
                "POST",
                "/poll",
                body=chunks,
                headers={"Content-Type": "application/json"},
                encode_chunked=True,
            )
            response = connection.getresponse()
            response_body = response.read().decode("utf-8")
            assert response.status == 413
            assert json.loads(response_body)["error"] == "Payload too large"
        finally:
            connection.close()

        # Draining a rejected body must leave the server usable.
        status, _, body = _request(f"{server.base_url}/health")
        assert status == 200
        assert json.loads(body)["status"] == "ok"


@pytest.mark.parametrize(
    ("context_name", "expected"),
    [
        ("PRODUCT.md", True),
        ("pRoDuCt.Md", True),
        (".impeccable.md", True),
        (None, False),
    ],
)
def test_live_context_signal_matches_loader_variants(
    tmp_path: Path, context_name: str | None, expected: bool
) -> None:
    if context_name:
        (tmp_path / context_name).write_text("# Product context\n")

    with LiveServer(tmp_path) as server:
        status, _, body = _request(f"{server.base_url}/health")
        assert status == 200
        assert json.loads(body)["hasProjectContext"] is expected
