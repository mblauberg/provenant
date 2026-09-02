import io
import os
import random
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

import pytest

from scripts import public_release_check as release_check
from scripts.public_release_check import publication_range_errors, scan_paths, tracked_files


def seed_required(root: Path) -> None:
    for relative in release_check.REQUIRED:
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("safe\n")


def copy_release_scripts(repository: Path) -> Path:
    source = Path(__file__).resolve().parents[1] / "scripts"
    target = repository / "scripts"
    target.mkdir()
    for name in ("public_release_check.py", "git_evidence.py"):
        shutil.copy2(source / name, target / name)
    return target / "public_release_check.py"


def git_at(root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert result.returncode == 0, result.stderr
    return result.stdout.strip()


def publication_repository(
    tmp_path: Path, object_format: str | None = None,
) -> tuple[Path, Path, str]:
    repository = tmp_path / "publication"
    repository.mkdir(parents=True)
    init_args = ["init", "-q"]
    if object_format is not None:
        init_args.append(f"--object-format={object_format}")
    git_at(repository, *init_args)
    git_at(repository, "config", "user.name", "Release Test")
    git_at(repository, "config", "user.email", "release@example.test")
    seed_required(repository)
    script = copy_release_scripts(repository)
    git_at(repository, "add", ".")
    git_at(repository, "commit", "-q", "-m", "base")
    return repository, script, git_at(repository, "rev-parse", "HEAD")


def run_publication_range(
    repository: Path, script: Path, base: str, head: str,
    environment: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(script), "--publication-range", base, head],
        cwd=repository,
        env=None if environment is None else {**os.environ, **environment},
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def run_history(
    repository: Path, script: Path, environment: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(script), "--history"],
        cwd=repository,
        env=None if environment is None else {**os.environ, **environment},
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def tainted_selected_and_clean_decoy(
    repository: Path, base: str,
) -> tuple[str, str]:
    git_at(repository, "switch", "-q", "-c", "selected")
    receipt = repository / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text('{"contract":"delivery-run"}\n')
    (repository / "leak.txt").write_text(
        "/" + "Users/alice/private\n"
        "github" + "_pat_abcdefghijklmnopqrstuvwxyz123456\n"
    )
    git_at(repository, "add", ".agent-run/DEL-999/RUN.json", "leak.txt")
    git_at(repository, "commit", "-q", "-m", "tainted selected commit")
    tainted = git_at(repository, "rev-parse", "HEAD")

    git_at(repository, "switch", "-q", "-c", "clean-decoy", base)
    (repository / "decoy.txt").write_text("clean decoy\n")
    git_at(repository, "add", "decoy.txt")
    git_at(repository, "commit", "-q", "-m", "clean decoy commit")
    return tainted, git_at(repository, "rev-parse", "HEAD")


def hidden_linear_taint(repository: Path) -> tuple[str, str]:
    git_at(repository, "switch", "-q", "-c", "selected")
    receipt = repository / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text('{"contract":"delivery-run"}\n')
    git_at(repository, "add", ".agent-run/DEL-999/RUN.json")
    git_at(repository, "commit", "-q", "-m", "tainted middle")
    middle = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "rm", "-q", ".agent-run/DEL-999/RUN.json")
    (repository / "head.txt").write_text("clean head\n")
    git_at(repository, "add", "head.txt")
    git_at(repository, "commit", "-q", "-m", "clean selected head")
    return middle, git_at(repository, "rev-parse", "HEAD")


def hidden_merge_taint(repository: Path, base: str) -> tuple[str, str, str]:
    git_at(repository, "switch", "-q", "-c", "tainted-side")
    receipt = repository / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text('{"contract":"delivery-run"}\n')
    git_at(repository, "add", ".agent-run/DEL-999/RUN.json")
    git_at(repository, "commit", "-q", "-m", "tainted side commit")
    tainted = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "rm", "-q", ".agent-run/DEL-999/RUN.json")
    git_at(repository, "commit", "-q", "-m", "clean side tip")
    side_tip = git_at(repository, "rev-parse", "HEAD")

    git_at(repository, "switch", "-q", "-c", "selected", base)
    (repository / "main.txt").write_text("clean main\n")
    git_at(repository, "add", "main.txt")
    git_at(repository, "commit", "-q", "-m", "clean main commit")
    git_at(repository, "merge", "-q", "--no-ff", "tainted-side", "-m", "clean merge")
    return tainted, side_tip, git_at(repository, "rev-parse", "HEAD")


def loose_object_path(repository: Path, object_id: str) -> Path:
    git_directory = Path(git_at(repository, "rev-parse", "--git-dir"))
    if not git_directory.is_absolute():
        git_directory = repository / git_directory
    return git_directory / "objects" / object_id[:2] / object_id[2:]


def substitute_loose_object(repository: Path, target: str, source: str) -> None:
    target_path = loose_object_path(repository, target)
    source_path = loose_object_path(repository, source)
    assert target_path.is_file()
    assert source_path.is_file()
    target_path.chmod(0o600)
    shutil.copy2(source_path, target_path)


def write_raw_object(repository: Path, kind: str, body: bytes) -> str:
    result = subprocess.run(
        ["git", "hash-object", "-w", "-t", kind, "--stdin"],
        cwd=repository,
        input=body,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert result.returncode == 0, result.stderr.decode(errors="replace")
    return result.stdout.decode("ascii").strip()


def raw_commit_body(tree: str, parent: str, message: bytes) -> bytes:
    return (
        f"tree {tree}\nparent {parent}\n".encode("ascii")
        + b"author Release Test <release@example.test> 1700000000 +0000\n"
        + b"committer Release Test <release@example.test> 1700000000 +0000\n\n"
        + message
    )


def chunk_crossing_payload(chunk_size: int, *patterns: bytes) -> bytes:
    payload = bytearray()
    for pattern in patterns:
        padding = (chunk_size - 2 - len(payload) % chunk_size) % chunk_size
        payload.extend(b"." * padding)
        payload.extend(pattern)
        payload.extend(b"\n")
    return bytes(payload)


def streamed_content_findings(payload: bytes, chunk_size: int) -> frozenset[str]:
    scanner = release_check._BoundedContentScanner()
    position = 0
    while position < len(payload):
        chunk = payload[position:position + chunk_size]
        scanner.feed(memoryview(chunk))
        position += len(chunk)
    return scanner.finish()


def feed_consumer_in_chunks(consumer, payload: bytes, chunk_size: int) -> None:
    for position in range(0, len(payload), chunk_size):
        consumer.feed(memoryview(payload)[position:position + chunk_size])


def raw_tree_entry(name: bytes, object_id_size: int = 20) -> bytes:
    return b"100644 " + name + b"\0" + b"\0" * object_id_size


@pytest.mark.parametrize("object_format", ["sha1", "sha256"])
def test_streamed_range_scan_is_chunk_bounded_and_finds_boundary_leaks(
    tmp_path, object_format,
):
    repository, _, base = publication_repository(tmp_path, object_format)
    chunk_size = 17
    payload = chunk_crossing_payload(
        chunk_size,
        b"/" + b"Users/alice/private/",
        b"-----" + b"BEGIN OPENSSH PRIVATE KEY-----",
        b"github" + b"_pat_abcdefghijklmnopqrstuvwxyz123456",
        b"sk" + b"-abcdefghijklmnopqrstuvwxyz123456",
        b"sk" + b"-ant-abcdefghijklmnopqrstuvwxyz123456",
        b"AK" + b"IAABCDEFGHIJKLMNOP",
    )
    (repository / "boundary-leaks.bin").write_bytes(payload)
    git_at(repository, "add", "boundary-leaks.bin")
    git_at(repository, "commit", "-q", "-m", "boundary-spanning evidence")
    head = git_at(repository, "rev-parse", "HEAD")
    accounting = release_check.EvidenceMemoryAccounting()

    errors = publication_range_errors(
        base,
        head,
        repository,
        blob_chunk_size=chunk_size,
        accounting=accounting,
    )

    assert "publication range contains a personal absolute home path" in errors
    assert "publication range contains a possible private key" in errors
    assert "publication range contains a possible GitHub token" in errors
    assert "publication range contains a possible OpenAI key" in errors
    assert "publication range contains a possible Anthropic key" in errors
    assert "publication range contains a possible AWS access key" in errors
    assert accounting.max_resident_body_bytes == (
        chunk_size + release_check.BLOB_SCAN_OVERLAP
    )
    assert accounting.max_body_chunk_bytes == chunk_size
    assert accounting.max_scanner_carry_bytes == release_check.BLOB_SCAN_OVERLAP
    assert accounting.announced_body_bytes == accounting.consumed_body_bytes
    assert accounting.objects_requested == accounting.objects_completed
    assert accounting.distinct_blobs_scanned == accounting.blob_objects_completed
    assert accounting.objects_completed == sum(
        (
            accounting.blob_objects_completed,
            accounting.commit_objects_completed,
            accounting.tag_objects_completed,
            accounting.tree_objects_completed,
        )
    )
    assert (
        accounting.batch_processes_started
        == accounting.batch_processes_completed
        == 1
    )
    assert (
        accounting.batch_stderr_drainers_started
        == accounting.batch_stderr_drainers_joined
        == 1
    )
    assert accounting.retained_full_blob_bodies == 0
    assert accounting.retained_full_nonblob_bodies == 0
    assert accounting.max_flattened_trees_retained == 1
    assert accounting.non_head_flattened_entries_retained == 0
    assert accounting.max_required_head_entries_retained <= len(release_check.REQUIRED)


def test_streamed_blob_reader_reports_process_failure_before_protocol_eof(
    tmp_path, monkeypatch,
):
    endpoint = release_check.EvidenceGitEndpoint(
        tmp_path, tmp_path, tmp_path, {},
    )
    endpoint.object_format = "sha1"
    endpoint.raw_object_id_size = 20

    class FailedBatch:
        def __init__(self, *_, stderr, **__):
            self.stdin = io.BytesIO()
            self.stdout = io.BytesIO()
            assert stderr == subprocess.PIPE
            self.stderr = io.BytesIO(b"fatal: synthetic cat-file failure\n")
            self.returncode = 7

        def poll(self):
            return self.returncode

        def wait(self, timeout=None):
            return self.returncode

        def terminate(self):
            raise AssertionError("an exited child must not be terminated")

        def kill(self):
            raise AssertionError("an exited child must not be killed")

    monkeypatch.setattr(release_check.subprocess, "Popen", FailedBatch)
    accounting = release_check.EvidenceMemoryAccounting()

    with pytest.raises(RuntimeError, match="synthetic cat-file failure"):
        release_check.stream_blob_evidence(
            endpoint, {"a" * 40}, accounting=accounting,
        )
    assert (
        accounting.batch_stderr_drainers_started
        == accounting.batch_stderr_drainers_joined
        == 1
    )


@pytest.mark.parametrize(
    ("response_factory", "message"),
    [
        (lambda oid, body: f"{oid} blob {len(body)}".encode(), "truncated header"),
        (lambda oid, body: f"{oid}  blob {len(body)}\n".encode(), "invalid header"),
        (
            lambda oid, body: f"{'b' * 40} blob {len(body)}\n".encode(),
            "does not match its tree entry",
        ),
        (lambda oid, body: f"{oid} tree {len(body)}\n".encode(), "does not match its tree entry"),
        (lambda oid, body: f"{oid} missing\n".encode(), "cannot extract publication blob"),
        (
            lambda oid, body: f"{oid} blob {len(body)}\n".encode() + body[:-1],
            "output is truncated",
        ),
        (
            lambda oid, body: f"{oid} blob {len(body)}\n".encode() + body + b"x",
            "output is truncated",
        ),
        (
            lambda oid, body: f"{oid} blob {len(body)}\n".encode() + body + b"\nextra",
            "trailing data",
        ),
    ],
)
def test_streamed_blob_reader_rejects_malformed_batch_framing(
    tmp_path, monkeypatch, response_factory, message,
):
    body = b"portable body\n"
    digest = release_check.hashlib.sha1()
    digest.update(f"blob {len(body)}\0".encode("ascii"))
    digest.update(body)
    object_id = digest.hexdigest()
    response = response_factory(object_id, body)
    endpoint = release_check.EvidenceGitEndpoint(tmp_path, tmp_path, tmp_path, {})
    endpoint.object_format = "sha1"
    endpoint.raw_object_id_size = 20

    class MalformedBatch:
        def __init__(self, *_, **__):
            self.stdin = io.BytesIO()
            self.stdout = io.BytesIO(response)
            self.returncode = 0

        def poll(self):
            return self.returncode

        def wait(self, timeout=None):
            return self.returncode

        def terminate(self):
            raise AssertionError("an exited child must not be terminated")

        def kill(self):
            raise AssertionError("an exited child must not be killed")

    monkeypatch.setattr(release_check.subprocess, "Popen", MalformedBatch)

    accounting = release_check.EvidenceMemoryAccounting()
    with pytest.raises(RuntimeError, match=message):
        release_check.stream_blob_evidence(
            endpoint, {object_id}, chunk_size=3, accounting=accounting,
        )
    assert accounting.objects_completed == 0


def test_streamed_reader_accounts_for_allocated_capacity_on_tiny_body(
    tmp_path, monkeypatch,
):
    body = b"x"
    digest = release_check.hashlib.sha1()
    digest.update(b"blob 1\0x")
    object_id = digest.hexdigest()
    response = f"{object_id} blob 1\n".encode() + body + b"\n"
    endpoint = release_check.EvidenceGitEndpoint(tmp_path, tmp_path, tmp_path, {})
    endpoint.object_format = "sha1"
    endpoint.raw_object_id_size = 20

    class TinyBatch:
        def __init__(self, *_, **__):
            self.stdin = io.BytesIO()
            self.stdout = io.BytesIO(response)
            self.returncode = 0

        def poll(self):
            return self.returncode

        def wait(self, timeout=None):
            return self.returncode

        def terminate(self):
            raise AssertionError("an exited child must not be terminated")

        def kill(self):
            raise AssertionError("an exited child must not be killed")

    monkeypatch.setattr(release_check.subprocess, "Popen", TinyBatch)
    accounting = release_check.EvidenceMemoryAccounting()

    release_check.stream_blob_evidence(
        endpoint, {object_id}, chunk_size=7, accounting=accounting,
    )

    assert accounting.max_resident_body_bytes == (
        7 + release_check.BLOB_SCAN_OVERLAP
    )


@pytest.mark.parametrize("kind", ["commit", "tag", "tree"])
def test_all_nonblob_kinds_use_strict_incremental_batch_framing(
    tmp_path, monkeypatch, kind,
):
    body = b"body"
    digest = release_check.hashlib.sha1()
    digest.update(f"{kind} {len(body)}\0".encode("ascii"))
    digest.update(body)
    object_id = digest.hexdigest()
    response = f"{object_id}  {kind} {len(body)}\n".encode() + body + b"\n"
    endpoint = release_check.EvidenceGitEndpoint(tmp_path, tmp_path, tmp_path, {})
    endpoint.object_format = "sha1"
    endpoint.raw_object_id_size = 20

    class MalformedBatch:
        def __init__(self, *_, **__):
            self.stdin = io.BytesIO()
            self.stdout = io.BytesIO(response)
            self.returncode = 0

        def poll(self):
            return self.returncode

        def wait(self, timeout=None):
            return self.returncode

        def terminate(self):
            raise AssertionError("an exited child must not be terminated")

        def kill(self):
            raise AssertionError("an exited child must not be killed")

    monkeypatch.setattr(release_check.subprocess, "Popen", MalformedBatch)

    with pytest.raises(RuntimeError, match="invalid header"):
        with endpoint.object_batch(chunk_size=3):
            endpoint.verify_stored_object(
                object_id,
                expected_kind=kind,
                label=f"publication {kind} {object_id}",
            )


def test_strict_batch_reader_times_out_and_reaps_silent_child(
    tmp_path, monkeypatch,
):
    read_descriptor, write_descriptor = os.pipe()
    endpoint = release_check.EvidenceGitEndpoint(tmp_path, tmp_path, tmp_path, {})
    endpoint.object_format = "sha1"
    endpoint.raw_object_id_size = 20

    class SilentBatch:
        def __init__(self, *_, stderr, **__):
            self.stdin = io.BytesIO()
            self.stdout = os.fdopen(read_descriptor, "rb", buffering=0)
            assert stderr == subprocess.PIPE
            self.stderr = io.BytesIO(b"x" * (release_check.MAX_BATCH_STDERR_BYTES * 2))
            self.returncode = None

        def poll(self):
            return self.returncode

        def wait(self, timeout=None):
            if self.returncode is None:
                raise subprocess.TimeoutExpired("git cat-file", timeout)
            return self.returncode

        def terminate(self):
            nonlocal write_descriptor
            if write_descriptor >= 0:
                os.close(write_descriptor)
                write_descriptor = -1
            self.returncode = -15

        def kill(self):
            self.terminate()

    monkeypatch.setattr(release_check.subprocess, "Popen", SilentBatch)
    accounting = release_check.EvidenceMemoryAccounting()

    with pytest.raises(RuntimeError, match="timed out"):
        with endpoint.object_batch(
            chunk_size=3, deadline_seconds=0.01, accounting=accounting,
        ):
            endpoint.verify_stored_object("a" * 40, expected_kind="commit")

    assert endpoint._batch_reader is None
    assert accounting.max_batch_stderr_bytes_retained == (
        release_check.MAX_BATCH_STDERR_BYTES
    )
    assert accounting.batch_stderr_bytes_discarded == (
        release_check.MAX_BATCH_STDERR_BYTES
    )
    assert (
        accounting.batch_stderr_drainers_started
        == accounting.batch_stderr_drainers_joined
        == 1
    )
    if write_descriptor >= 0:
        os.close(write_descriptor)


def test_batch_stderr_is_concurrently_drained_into_fixed_retention_and_reaped(
    tmp_path, monkeypatch,
):
    retained_files = []
    child = None
    total_stderr = release_check.MAX_BATCH_STDERR_BYTES * 4

    class TrackingTemporaryFile(io.BytesIO):
        def __init__(self):
            super().__init__()
            self.max_size = 0

        def write(self, raw):
            written = super().write(raw)
            self.max_size = max(self.max_size, len(self.getbuffer()))
            return written

    def temporary_file():
        result = TrackingTemporaryFile()
        retained_files.append(result)
        return result

    class GeneratedStderr:
        def __init__(self):
            self.remaining = total_stderr
            self.closed = False

        def fileno(self):
            raise io.UnsupportedOperation

        def read(self, size=-1):
            if self.remaining == 0:
                return b""
            count = self.remaining if size < 0 else min(size, self.remaining)
            self.remaining -= count
            return b"x" * count

        def close(self):
            self.closed = True

    class StalledBatch:
        def __init__(self, *_, stderr, **__):
            nonlocal child
            self.stdin = io.BytesIO()
            self.stdout = io.BytesIO()
            self.returncode = None
            self.terminated = False
            self.killed = False
            if stderr == subprocess.PIPE:
                self.stderr = GeneratedStderr()
            else:
                stderr.write(b"x" * total_stderr)
                self.stderr = None
            child = self

        def poll(self):
            return self.returncode

        def wait(self, timeout=None):
            if self.returncode is None:
                raise subprocess.TimeoutExpired("git cat-file", timeout)
            return self.returncode

        def terminate(self):
            self.terminated = True

        def kill(self):
            self.killed = True
            self.returncode = -9

    monkeypatch.setattr(tempfile, "TemporaryFile", temporary_file)
    monkeypatch.setattr(release_check.subprocess, "Popen", StalledBatch)
    endpoint = release_check.EvidenceGitEndpoint(tmp_path, tmp_path, tmp_path, {})
    endpoint.object_format = "sha1"
    endpoint.raw_object_id_size = 20
    accounting = release_check.EvidenceMemoryAccounting()

    with pytest.raises(RuntimeError, match="truncated header"):
        with endpoint.object_batch(chunk_size=7, accounting=accounting):
            endpoint.verify_stored_object("a" * 40, expected_kind="commit")

    assert child is not None and child.terminated and child.killed
    retained = (
        retained_files[0].max_size
        if retained_files
        else getattr(accounting, "max_batch_stderr_bytes_retained", None)
    )
    assert retained is not None
    assert retained <= release_check.MAX_BATCH_STDERR_BYTES
    assert getattr(accounting, "batch_stderr_drainers_started", 0) == 1
    assert getattr(accounting, "batch_stderr_drainers_joined", 0) == 1
    assert getattr(accounting, "batch_stderr_bytes_discarded", 0) >= (
        total_stderr - release_check.MAX_BATCH_STDERR_BYTES
    )


def test_batch_reader_does_not_drain_after_keyboard_interrupt(
    tmp_path, monkeypatch,
):
    body = b"x" * 10000
    digest = release_check.hashlib.sha1()
    digest.update(f"blob {len(body)}\0".encode("ascii"))
    digest.update(body)
    object_id = digest.hexdigest()
    response = (
        f"{object_id} blob {len(body)}\n".encode("ascii") + body + b"\n"
    )
    endpoint = release_check.EvidenceGitEndpoint(tmp_path, tmp_path, tmp_path, {})
    endpoint.object_format = "sha1"
    endpoint.raw_object_id_size = 20
    batch: object | None = None

    class TrackingBytesIO(io.BytesIO):
        def close(self):
            self.position_at_close = self.tell()

    class InterruptBatch:
        def __init__(self, *_, **__):
            nonlocal batch
            self.stdin = io.BytesIO()
            self.stdout = TrackingBytesIO(response)
            self.returncode = 7
            batch = self

        def poll(self):
            return self.returncode

        def wait(self, timeout=None):
            return self.returncode

        def terminate(self):
            raise AssertionError("an exited child must not be terminated")

        def kill(self):
            raise AssertionError("an exited child must not be killed")

    class InterruptConsumer:
        def feed(self, raw):
            raise KeyboardInterrupt

        def finish(self):
            raise AssertionError("cancelled consumer must not finish")

    monkeypatch.setattr(release_check.subprocess, "Popen", InterruptBatch)

    with pytest.raises(KeyboardInterrupt):
        with endpoint.object_batch(chunk_size=7) as reader:
            reader.read_object(
                object_id,
                expected_kind="blob",
                label=f"publication blob {object_id}",
                consumer=InterruptConsumer(),
            )

    assert batch is not None
    assert batch.stdout.position_at_close < len(response)


def test_metadata_consumers_bound_duplicate_required_header_summaries():
    object_id = "a" * 40
    accounting = release_check.EvidenceMemoryAccounting()
    identity = b"Release Test <release@example.test> 1700000000 +0000"
    commit = release_check._CommitObjectConsumer(object_id, accounting)
    commit.feed(
        memoryview(
            (f"tree {object_id}\n" * 10000).encode("ascii")
            + (b"author " + identity + b"\n") * 1000
            + b"committer " + identity + b"\n\nmessage\n"
        )
    )

    with pytest.raises(RuntimeError, match="exactly one tree"):
        commit.finish()
    assert commit.tree_headers == 10000
    assert commit.author_headers == 1000
    assert len(commit.trees) == len(commit.authors) == 1

    tag = release_check._TagObjectConsumer(object_id, accounting)
    tag.feed(
        memoryview(
            (f"object {object_id}\n" * 10000).encode("ascii")
            + b"type commit\ntag bounded\n"
            + b"tagger " + identity + b"\n\nmessage\n"
        )
    )
    with pytest.raises(RuntimeError, match="malformed"):
        tag.finish()
    assert tag.counts[b"object"] == 10000
    assert len(tag.values[b"object"]) == 1
    assert accounting.max_tag_summary_bytes <= (
        len(object_id) + len("commit") + len("release@example.test")
    )


@pytest.mark.parametrize("separator", [b"\r\n", b"\r"])
def test_streamed_tag_parser_preserves_splitlines_and_oid_case(separator):
    object_id = "a" * 40
    accounting = release_check.EvidenceMemoryAccounting()
    lines = [
        f"object {object_id.upper()}".encode("ascii"),
        b"type commit",
        b"tag portable",
        b"tagger Release Test <release@example.test> 1700000000 +0000",
    ]
    tag = release_check._TagObjectConsumer(object_id, accounting)

    tag.feed(memoryview(separator.join(lines) + b"\n\nmessage\n"))

    parsed = tag.finish()
    assert parsed.target == object_id
    assert parsed.declared_type == "commit"
    assert parsed.tagger_email == "release@example.test"
    assert parsed.message_findings == frozenset()


def test_streamed_tag_parser_keeps_non_crlf_bytes_inside_values():
    object_id = "a" * 40
    accounting = release_check.EvidenceMemoryAccounting()
    tag = release_check._TagObjectConsumer(object_id, accounting)
    raw = (
        f"object {object_id}\ntype commit\n".encode("ascii")
        + b"tag portable\vvalue\n"
        + b"tagger Release\fTest <release@example.test> 1700000000 +0000\n\n"
    )

    tag.feed(memoryview(raw))

    parsed = tag.finish()
    assert parsed.target == object_id
    assert parsed.declared_type == "commit"
    assert parsed.tagger_email == "release@example.test"
    assert parsed.message_findings == frozenset()


@pytest.mark.parametrize("chunk_size", [1, 7, 17])
def test_streamed_tag_parser_rejects_oversized_tagger_email_before_retention(
    chunk_size,
):
    object_id = "a" * 40
    accounting = release_check.EvidenceMemoryAccounting()
    tag = release_check._TagObjectConsumer(object_id, accounting)
    email_limit = release_check.MAX_IDENTITY_EMAIL_BYTES
    oversized_email = (
        b"a" * (email_limit + 1 - len(b"@example.test")) + b"@example.test"
    )
    raw = (
        f"object {object_id}\ntype commit\ntag portable\n".encode("ascii")
        + b"tagger Release Test <"
        + oversized_email
        + b"> 1700000000 +0000\n\nmessage\n"
    )

    with pytest.raises(RuntimeError, match="publication tag .* is malformed"):
        feed_consumer_in_chunks(tag, raw, chunk_size)

    assert accounting.max_identity_email_bytes_retained == email_limit


@pytest.mark.parametrize("chunk_size", [1, 7, 17])
@pytest.mark.parametrize(
    ("raw", "message"),
    [
        (
            b"tree " + b"a" * 40 + b"\n"
            b"author Release Test <release@example.test> 1 +0000\n"
            b"committer Release Test <release@example.test> 1 +0000\n",
            "no message boundary",
        ),
        (
            b"tree " + b"a" * 40 + b"\xff\n"
            b"author Release Test <release@example.test> 1 +0000\n"
            b"committer Release Test <release@example.test> 1 +0000\n\nmessage\n",
            "invalid tree",
        ),
    ],
)
def test_streamed_commit_parser_preserves_malformed_input_checks_across_chunks(
    chunk_size, raw, message,
):
    consumer = release_check._CommitObjectConsumer(
        "b" * 40, release_check.EvidenceMemoryAccounting(),
    )

    with pytest.raises(RuntimeError, match=message):
        feed_consumer_in_chunks(consumer, raw, chunk_size)
        consumer.finish()


def test_streamed_commit_parser_scans_gpgsig_continuations():
    object_id = "a" * 40
    raw = (
        f"tree {object_id}\n".encode("ascii")
        + b"author Release Test <release@example.test> 1 +0000\n"
        + b"committer Release Test <release@example.test> 1 +0000\n"
        + b"gpgsig -----BEGIN PGP SIGNATURE-----\n"
        + b" github" + b"_pat_abcdefghijklmnopqrstuvwxyz123456\n"
        + b" -----END PGP SIGNATURE-----\n\n"
        + b"portable message\n"
    )
    expected = frozenset({"possible GitHub token"})

    for chunk_size in (len(raw), 1, 7, 17):
        consumer = release_check._CommitObjectConsumer(
            "b" * 40, release_check.EvidenceMemoryAccounting(),
        )
        feed_consumer_in_chunks(consumer, raw, chunk_size)

        assert consumer.finish().message_findings == expected


def test_streamed_commit_parser_preserves_ordinary_signed_commits():
    object_id = "a" * 40
    raw = (
        f"tree {object_id}\n".encode("ascii")
        + b"author Release Test <release@example.test> 1 +0000\n"
        + b"committer Release Test <release@example.test> 1 +0000\n"
        + b"gpgsig -----BEGIN PGP SIGNATURE-----\n"
        + b" \n"
        + b" iQEzBAABCgAdFiEEportable-signature-material\n"
        + b" =ABCD\n"
        + b" -----END PGP SIGNATURE-----\n\n"
        + b"portable message\n"
    )

    for chunk_size in (len(raw), 1, 7, 17):
        consumer = release_check._CommitObjectConsumer(
            "b" * 40, release_check.EvidenceMemoryAccounting(),
        )
        feed_consumer_in_chunks(consumer, raw, chunk_size)

        assert consumer.finish().message_findings == frozenset()


@pytest.mark.parametrize("chunk_size", [1, 7, 17])
@pytest.mark.parametrize(
    "malformation", ["missing", "duplicate", "invalid", "continuation"],
)
def test_streamed_tag_parser_rejects_malformed_tagger_across_chunks(
    chunk_size, malformation,
):
    object_id = "a" * 40
    prefix = f"object {object_id}\ntype commit\ntag portable\n".encode("ascii")
    identity = b"tagger Release Test <release@example.test> 1700000000 +0000\n"
    if malformation == "missing":
        raw = prefix + b"\nmessage\n"
    elif malformation == "duplicate":
        raw = prefix + identity + identity + b"\nmessage\n"
    elif malformation == "invalid":
        raw = (
            prefix
            + b"tagger Release Test release@example.test 1700000000 +0000\n"
            + b"\nmessage\n"
        )
    else:
        raw = prefix + identity + b" alice@gmail.com\n\nmessage\n"
    consumer = release_check._TagObjectConsumer(
        object_id, release_check.EvidenceMemoryAccounting(),
    )

    with pytest.raises(RuntimeError, match="publication tag .* is malformed"):
        feed_consumer_in_chunks(consumer, raw, chunk_size)
        consumer.finish()


def test_streamed_tag_parser_scans_nonrequired_header_continuations():
    object_id = "a" * 40
    raw = (
        f"object {object_id}\ntype commit\ntag portable\n".encode("ascii")
        + b"x-release-note portable\n"
        + b" /" + b"Users/alice/private/\n"
        + b"tagger Release Test <release@example.test> 1700000000 +0000\n\n"
        + b"portable message\n"
    )
    expected = frozenset({"personal absolute home path"})

    for chunk_size in (len(raw), 1, 7, 17):
        consumer = release_check._TagObjectConsumer(
            object_id, release_check.EvidenceMemoryAccounting(),
        )
        feed_consumer_in_chunks(consumer, raw, chunk_size)

        assert consumer.finish().message_findings == expected


@pytest.mark.parametrize("chunk_size", [1, 7, 17])
def test_streaming_content_detector_is_equivalent_to_policy_regexes(chunk_size):
    cases = [
        b"/" + b"Users/alice/private/",
        b"/" + b"Users/alice",
        b"/" + b"Users/a+b/",
        b"/" + b"home/" + b"a" * (chunk_size * 3) + b"/",
        b"-----" + b"BEGIN OPENSSH PRIVATE KEY-----",
        b"ghp_" + b"a" * 20,
        b"github" + b"_pat_" + b"a" * (chunk_size * 3 + 20),
        b"xgithub" + b"_pat_" + b"a" * 20,
        b"sk" + b"-" + b"a" * 20,
        b"sk" + b"-" + b"-" * 20,
        b"sk" + b"-" + b"a" * 20 + b"-",
        b"sk" + b"-ant-" + b"a" * 20,
        b"sk" + b"-ant-" + b"a" * 20 + b"-",
        b"AK" + b"IA" + b"A" * 16,
        b"AK" + b"IA" + b"A" * 17,
    ]
    for payload in cases:
        padded = b"." * (chunk_size - 1) + payload
        assert streamed_content_findings(
            padded, chunk_size,
        ) == release_check._whole_content_findings(padded)


def test_streaming_content_detector_matches_embedded_prefix_restart_property():
    seed = 0xD030
    generator = random.Random(seed)
    restart_prefixes = (b"-sk-", b"-sk-ant-")
    alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-"
    terminal_regressions = (
        b"sk-" + b"A" * 20 + b"-sk-" + b"B" * 13,
        b"sk-" + b"A" * 20 + b"-sk-ant-" + b"C" * 13,
    )
    for payload in terminal_regressions:
        assert streamed_content_findings(
            payload, len(payload),
        ) == release_check._whole_content_findings(payload)
    for case in range(2000):
        leading = bytes(generator.choice(b".! ") for _ in range(generator.randrange(4)))
        first_run = bytes(
            generator.choice(alphabet)
            for _ in range(generator.randrange(20, 80))
        )
        restart = generator.choice(restart_prefixes)
        short_tail = bytes(
            generator.choice(alphabet)
            for _ in range(generator.randrange(0, 20))
        )
        payload = leading + b"sk-" + first_run + restart + short_tail
        scanner = release_check._BoundedContentScanner()
        position = 0
        while position < len(payload):
            width = generator.randrange(1, 24)
            scanner.feed(memoryview(payload)[position:position + width])
            position += width
        assert scanner.finish() == release_check._whole_content_findings(payload), (
            seed, case, payload
        )


def test_streaming_range_and_default_gate_agree_on_terminal_prefix_restart(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    payload = b"sk-" + b"A" * 20 + b"-sk-ant-" + b"C" * 13
    (repository / "leak.txt").write_bytes(payload)
    git_at(repository, "add", "leak.txt")
    git_at(repository, "commit", "-q", "-m", "terminal prefix restart")
    head = git_at(repository, "rev-parse", "HEAD")

    default = subprocess.run(
        [sys.executable, str(script)], cwd=repository, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
    )
    selected = run_publication_range(repository, script, base, head)

    assert default.returncode == 1
    assert "possible OpenAI key: leak.txt" in default.stderr
    assert selected.returncode == 1
    assert "publication range contains a possible OpenAI key" in selected.stderr


def test_clean_scanner_work_is_chunk_bounded_not_byte_walked():
    chunk_size = 64 * 1024
    payload = b"x" * (5 * 1024 * 1024)
    scanner = release_check._BoundedContentScanner()

    for position in range(0, len(payload), chunk_size):
        scanner.feed(memoryview(payload)[position:position + chunk_size])

    assert scanner.finish() == frozenset()
    assert scanner.chunks_scanned == len(payload) // chunk_size
    assert scanner.active_candidate_chunks == 0


def test_large_commit_and_historical_tree_are_streamed_without_nonhead_flattening(
    tmp_path,
):
    repository, _, base = publication_repository(tmp_path)
    base_tree = git_at(repository, "rev-parse", f"{base}^{{tree}}")
    safe_blob = git_at(repository, "rev-parse", f"{base}:README.md")
    raw_tree = b"".join(
        f"100644 file-{index:05d}\0".encode("ascii")
        + bytes.fromhex(safe_blob)
        for index in range(12000)
    )
    historical_tree = write_raw_object(repository, "tree", raw_tree)
    large_message = (
        b"x" * (release_check.MAX_PUBLIC_FILE_BYTES + 1)
        + b"\ngithub" + b"_pat_abcdefghijklmnopqrstuvwxyz123456\n"
    )
    historical_commit = write_raw_object(
        repository,
        "commit",
        raw_commit_body(historical_tree, base, large_message),
    )
    clean_head = write_raw_object(
        repository,
        "commit",
        raw_commit_body(base_tree, historical_commit, b"clean selected head\n"),
    )
    accounting = release_check.EvidenceMemoryAccounting()

    errors = publication_range_errors(
        base,
        clean_head,
        repository,
        blob_chunk_size=1024,
        accounting=accounting,
    )

    assert "publication range commit message contains a possible GitHub token" in errors
    assert accounting.non_head_flattened_entries_retained == 0
    assert accounting.max_flattened_trees_retained == 1
    assert accounting.head_flattened_entries_retained > 0
    assert accounting.tree_objects_completed >= 2
    assert accounting.commit_objects_completed >= 3
    assert accounting.retained_full_nonblob_bodies == 0
    assert accounting.max_resident_body_bytes == (
        1024 + release_check.BLOB_SCAN_OVERLAP
    )
    assert accounting.max_tree_entry_bytes < 128
    assert accounting.announced_body_bytes == accounting.consumed_body_bytes
    assert accounting.objects_requested == accounting.objects_completed


def test_streamed_tree_parser_retains_only_two_portable_components():
    component_limit = 255
    accounting = release_check.EvidenceMemoryAccounting()
    visited = 0

    def visit(_item):
        nonlocal visited
        visited += 1

    consumer = release_check._TreeObjectConsumer(20, visit, accounting)
    for index in range(10000):
        prefix = f"{index:05d}-".encode("ascii")
        name = prefix + b"x" * (component_limit - len(prefix))
        consumer.feed(memoryview(raw_tree_entry(name)))
    consumer.finish()

    assert visited == 10000
    assert not hasattr(consumer, "names")
    assert accounting.max_tree_object_summary_bytes <= 2 * component_limit


@pytest.mark.parametrize(
    "raw",
    [
        raw_tree_entry(b"x" * 256),
        raw_tree_entry(b"second") + raw_tree_entry(b"first"),
        raw_tree_entry(b"duplicate") + raw_tree_entry(b"duplicate"),
    ],
    ids=["oversized-component", "noncanonical-order", "duplicate"],
)
def test_streamed_tree_parser_rejects_nonportable_or_noncanonical_names(raw):
    accounting = release_check.EvidenceMemoryAccounting()
    consumer = release_check._TreeObjectConsumer(20, lambda _item: None, accounting)

    with pytest.raises(RuntimeError, match="publication tree contains an invalid entry"):
        consumer.feed(memoryview(raw))

    assert accounting.max_tree_object_summary_bytes <= 510


def test_large_annotated_tag_is_streamed_and_discarded(tmp_path):
    repository, _, base = publication_repository(tmp_path)
    tag_body = (
        f"object {base}\ntype commit\ntag audit-large\n".encode("ascii")
        + b"tagger Release Test <release@example.test> 1700000000 +0000\n\n"
        + b"x" * (release_check.MAX_PUBLIC_FILE_BYTES + 1)
    )
    tag = write_raw_object(repository, "tag", tag_body)
    git_at(repository, "update-ref", "refs/tags/audit-large", tag)
    accounting = release_check.EvidenceMemoryAccounting()

    errors = release_check.history_errors(
        repository, blob_chunk_size=1024, accounting=accounting,
    )

    assert errors == []
    assert accounting.tag_objects_completed == 1
    assert accounting.retained_full_nonblob_bodies == 0
    assert accounting.max_resident_body_bytes == (
        1024 + release_check.BLOB_SCAN_OVERLAP
    )
    assert accounting.announced_body_bytes == accounting.consumed_body_bytes
    assert accounting.objects_requested == accounting.objects_completed


@pytest.mark.parametrize("object_format", ["sha1", "sha256"])
def test_history_rejects_personal_annotated_tag_tagger_email(
    tmp_path, object_format,
):
    repository, _, base = publication_repository(tmp_path, object_format)
    tag = write_raw_object(
        repository,
        "tag",
        (
            f"object {base}\ntype commit\ntag personal-tagger\n".encode("ascii")
            + b"tagger Release Test <alice@gmail.com> 1700000000 +0000\n\n"
            + b"portable tag message\n"
        ),
    )
    git_at(repository, "update-ref", "refs/tags/personal-tagger", tag)

    errors = release_check.history_errors(repository, blob_chunk_size=17)

    assert "reachable history exposes a personal email: alice@gmail.com" in errors


@pytest.mark.parametrize("object_format", ["sha1", "sha256"])
def test_history_stream_scans_annotated_tag_messages(
    tmp_path, object_format,
):
    repository, _, base = publication_repository(tmp_path, object_format)
    chunk_size = 17
    message = chunk_crossing_payload(
        chunk_size,
        b"/" + b"Users/alice/private/",
        b"github" + b"_pat_abcdefghijklmnopqrstuvwxyz123456",
    )
    tag = write_raw_object(
        repository,
        "tag",
        (
            f"object {base}\ntype commit\ntag message-evidence\n".encode("ascii")
            + b"tagger Release Test <release@example.test> 1700000000 +0000\n\n"
            + message
        ),
    )
    git_at(repository, "update-ref", "refs/tags/message-evidence", tag)
    accounting = release_check.EvidenceMemoryAccounting()

    errors = release_check.history_errors(
        repository, blob_chunk_size=chunk_size, accounting=accounting,
    )

    assert (
        "reachable history annotated tag message contains a "
        "personal absolute home path"
    ) in errors
    assert (
        "reachable history annotated tag message contains a possible GitHub token"
    ) in errors
    assert accounting.retained_full_nonblob_bodies == 0
    assert accounting.max_body_chunk_bytes == chunk_size
    assert accounting.announced_body_bytes == accounting.consumed_body_bytes
    assert accounting.objects_requested == accounting.objects_completed


@pytest.mark.parametrize("object_format", ["sha1", "sha256"])
def test_oversized_historical_blob_is_streamed_and_head_size_uses_header(
    tmp_path, object_format,
):
    repository, _, base = publication_repository(tmp_path, object_format)
    large = repository / "historical-large.bin"
    large.write_bytes(
        b"x" * (release_check.MAX_PUBLIC_FILE_BYTES + 1)
        + b"\ngithub" + b"_pat_abcdefghijklmnopqrstuvwxyz123456\n"
    )
    git_at(repository, "add", large.name)
    git_at(repository, "commit", "-q", "-m", "oversized historical body")
    git_at(repository, "rm", "-q", large.name)
    git_at(repository, "commit", "-q", "-m", "remove oversized body")
    clean_head = git_at(repository, "rev-parse", "HEAD")

    range_accounting = release_check.EvidenceMemoryAccounting()
    range_errors = publication_range_errors(
        base,
        clean_head,
        repository,
        blob_chunk_size=1024,
        accounting=range_accounting,
    )
    history_accounting = release_check.EvidenceMemoryAccounting()
    history_errors = release_check.history_errors(
        repository,
        blob_chunk_size=1024,
        accounting=history_accounting,
    )

    assert "publication range contains a possible GitHub token" in range_errors
    assert "reachable history contains a possible GitHub token" in history_errors
    for accounting in (range_accounting, history_accounting):
        assert accounting.oversized_historical_blobs == 1
        assert accounting.oversized_head_blobs == 0
        assert accounting.max_resident_body_bytes <= (
            1024 + release_check.BLOB_SCAN_OVERLAP
        )
        assert accounting.announced_body_bytes == accounting.consumed_body_bytes
        assert accounting.objects_requested == accounting.objects_completed
        assert (
            accounting.batch_processes_started
            == accounting.batch_processes_completed
            == 1
        )
        assert accounting.retained_full_blob_bodies == 0
        assert accounting.retained_full_nonblob_bodies == 0
        assert accounting.max_flattened_trees_retained == 1
        assert accounting.non_head_flattened_entries_retained == 0
        assert accounting.max_required_head_entries_retained <= len(
            release_check.REQUIRED
        )
        assert accounting.distinct_root_trees_walked == 2
        assert accounting._flattened_trees_retained == 0

    large.write_bytes(
        b"x" * (release_check.MAX_PUBLIC_FILE_BYTES + 1)
        + b"\ngithub" + b"_pat_abcdefghijklmnopqrstuvwxyz123456\n"
    )
    git_at(repository, "add", large.name)
    git_at(repository, "commit", "-q", "-m", "oversized selected head")
    oversized_head = git_at(repository, "rev-parse", "HEAD")
    head_accounting = release_check.EvidenceMemoryAccounting()

    head_errors = publication_range_errors(
        clean_head,
        oversized_head,
        repository,
        blob_chunk_size=1024,
        accounting=head_accounting,
    )

    assert (
        "publication HEAD tracked file exceeds 5 MiB: historical-large.bin"
        in head_errors
    )
    assert "publication range contains a possible GitHub token" in head_errors
    assert head_accounting.oversized_head_blobs == 1
    assert head_accounting.max_resident_body_bytes <= (
        1024 + release_check.BLOB_SCAN_OVERLAP
    )


def test_public_scan_rejects_private_paths_secrets_and_unlicensed_skill(tmp_path):
    seed_required(tmp_path)
    private = tmp_path / "notes.md"
    private.write_text("/" + "Users/alice/secret/file\n")
    token = tmp_path / "token.txt"
    token.write_text("github" + "_pat_abcdefghijklmnopqrstuvwxyz123456\n")
    errors = scan_paths(
        [
            *release_check.REQUIRED,
            "notes.md",
            "token.txt",
            "skills/academic-writing/SKILL.md",
            "skills/clean-writing/SKILL.md",
            "skills/humanise-text/SKILL.md",
            "skills/tanstack-query-best-practices/SKILL.md",
            "skills/vercel-react-best-practices/SKILL.md",
            "skills/playwright/SKILL.md",
            "skills/react-performance/SKILL.md",
            "skills/tanstack-query/SKILL.md",
            "skills/typescript-clean-code/SKILL.md",
            "skills/uml-diagrams/SKILL.md",
            "skills/web-stack-conventions/SKILL.md",
        ],
        tmp_path,
    )
    assert any("personal absolute home path" in error for error in errors)
    assert any("possible GitHub token" in error for error in errors)
    assert sum("forbidden tracked path" in error for error in errors) == 11


def test_public_scan_accepts_portable_text_tree(tmp_path):
    seed_required(tmp_path)
    (tmp_path / "safe.md").write_text("Use ${PROJECT_ROOT:-$PWD}.\n")
    assert scan_paths([*release_check.REQUIRED, "safe.md"], tmp_path) == []


def test_public_scan_rejects_untracked_required_legal_file(tmp_path):
    seed_required(tmp_path)
    tracked = release_check.REQUIRED - {"NOTICE"}

    assert scan_paths(list(tracked), tmp_path) == [
        "required public file is not tracked: NOTICE"
    ]


def test_public_scan_rejects_delivery_receipt_when_tracked(tmp_path):
    seed_required(tmp_path)
    receipt = tmp_path / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text(
        '{"schema_version":1,"contract":"delivery-run","run_id":"DEL-999"}\n'
    )

    assert scan_paths([
        *release_check.REQUIRED,
        ".agent-run/DEL-999/RUN.json",
    ], tmp_path) == [
        "forbidden tracked path: .agent-run/DEL-999/RUN.json"
    ]


def test_repository_tracks_no_private_agent_runs():
    assert [path for path in tracked_files() if path.startswith(".agent-run/")] == []


def test_publication_range_ignores_tainted_sibling_but_scans_selected_commits(tmp_path):
    repository = tmp_path / "publication"
    repository.mkdir()
    git_at(repository, "init", "-q")
    git_at(repository, "config", "user.name", "Release Test")
    git_at(repository, "config", "user.email", "release@example.test")
    seed_required(repository)
    script = copy_release_scripts(repository)
    git_at(repository, "add", ".")
    git_at(repository, "commit", "-q", "-m", "base")
    base = git_at(repository, "rev-parse", "HEAD")

    (repository / "safe.md").write_text("portable\n")
    git_at(repository, "add", "safe.md")
    git_at(repository, "commit", "-q", "-m", "clean publication change")
    clean_head = git_at(repository, "rev-parse", "HEAD")

    git_at(repository, "switch", "-q", "-c", "tainted", base)
    git_at(repository, "config", "user.email", "alice@gmail.com")
    (repository / "leak.txt").write_text(
        "/" + "Users/alice/private\n"
        "github" + "_pat_abcdefghijklmnopqrstuvwxyz123456\n"
    )
    git_at(repository, "add", "leak.txt")
    git_at(repository, "commit", "-q", "-m", "tainted sibling")
    tainted_head = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "switch", "-q", "--detach", clean_head)

    clean = subprocess.run(
        [sys.executable, str(script), "--publication-range", base, clean_head],
        cwd=repository,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert clean.returncode == 0, clean.stderr
    assert "publication range" in clean.stdout

    tainted = subprocess.run(
        [sys.executable, str(script), "--publication-range", base, tainted_head],
        cwd=repository,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert tainted.returncode == 1
    assert "publication range contains a personal absolute home path" in tainted.stderr
    assert "publication range contains a possible GitHub token" in tainted.stderr
    assert "publication range exposes a personal email: alice@gmail.com" in tainted.stderr

    assert publication_range_errors(clean_head, clean_head, repository) == [
        "publication range must contain at least one commit"
    ]
    assert publication_range_errors(clean_head, tainted_head, repository) == [
        "publication range base is not an ancestor of head"
    ]
    invalid = publication_range_errors("--all", clean_head, repository)
    assert len(invalid) == 1
    assert invalid[0].startswith("cannot resolve publication endpoint '--all':")

    all_refs = subprocess.run(
        [sys.executable, str(script), "--history"],
        cwd=repository,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert all_refs.returncode == 1
    assert "reachable history contains a personal absolute home path" in all_refs.stderr
    assert "reachable history exposes a personal email: alice@gmail.com" in all_refs.stderr


def test_publication_gates_reject_a_clean_author_with_a_personal_committer(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    (repository / "safe.md").write_text("portable\n")
    git_at(repository, "add", "safe.md")
    git_at(
        repository,
        "-c", "user.email=alice@gmail.com",
        "commit", "-q", "-m", "replayed publication commit",
        "--author=Contained Candidate <candidate@example.invalid>",
    )
    head = git_at(repository, "rev-parse", "HEAD")
    assert git_at(repository, "show", "-s", "--format=%ae %ce", head) == (
        "candidate@example.invalid alice@gmail.com"
    )

    selected = run_publication_range(repository, script, base, head)

    assert selected.returncode == 1, selected.stdout
    assert (
        "publication range exposes a personal email: alice@gmail.com"
        in selected.stderr
    )

    history = run_history(repository, script)

    assert history.returncode == 1, history.stdout
    assert (
        "reachable history exposes a personal email: alice@gmail.com"
        in history.stderr
    )

def test_publication_range_ignores_tainted_checked_out_sibling(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    git_at(repository, "switch", "-q", "-c", "selected")
    (repository / "safe.md").write_text("portable selected change\n")
    git_at(repository, "add", "safe.md")
    git_at(repository, "commit", "-q", "-m", "clean selected change")
    selected_head = git_at(repository, "rev-parse", "HEAD")

    git_at(repository, "switch", "-q", "-c", "tainted-checkout", base)
    receipt = repository / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text('{"contract":"delivery-run"}\n')
    (repository / "leak.txt").write_text(
        "/" + "Users/alice/private\n"
        "github" + "_pat_abcdefghijklmnopqrstuvwxyz123456\n"
    )
    (repository / "LICENSE").unlink()
    git_at(repository, "add", "-A")
    git_at(repository, "commit", "-q", "-m", "tainted checked-out sibling")

    result = run_publication_range(repository, script, base, selected_head)

    assert result.returncode == 0, result.stderr
    assert result.stdout == "PASS: publication range clean\n"


def test_raw_range_and_history_ignore_default_replacement_objects(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    tainted, clean = tainted_selected_and_clean_decoy(repository, base)
    git_at(repository, "replace", tainted, clean)

    selected = run_publication_range(repository, script, base, tainted)
    history = run_history(repository, script)

    assert selected.returncode == 1
    assert (
        "publication HEAD forbidden tracked path: .agent-run/DEL-999/RUN.json"
    ) in selected.stderr
    assert history.returncode == 1
    assert (
        "reachable history contains a forbidden tracked path: "
        ".agent-run/DEL-999/RUN.json"
    ) in history.stderr


def test_raw_range_and_history_ignore_alternate_replacement_namespace(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    tainted, clean = tainted_selected_and_clean_decoy(repository, base)
    git_at(repository, "update-ref", f"refs/custom-replace/{tainted}", clean)
    environment = {"GIT_REPLACE_REF_BASE": "refs/custom-replace/"}

    selected = run_publication_range(repository, script, base, tainted, environment)
    history = run_history(repository, script, environment)

    assert selected.returncode == 1
    assert (
        "publication HEAD forbidden tracked path: .agent-run/DEL-999/RUN.json"
    ) in selected.stderr
    assert history.returncode == 1
    assert (
        "reachable history contains a forbidden tracked path: "
        ".agent-run/DEL-999/RUN.json"
    ) in history.stderr


def test_range_and_history_hash_bind_consumed_commit_bytes(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    tainted, clean = tainted_selected_and_clean_decoy(repository, base)
    substitute_loose_object(repository, tainted, clean)

    selected = run_publication_range(repository, script, base, tainted)
    history = run_history(repository, script)

    assert selected.returncode == 1
    assert tainted in selected.stderr
    assert "object identity does not match sha1" in selected.stderr
    assert history.returncode == 1
    assert tainted in history.stderr
    assert "object identity does not match sha1" in history.stderr


def test_range_and_history_hash_bind_consumed_tree_bytes(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    tainted, clean = tainted_selected_and_clean_decoy(repository, base)
    tainted_tree = git_at(repository, "rev-parse", f"{tainted}^{{tree}}")
    clean_tree = git_at(repository, "rev-parse", f"{clean}^{{tree}}")
    substitute_loose_object(repository, tainted_tree, clean_tree)

    selected = run_publication_range(repository, script, base, tainted)
    history = run_history(repository, script)

    expected = f"publication tree {tainted_tree} object identity does not match sha1"
    assert selected.returncode == 1
    assert expected in selected.stderr
    assert history.returncode == 1
    assert expected in history.stderr


@pytest.mark.parametrize("object_format", ["sha1", "sha256"])
def test_range_and_history_hash_bind_consumed_blob_bytes(tmp_path, object_format):
    repository, _, base = publication_repository(tmp_path, object_format)
    git_at(repository, "switch", "-q", "-c", "selected")
    leak = repository / "leak.txt"
    leak.write_text(
        "/" + "Users/alice/private\n"
        "github" + "_pat_abcdefghijklmnopqrstuvwxyz123456\n"
    )
    git_at(repository, "add", "leak.txt")
    git_at(repository, "commit", "-q", "-m", "selected content")
    head = git_at(repository, "rev-parse", "HEAD")
    tainted_blob = git_at(repository, "rev-parse", f"{head}:leak.txt")
    clean_blob = subprocess.run(
        ["git", "hash-object", "-w", "--stdin"], cwd=repository,
        input=b"portable content\n", stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, check=True,
    ).stdout.decode("ascii").strip()
    substitute_loose_object(repository, tainted_blob, clean_blob)

    selected = publication_range_errors(
        base, head, repository, blob_chunk_size=3,
    )
    history = release_check.history_errors(
        repository, blob_chunk_size=3,
    )

    expected = (
        f"publication blob {tainted_blob} object identity does not match "
        f"{object_format}"
    )
    assert selected == [expected]
    assert history == [expected]


def test_history_hash_binds_consumed_annotated_tag_bytes(tmp_path):
    repository, script, _ = publication_repository(tmp_path)
    git_at(repository, "tag", "-a", "audit-tag", "-m", "audit tag")
    git_at(repository, "tag", "-a", "clean-decoy-tag", "-m", "clean decoy tag")
    audit_tag = git_at(repository, "rev-parse", "refs/tags/audit-tag")
    clean_tag = git_at(repository, "rev-parse", "refs/tags/clean-decoy-tag")
    substitute_loose_object(repository, audit_tag, clean_tag)

    history = run_history(repository, script)

    assert history.returncode == 1
    assert audit_tag in history.stderr
    assert "object identity does not match sha1" in history.stderr


def test_history_hash_binds_cross_type_raw_ref_objects(tmp_path):
    repository, script, _ = publication_repository(tmp_path)
    git_at(repository, "tag", "-a", "audit-tag", "-m", "audit tag")
    audit_tag = git_at(repository, "rev-parse", "refs/tags/audit-tag")
    blob = subprocess.run(
        ["git", "hash-object", "-w", "--stdin"], cwd=repository,
        input=b"cross-type decoy\n", stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, check=True,
    ).stdout.decode("ascii").strip()
    substitute_loose_object(repository, audit_tag, blob)

    history = run_history(repository, script)

    expected = f"publication object {audit_tag} object identity does not match sha1"
    assert history.returncode == 1
    assert expected in history.stderr


@pytest.mark.parametrize("kind", ["blob", "tree"])
def test_history_hash_binds_noncommit_raw_ref_bytes(tmp_path, kind):
    repository, script, _ = publication_repository(tmp_path)
    first_blob = subprocess.run(
        ["git", "hash-object", "-w", "--stdin"], cwd=repository,
        input=b"first raw ref object\n", stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, check=True,
    ).stdout.decode("ascii").strip()
    second_blob = subprocess.run(
        ["git", "hash-object", "-w", "--stdin"], cwd=repository,
        input=b"second raw ref object\n", stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, check=True,
    ).stdout.decode("ascii").strip()
    if kind == "blob":
        target, decoy = first_blob, second_blob
    else:
        target = subprocess.run(
            ["git", "mktree"], cwd=repository,
            input=f"100644 blob {first_blob}\tfirst.txt\n".encode("ascii"),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True,
        ).stdout.decode("ascii").strip()
        decoy = subprocess.run(
            ["git", "mktree"], cwd=repository,
            input=f"100644 blob {second_blob}\tsecond.txt\n".encode("ascii"),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True,
        ).stdout.decode("ascii").strip()
    git_at(repository, "update-ref", f"refs/evidence/{kind}", target)
    substitute_loose_object(repository, target, decoy)

    history = run_history(repository, script)

    assert history.returncode == 1
    assert target in history.stderr
    assert "object identity does not match sha1" in history.stderr


@pytest.mark.parametrize("object_format", ["sha1", "sha256"])
@pytest.mark.parametrize("ref_kind", ["direct", "annotated-tag"])
def test_history_walks_and_scans_tree_valued_reference_roots(
    tmp_path, object_format, ref_kind,
):
    repository, _, base = publication_repository(tmp_path, object_format)
    secret_blob = write_raw_object(
        repository,
        "blob",
        b"github" + b"_pat_abcdefghijklmnopqrstuvwxyz123456\n",
    )

    def tree(record: str) -> str:
        result = subprocess.run(
            ["git", "mktree"],
            cwd=repository,
            input=(record + "\n").encode("ascii"),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        assert result.returncode == 0, result.stderr.decode(errors="replace")
        return result.stdout.decode("ascii").strip()

    run_tree = tree(f"100644 blob {secret_blob}\tRUN.json")
    delivery_tree = tree(f"040000 tree {run_tree}\tDEL-999")
    root_tree = tree(f"040000 tree {delivery_tree}\t.agent-run")
    target = root_tree
    if ref_kind == "annotated-tag":
        target = write_raw_object(
            repository,
            "tag",
            (
                f"object {root_tree}\ntype tree\ntag audit-tree\n".encode("ascii")
                + b"tagger Release Test <release@example.test> "
                + b"1700000000 +0000\n\naudit tree\n"
            ),
        )
    git_at(repository, "update-ref", f"refs/meta/{ref_kind}", target)
    accounting = release_check.EvidenceMemoryAccounting()

    errors = release_check.history_errors(
        repository, blob_chunk_size=17, accounting=accounting,
    )

    assert (
        "reachable history contains a forbidden tracked path: "
        ".agent-run/DEL-999/RUN.json"
    ) in errors
    assert "reachable history contains a possible GitHub token" in errors
    assert accounting.non_head_flattened_entries_retained == 0
    assert accounting.max_flattened_trees_retained == 1
    assert accounting.distinct_root_trees_walked >= 2
    assert accounting.announced_body_bytes == accounting.consumed_body_bytes
    assert accounting.objects_requested == accounting.objects_completed


@pytest.mark.parametrize("object_format", ["sha1", "sha256"])
@pytest.mark.parametrize("ref_kind", ["direct", "annotated-tag"])
def test_history_scans_blob_valued_reference_roots(
    tmp_path, object_format, ref_kind,
):
    repository, _, _ = publication_repository(tmp_path, object_format)
    secret_blob = write_raw_object(
        repository,
        "blob",
        b"github" + b"_pat_abcdefghijklmnopqrstuvwxyz123456\n",
    )
    target = secret_blob
    if ref_kind == "annotated-tag":
        target = write_raw_object(
            repository,
            "tag",
            (
                f"object {secret_blob}\ntype blob\ntag audit-blob\n".encode("ascii")
                + b"tagger Release Test <release@example.test> "
                + b"1700000000 +0000\n\naudit blob\n"
            ),
        )
    git_at(repository, "update-ref", f"refs/meta/{ref_kind}-blob", target)

    errors = release_check.history_errors(repository, blob_chunk_size=17)

    assert "reachable history contains a possible GitHub token" in errors


def test_deep_tree_walk_retains_no_prefixes_on_exit_frames():
    depth = 500
    accounting = release_check.EvidenceMemoryAccounting()

    class Reader:
        def __init__(self):
            self.accounting = accounting

    class Endpoint:
        def __init__(self):
            self.reader = Reader()

        def _reader(self):
            return self.reader

        def visit_tree(self, object_id, visitor):
            index = int(object_id)
            if index < depth:
                visitor(
                    release_check.RawTreeItem(
                        mode="40000",
                        kind="tree",
                        object_id=str(index + 1),
                        name="d",
                    )
                )
            else:
                visitor(
                    release_check.RawTreeItem(
                        mode="100644",
                        kind="blob",
                        object_id="b",
                        name="leaf",
                    )
                )

    entries = []
    release_check.visit_raw_tree_entries(Endpoint(), "0", entries.append)

    assert len(entries) == 1
    assert entries[0][4].count("/") == depth
    assert accounting.max_tree_path_nodes_retained <= depth
    assert accounting.max_tree_path_component_bytes_retained <= depth
    assert accounting.max_materialized_tree_path_bytes <= 2 * depth + len("leaf")


def test_historical_tree_walk_deduplicates_shared_oid_path_summaries():
    accounting = release_check.EvidenceMemoryAccounting()
    visits: dict[str, int] = {}

    class Reader:
        def __init__(self):
            self.accounting = accounting

    class Endpoint:
        def __init__(self):
            self.reader = Reader()

        def _reader(self):
            return self.reader

        def visit_tree(self, object_id, visitor):
            visits[object_id] = visits.get(object_id, 0) + 1
            if object_id == "shared":
                visitor(
                    release_check.RawTreeItem(
                        mode="100644",
                        kind="blob",
                        object_id="blob",
                        name="file.txt",
                    )
                )
            else:
                visitor(
                    release_check.RawTreeItem(
                        mode="40000",
                        kind="tree",
                        object_id="shared",
                        name="stable",
                    )
                )

    roots = [f"root-{index}" for index in range(20)]
    head_tree = "head"
    commits = {
        tree: release_check.RawCommit(
            object_id=tree,
            tree=tree,
            parents=(),
            message_findings=frozenset(),
            author_email="release@example.test",
            committer_email="release@example.test",
        )
        for tree in [*roots, head_tree]
    }

    material = release_check.collect_tree_material(
        Endpoint(), commits, head_tree, accounting,
    )
    accounting.release_flattened_tree()

    assert visits["shared"] == 2
    assert len(material.head_entries) == 1
    assert accounting.retained_tree_context_summaries <= len(roots) + 1
    assert accounting.historical_tree_contexts_released_before_head


def test_range_and_history_reject_native_grafts(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    _, head = hidden_linear_taint(repository)
    graft_name = git_at(repository, "rev-parse", "--git-path", "info/grafts")
    graft_file = Path(graft_name)
    if not graft_file.is_absolute():
        graft_file = repository / graft_file
    graft_file.parent.mkdir(parents=True, exist_ok=True)
    graft_file.write_text(f"{head} {base}\n")

    selected = run_publication_range(repository, script, base, head)
    history = run_history(repository, script)

    assert selected.returncode == 1
    assert selected.stderr == "FAIL: publication evidence rejected: nonempty repository grafts\n"
    assert history.returncode == 1
    assert history.stderr == "FAIL: publication evidence rejected: nonempty repository grafts\n"


def test_raw_range_and_history_ignore_inherited_graft_file(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    _, head = hidden_linear_taint(repository)
    graft_file = tmp_path / "inherited-grafts"
    graft_file.write_text(f"{head} {base}\n")
    environment = {"GIT_GRAFT_FILE": str(graft_file)}

    selected = run_publication_range(repository, script, base, head, environment)
    history = run_history(repository, script, environment)

    assert selected.returncode == 1
    assert (
        "publication range contains a forbidden tracked path: "
        ".agent-run/DEL-999/RUN.json"
    ) in selected.stderr
    assert history.returncode == 1
    assert (
        "reachable history contains a forbidden tracked path: "
        ".agent-run/DEL-999/RUN.json"
    ) in history.stderr


def test_range_and_history_reject_native_shallow_history(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    _, side_tip, head = hidden_merge_taint(repository, base)
    shallow_name = git_at(repository, "rev-parse", "--git-path", "shallow")
    shallow_file = Path(shallow_name)
    if not shallow_file.is_absolute():
        shallow_file = repository / shallow_file
    shallow_file.write_text(f"{side_tip}\n")

    selected = run_publication_range(repository, script, base, head)
    history = run_history(repository, script)

    assert selected.returncode == 1
    assert selected.stderr == "FAIL: publication evidence rejected: shallow repository\n"
    assert history.returncode == 1
    assert history.stderr == "FAIL: publication evidence rejected: shallow repository\n"


def test_raw_range_and_history_ignore_inherited_shallow_file(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    _, side_tip, head = hidden_merge_taint(repository, base)
    shallow_file = tmp_path / "inherited-shallow"
    shallow_file.write_text(f"{side_tip}\n")
    environment = {"GIT_SHALLOW_FILE": str(shallow_file)}

    selected = run_publication_range(repository, script, base, head, environment)
    history = run_history(repository, script, environment)

    assert selected.returncode == 1
    assert (
        "publication range contains a forbidden tracked path: "
        ".agent-run/DEL-999/RUN.json"
    ) in selected.stderr
    assert history.returncode == 1
    assert (
        "reachable history contains a forbidden tracked path: "
        ".agent-run/DEL-999/RUN.json"
    ) in history.stderr


def test_range_and_history_anchor_git_endpoint_to_repository_root(tmp_path):
    repository, script, base = publication_repository(tmp_path / "tainted")
    tainted, _ = tainted_selected_and_clean_decoy(repository, base)
    git_at(repository, "update-ref", "refs/heads/publication-base", base)
    git_at(repository, "update-ref", "refs/heads/publication-head", tainted)

    decoy, _, decoy_base = publication_repository(tmp_path / "decoy")
    (decoy / "safe.md").write_text("clean decoy endpoint\n")
    git_at(decoy, "add", "safe.md")
    git_at(decoy, "commit", "-q", "-m", "clean decoy endpoint")
    decoy_head = git_at(decoy, "rev-parse", "HEAD")
    git_at(decoy, "update-ref", "refs/heads/publication-base", decoy_base)
    git_at(decoy, "update-ref", "refs/heads/publication-head", decoy_head)
    environment = {"GIT_DIR": str(decoy / ".git")}

    selected = run_publication_range(
        repository, script, "publication-base", "publication-head", environment,
    )
    history = run_history(repository, script, environment)

    assert selected.returncode == 1
    assert (
        "publication HEAD forbidden tracked path: .agent-run/DEL-999/RUN.json"
    ) in selected.stderr
    assert history.returncode == 1
    assert (
        "reachable history contains a forbidden tracked path: "
        ".agent-run/DEL-999/RUN.json"
    ) in history.stderr


def test_history_ignores_inherited_alternate_index(tmp_path):
    repository, script, _ = publication_repository(tmp_path)
    receipt = repository / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text('{"contract":"delivery-run"}\n')
    git_at(repository, "add", ".agent-run/DEL-999/RUN.json")
    alternate_index = tmp_path / "alternate-index"
    subprocess.run(
        ["git", "read-tree", "HEAD"], cwd=repository,
        env={**os.environ, "GIT_INDEX_FILE": str(alternate_index)},
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True,
    )

    history = run_history(
        repository, script, {"GIT_INDEX_FILE": str(alternate_index)},
    )

    assert history.returncode == 1
    assert "forbidden tracked path: .agent-run/DEL-999/RUN.json" in history.stderr


def test_history_validates_raw_head_required_files_over_worktree_decoy(tmp_path):
    repository, script, _ = publication_repository(tmp_path)
    git_at(repository, "rm", "-q", "LICENSE")
    git_at(repository, "commit", "-q", "-m", "remove required licence")
    (repository / "LICENSE").write_text("untracked decoy licence\n")

    history = run_history(repository, script)

    assert history.returncode == 1
    assert "publication HEAD missing required public file: LICENSE" in history.stderr


def test_history_validates_raw_head_symlink_and_size_over_index_decoy(tmp_path):
    repository, script, _ = publication_repository(tmp_path)
    (repository / "portable-link").symlink_to("README.md")
    (repository / "oversized.bin").write_bytes(b"x" * (5 * 1024 * 1024 + 1))
    git_at(repository, "add", "portable-link", "oversized.bin")
    git_at(repository, "commit", "-q", "-m", "non-portable raw head")
    git_at(repository, "rm", "-q", "portable-link", "oversized.bin")

    history = run_history(repository, script)

    assert history.returncode == 1
    assert "publication HEAD tracked symlink is not portable: portable-link" in history.stderr
    assert "publication HEAD tracked file exceeds 5 MiB: oversized.bin" in history.stderr


def test_evidence_git_environment_removes_inherited_redirections():
    inherited = {
        "PATH": "/trusted/bin",
        "HOME": "/trusted/home",
        "TMPDIR": "/trusted/tmp",
        "GIT_DIR": "/redirected/repo",
        "GIT_COMMON_DIR": "/redirected/common",
        "GIT_INDEX_FILE": "/redirected/index",
        "GIT_REPLACE_REF_BASE": "refs/custom-replace/",
        "GIT_OBJECT_DIRECTORY": "/redirected/objects",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES": "/redirected/alternate",
        "GIT_SHALLOW_FILE": "/redirected/shallow",
        "GIT_CONFIG_COUNT": "1",
        "GIT_CONFIG_KEY_0": "core.useReplaceRefs",
        "GIT_CONFIG_VALUE_0": "true",
    }

    environment = release_check.sanitized_git_environment(inherited)

    for name in (
        "GIT_DIR", "GIT_COMMON_DIR", "GIT_INDEX_FILE", "GIT_REPLACE_REF_BASE",
        "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_SHALLOW_FILE", "GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0",
        "GIT_CONFIG_VALUE_0",
    ):
        assert name not in environment
    assert environment["GIT_NO_REPLACE_OBJECTS"] == "1"
    assert environment["GIT_CONFIG_NOSYSTEM"] == "1"
    assert environment["GIT_CONFIG_GLOBAL"] == os.devnull
    assert environment["GIT_CONFIG_SYSTEM"] == os.devnull
    assert environment["GIT_GRAFT_FILE"] == os.devnull
    assert environment["GIT_NO_LAZY_FETCH"] == "1"
    assert environment["GIT_OPTIONAL_LOCKS"] == "0"
    assert environment["GIT_TERMINAL_PROMPT"] == "0"


@pytest.mark.parametrize("config_source", ["direct", "included", "worktree"])
def test_history_disables_repository_fsmonitor_hooks(tmp_path, config_source):
    repository, script, _ = publication_repository(tmp_path)
    marker = tmp_path / f"{config_source}-fsmonitor-ran"
    hook = tmp_path / f"{config_source}-fsmonitor.sh"
    hook.write_text(
        "#!/bin/sh\n"
        f": > {shlex.quote(str(marker))}\n"
    )
    hook.chmod(0o700)
    if config_source == "direct":
        git_at(repository, "config", "core.fsmonitor", str(hook))
    elif config_source == "included":
        included = tmp_path / "included-config"
        included.write_text(f"[core]\n\tfsmonitor = {hook}\n")
        git_at(repository, "config", "include.path", str(included))
    else:
        git_at(repository, "config", "extensions.worktreeConfig", "true")
        git_at(repository, "config", "--worktree", "core.fsmonitor", str(hook))
    git_at(repository, "status", "--porcelain")
    assert marker.is_file()
    marker.unlink()

    history = run_history(repository, script)

    assert history.returncode == 0, history.stderr
    assert not marker.exists()


def test_publication_range_rejects_noncommit_raw_endpoint_identity(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    (repository / "safe.md").write_text("clean selected endpoint\n")
    git_at(repository, "add", "safe.md")
    git_at(repository, "commit", "-q", "-m", "clean selected endpoint")
    git_at(repository, "tag", "-a", "publication-tag", "-m", "annotated endpoint")

    selected = run_publication_range(repository, script, base, "publication-tag")

    assert selected.returncode == 1
    assert selected.stderr == (
        "FAIL: publication endpoint 'publication-tag' does not match its raw commit identity\n"
    )


def test_range_passes_from_clean_linked_worktree(tmp_path):
    repository, _, base = publication_repository(tmp_path / "source")
    (repository / "safe.md").write_text("clean linked worktree change\n")
    git_at(repository, "add", "safe.md")
    git_at(repository, "commit", "-q", "-m", "clean linked worktree change")
    head = git_at(repository, "rev-parse", "HEAD")
    linked = tmp_path / "linked"
    git_at(repository, "worktree", "add", "-q", "--detach", str(linked), head)

    selected = run_publication_range(
        linked, linked / "scripts" / "public_release_check.py", base, head,
    )

    assert selected.returncode == 0, selected.stderr
    assert selected.stdout == "PASS: publication range clean\n"


def test_range_passes_with_repository_native_object_alternate(tmp_path):
    source, _, base = publication_repository(tmp_path / "source")
    (source / "safe.md").write_text("clean alternate-backed change\n")
    git_at(source, "add", "safe.md")
    git_at(source, "commit", "-q", "-m", "clean alternate-backed change")
    head = git_at(source, "rev-parse", "HEAD")
    alternate = tmp_path / "alternate"
    subprocess.run(
        ["git", "clone", "-q", "--shared", str(source), str(alternate)],
        check=True,
    )
    assert (alternate / ".git" / "objects" / "info" / "alternates").read_text().strip()

    selected = run_publication_range(
        alternate, alternate / "scripts" / "public_release_check.py", base, head,
    )

    assert selected.returncode == 0, selected.stderr
    assert selected.stdout == "PASS: publication range clean\n"


def test_range_and_history_pass_with_packed_object_storage(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    (repository / "safe.md").write_text("packed object evidence\n")
    git_at(repository, "add", "safe.md")
    git_at(repository, "commit", "-q", "-m", "packed object evidence")
    head = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "gc", "--prune=now")
    assert not loose_object_path(repository, head).exists()
    assert list((repository / ".git" / "objects" / "pack").glob("*.pack"))
    assert list((repository / ".git" / "objects" / "pack").glob("*.idx"))

    selected = run_publication_range(repository, script, base, head)
    history = run_history(repository, script)

    assert selected.returncode == 0, selected.stderr
    assert history.returncode == 0, history.stderr


def test_sha256_repository_hashes_clean_and_substituted_objects(tmp_path):
    repository, script, base = publication_repository(tmp_path, "sha256")
    assert len(base) == 64
    git_at(repository, "switch", "-q", "-c", "selected")
    (repository / "safe.md").write_text("portable sha256 tree\n")
    git_at(repository, "add", "safe.md")
    git_at(repository, "commit", "-q", "-m", "clean sha256 content")
    clean_head = git_at(repository, "rev-parse", "HEAD")
    clean = run_publication_range(repository, script, base, clean_head)
    assert clean.returncode == 0, clean.stderr
    leak = repository / "leak.txt"
    leak.write_text("/" + "Users/alice/private\n")
    git_at(repository, "add", "leak.txt")
    git_at(repository, "commit", "-q", "-m", "sha256 selected content")
    head = git_at(repository, "rev-parse", "HEAD")
    tainted_blob = git_at(repository, "rev-parse", f"{head}:leak.txt")
    clean_blob = subprocess.run(
        ["git", "hash-object", "-w", "--stdin"], cwd=repository,
        input=b"portable sha256 content\n", stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, check=True,
    ).stdout.decode("ascii").strip()
    substitute_loose_object(repository, tainted_blob, clean_blob)

    selected = run_publication_range(repository, script, base, head)
    history = run_history(repository, script)

    assert selected.returncode == 1
    assert tainted_blob in selected.stderr
    assert "object identity does not match sha256" in selected.stderr
    assert history.returncode == 1
    assert tainted_blob in history.stderr
    assert "object identity does not match sha256" in history.stderr


def test_publication_range_rejects_receipt_added_then_deleted_on_selected_branch(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    git_at(repository, "switch", "-q", "-c", "selected")
    receipt = repository / ".agent-run" / "DEL-999" / "RUN.json"
    receipt.parent.mkdir(parents=True)
    receipt.write_text(
        '{"schema_version":1,"contract":"delivery-run","run_id":"DEL-999"}\n'
    )
    git_at(repository, "add", ".agent-run/DEL-999/RUN.json")
    git_at(repository, "commit", "-q", "-m", "add private receipt")
    git_at(repository, "rm", "-q", ".agent-run/DEL-999/RUN.json")
    git_at(repository, "commit", "-q", "-m", "delete private receipt")
    selected_head = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "switch", "-q", "-c", "clean-sibling", base)

    result = run_publication_range(repository, script, base, selected_head)

    assert result.returncode == 1
    assert (
        "publication range contains a forbidden tracked path: "
        ".agent-run/DEL-999/RUN.json"
    ) in result.stderr


def test_publication_range_validates_selected_head_tree_not_checked_out_sibling(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    git_at(repository, "switch", "-q", "-c", "selected")
    git_at(repository, "rm", "-q", "LICENSE")
    git_at(repository, "commit", "-q", "-m", "remove required licence")
    selected_head = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "switch", "-q", "-c", "clean-sibling", base)

    result = run_publication_range(repository, script, base, selected_head)

    assert result.returncode == 1
    assert "publication HEAD missing required public file: LICENSE" in result.stderr


def test_publication_range_rejects_selected_head_symlink_and_oversized_file(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    git_at(repository, "switch", "-q", "-c", "selected")
    (repository / "portable-link").symlink_to("README.md")
    (repository / "oversized.bin").write_bytes(b"x" * (5 * 1024 * 1024 + 1))
    git_at(repository, "add", "portable-link", "oversized.bin")
    git_at(repository, "commit", "-q", "-m", "add non-portable tree entries")
    selected_head = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "switch", "-q", "-c", "clean-sibling", base)

    result = run_publication_range(repository, script, base, selected_head)

    assert result.returncode == 1
    assert "publication HEAD tracked symlink is not portable: portable-link" in result.stderr
    assert "publication HEAD tracked file exceeds 5 MiB: oversized.bin" in result.stderr


def test_publication_range_scans_full_selected_commit_messages(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    git_at(repository, "switch", "-q", "-c", "selected")
    body = (
        "metadata must not expose /" + "Users/alice/private\n"
        "or github" + "_pat_abcdefghijklmnopqrstuvwxyz123456"
    )
    git_at(
        repository, "commit", "-q", "--allow-empty", "-m", "metadata leak",
        "-m", body,
    )
    selected_head = git_at(repository, "rev-parse", "HEAD")
    git_at(repository, "switch", "-q", "-c", "clean-sibling", base)

    result = run_publication_range(repository, script, base, selected_head)

    assert result.returncode == 1
    assert (
        "publication range commit message contains a personal absolute home path"
    ) in result.stderr
    assert (
        "publication range commit message contains a possible GitHub token"
    ) in result.stderr


def test_publication_tree_parser_fails_closed_on_malformed_git_output():
    with pytest.raises(RuntimeError, match="unparseable entry"):
        release_check.parse_raw_tree(b"truncated", 20)


def test_raw_parent_traversal_fails_closed_on_missing_parent_object(tmp_path):
    repository, script, base = publication_repository(tmp_path)
    tree = git_at(repository, "rev-parse", f"{base}^{{tree}}")
    missing_parent = "f" * 40
    raw_commit = (
        f"tree {tree}\n"
        f"parent {missing_parent}\n"
        "author Release Test <release@example.test> 1 +0000\n"
        "committer Release Test <release@example.test> 1 +0000\n\n"
        "missing raw parent\n"
    ).encode("ascii")
    written = subprocess.run(
        ["git", "hash-object", "-t", "commit", "-w", "--stdin"],
        cwd=repository, input=raw_commit, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, check=True,
    ).stdout.decode("ascii").strip()

    selected = run_publication_range(repository, script, base, written)

    assert selected.returncode == 1
    assert missing_parent in selected.stderr
    assert "cannot extract publication object" in selected.stderr


def test_public_tree_retains_ui_ux_pro_max_attribution():
    # Epic #124 Workstream E: third-party licence texts are centralised in the
    # top-level LICENSES/ directory (not beside each skill), and the prose
    # provenance index lives in THIRD_PARTY_NOTICES.md.
    root = Path(__file__).resolve().parents[1]
    licence = root / "LICENSES/ui-ux-pro-max-MIT.txt"
    repository_notice = (root / "THIRD_PARTY_NOTICES.md").read_text()
    assert licence.is_file()
    assert "Copyright (c) 2024 Next Level Builder" in licence.read_text()
    assert "UI UX Pro Max v2.0.0" in repository_notice
    assert "ui-ux-pro-max-MIT.txt" in repository_notice


# --- D-032 regression group 1: streaming decoy-bridged restart equivalence ---

DECOY_BRIDGE_CLASSES = [
    pytest.param(b"sk-", "possible OpenAI key", id="openai"),
    pytest.param(b"sk-ant-", "possible Anthropic key", id="anthropic"),
]


def decoy_bridge_payload(prefix: bytes) -> bytes:
    """A word-prefixed invalid decoy, then a valid dash-class restart ending mid-chunk.

    The leading ``x`` makes the first prefix an invalid candidate (no ``\\b``), while the
    dash-class run bridges the decoy into a genuine key that terminates on the trailing
    ``.`` rather than at end of content.
    """
    return b"x" + prefix + b"A" * 20 + b"-" + prefix + b"B" * 20 + b"."


@pytest.mark.parametrize("prefix,label", DECOY_BRIDGE_CLASSES)
def test_streaming_decoy_bridged_restart_matches_policy_under_every_single_cut(
    prefix, label,
):
    payload = decoy_bridge_payload(prefix)
    expected = release_check._whole_content_findings(payload)
    assert label in expected

    for cut in range(1, len(payload)):
        scanner = release_check._BoundedContentScanner()
        scanner.feed(memoryview(payload)[:cut])
        scanner.feed(memoryview(payload)[cut:])
        assert scanner.finish() == expected, (label, "cut", cut)

    for chunk_size in range(1, len(payload) + 1):
        assert streamed_content_findings(payload, chunk_size) == expected, (
            label, "chunk_size", chunk_size,
        )


@pytest.mark.parametrize("prefix,label", DECOY_BRIDGE_CLASSES)
def test_streaming_decoy_bridged_restart_matches_policy_at_exact_decoy_boundary(
    prefix, label,
):
    payload = decoy_bridge_payload(prefix)
    expected = release_check._whole_content_findings(payload)

    scanner = release_check._BoundedContentScanner()
    scanner.feed(memoryview(payload)[:1])
    scanner.feed(memoryview(payload)[1:])

    assert scanner.finish() == expected
    assert label in scanner.finish()


@pytest.mark.parametrize("prefix,label", DECOY_BRIDGE_CLASSES)
def test_streamed_blob_evidence_finds_decoy_bridged_key_at_forced_chunk_boundary(
    tmp_path, prefix, label,
):
    chunk_size = 64
    repository, _, _ = publication_repository(tmp_path)
    payload = b"." * (chunk_size - 1) + decoy_bridge_payload(prefix)
    (repository / "leak.txt").write_bytes(payload)
    git_at(repository, "add", "leak.txt")
    git_at(repository, "commit", "-q", "-m", "decoy bridged restart")
    blob = git_at(repository, "rev-parse", "HEAD:leak.txt")

    endpoint = release_check.EvidenceGitEndpoint.open(repository)
    accounting = release_check.EvidenceMemoryAccounting()
    evidence = release_check.stream_blob_evidence(
        endpoint, {blob}, chunk_size=chunk_size, accounting=accounting,
    )

    assert evidence[blob].findings == release_check._whole_content_findings(payload)
    assert label in evidence[blob].findings


def aws_run_payload(terminator: bytes) -> bytes:
    """A boundary-valid ``AKIA`` run of exactly 16 bytes, closed by ``terminator``.

    The AWS run class is ``[A-Z0-9]``, so a lowercase letter or ``_`` both *ends* the run
    and is a word byte that denies the trailing ``\\b``. The whole-content policy reports
    nothing there, which makes the terminal-boundary guard on the mid-chunk termination
    arm load-bearing for this class alone: the dash classes admit every word byte into
    their run, so their run can never stop on one.
    """
    return b"-AKIA" + b"A" * 16 + terminator


@pytest.mark.parametrize(
    "terminator", [pytest.param(b"a", id="lowercase"), pytest.param(b"_", id="underscore")],
)
def test_streaming_word_byte_terminated_aws_run_matches_policy_under_every_single_cut(
    terminator,
):
    suppressed = aws_run_payload(terminator)
    reported = aws_run_payload(b".")

    # The terminal byte is the only difference between the two payloads, so a scanner
    # that ignores it reports a secret the whole-content policy does not.
    assert release_check._whole_content_findings(suppressed) == frozenset()
    assert release_check._whole_content_findings(reported) == frozenset(
        {"possible AWS access key"},
    )

    for payload in (suppressed, reported):
        expected = release_check._whole_content_findings(payload)

        for cut in range(1, len(payload)):
            scanner = release_check._BoundedContentScanner()
            scanner.feed(memoryview(payload)[:cut])
            scanner.feed(memoryview(payload)[cut:])
            assert scanner.finish() == expected, (payload, "cut", cut)

        for chunk_size in range(1, len(payload) + 1):
            assert streamed_content_findings(payload, chunk_size) == expected, (
                payload, "chunk_size", chunk_size,
            )


# --- D-032 regression group 2: stderr drain thread-start failure ---

class TrackingPipe(io.BytesIO):
    pass


class TrackingChild:
    instances: list["TrackingChild"] = []

    def __init__(self, *_, stderr, **__):
        self.stdin = TrackingPipe()
        self.stdout = TrackingPipe()
        self.stderr = TrackingPipe() if stderr == subprocess.PIPE else None
        self.returncode = None
        self.terminated = False
        self.killed = False
        self.waits = 0
        self.stubborn = False
        TrackingChild.instances.append(self)

    def poll(self):
        return self.returncode

    def wait(self, timeout=None):
        self.waits += 1
        if self.returncode is None:
            raise subprocess.TimeoutExpired("git cat-file", timeout)
        return self.returncode

    def terminate(self):
        self.terminated = True
        if not self.stubborn:
            self.returncode = -15

    def kill(self):
        self.killed = True
        self.returncode = -9


@pytest.mark.parametrize("stubborn", [False, True], ids=["terminate", "kill"])
def test_batch_stderr_drain_start_failure_reaps_child_and_preserves_exception(
    tmp_path, monkeypatch, stubborn,
):
    TrackingChild.instances.clear()

    class UnstartableThread(threading.Thread):
        def start(self):
            raise RuntimeError("cannot start new thread")

    def spawn(*args, **kwargs):
        child = TrackingChild(*args, **kwargs)
        child.stubborn = stubborn
        return child

    monkeypatch.setattr(release_check.subprocess, "Popen", spawn)
    monkeypatch.setattr(release_check.threading, "Thread", UnstartableThread)
    endpoint = release_check.EvidenceGitEndpoint(tmp_path, tmp_path, tmp_path, {})
    endpoint.object_format = "sha1"
    endpoint.raw_object_id_size = 20
    accounting = release_check.EvidenceMemoryAccounting()

    with pytest.raises(RuntimeError, match="cannot start new thread"):
        with endpoint.object_batch(chunk_size=7, accounting=accounting):
            pass

    assert len(TrackingChild.instances) == 1
    child = TrackingChild.instances[0]

    # No false started-count increment, and nothing claims to have joined a dead thread.
    assert accounting.batch_stderr_drainers_started == 0
    assert accounting.batch_stderr_drainers_joined == 0

    # The child is terminated or killed, and reaped.
    assert child.terminated
    assert child.killed is stubborn
    assert child.returncode is not None

    # Every owned pipe is closed.
    assert child.stdin.closed
    assert child.stdout.closed
    assert child.stderr is not None and child.stderr.closed


# --- D-032 regression group 3: unexpected stderr worker exception fails closed ---

class SyntheticDrainFailure(Exception):
    """A non-OS worker failure that is not a ``RuntimeError``.

    The invariant D-032 restores is that *every* non-OS ``Exception`` reaches the joining
    thread, so the regression must exercise a class the worker's ``except`` clause cannot
    name. Pinning ``RuntimeError`` alone cannot tell ``except Exception`` apart from a
    narrower catch that silently loses the failure.
    """


@pytest.mark.parametrize(
    "failure",
    [pytest.param(RuntimeError, id="runtime"), pytest.param(SyntheticDrainFailure, id="custom")],
)
def test_batch_stderr_drain_surfaces_unexpected_worker_exception_at_join(failure):
    accounting = release_check.EvidenceMemoryAccounting()
    drain = release_check._BoundedStderrDrain(accounting)

    class ExplodingStderr:
        def __init__(self):
            self.closed = False

        def fileno(self):
            raise io.UnsupportedOperation

        def read(self, size=-1):
            raise failure("synthetic non-OS drain failure")

        def close(self):
            self.closed = True

    drain.start(ExplodingStderr())

    # Let the worker die on its own before any stop is requested, so the recorded
    # error cannot be excused as shutdown noise.
    assert drain.thread is not None
    drain.thread.join(timeout=5)
    assert not drain.thread.is_alive()

    with pytest.raises(RuntimeError, match="synthetic non-OS drain failure"):
        drain.stop_and_join()

    assert type(drain.error) is failure
    assert accounting.batch_stderr_drainers_started == 1
    assert accounting.batch_stderr_drainers_joined == 1
