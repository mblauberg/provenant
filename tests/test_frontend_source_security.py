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

from ui_ux_live_test_support import (
    ACCEPT,
    INJECT,
    ROOT,
    SCRIPTS,
    STATUS,
    TOKEN,
    WRAP,
    write_live_config,
)
from test_frontend_live_server_security import LiveServer, _request


_write_config = write_live_config


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


def test_contained_source_rollback_preserves_a_concurrent_edit(tmp_path: Path) -> None:
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
        "{snapshot:second,content:'changed-second'}],{beforeReplace({index}){if(index===1){"
        "fs.writeFileSync(first.path,'concurrent-first');"
        "fs.writeFileSync(second.path,'concurrent-second');}}});}"
        "catch(error){process.stdout.write(JSON.stringify({code:error.code,"
        "rollbackErrors:error.rollbackErrors}));process.exit(7)}",
    )

    assert result.returncode == 7
    payload = json.loads(result.stdout)
    assert payload["code"] == "source_rollback_failed"
    assert payload["rollbackErrors"][0]["code"] == "source_rollback_conflict"
    assert first.read_text() == "concurrent-first"
    assert second.read_text() == "concurrent-second"


def test_contained_source_rolls_back_its_own_partial_write(tmp_path: Path) -> None:
    source = tmp_path / "page.html"
    source.write_text("original-bytes")
    result = _run_contained_source_probe(
        tmp_path,
        "const fs=(await import('node:fs')).default;"
        "const original=fs.writeSync.bind(fs);let calls=0;"
        "fs.writeSync=(target,data,offset,length,position)=>{"
        "if(Buffer.from(data).toString()==='replacement'){calls+=1;"
        "if(calls===1){return original(target,data,offset,3,position);}"
        "const error=new Error('partial');error.code='EIO';throw error;}"
        "return original(target,data,offset,length,position);};"
        "const snapshot=readContainedSource(process.argv[1],'page.html',{relativeOnly:true});"
        "try{replaceContainedSources([{snapshot,content:'replacement'}]);}"
        "catch(error){process.stdout.write(error.code);process.exit(7)}",
    )

    assert result.returncode == 7
    assert result.stdout == "source_replace_failed"
    assert source.read_text() == "original-bytes"


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
        + '<meta http-equiv=Content-Security-Policy '
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
