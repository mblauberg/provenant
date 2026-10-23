"""Streaming structure scanner for retained Git evidence packets."""

from __future__ import annotations

import json
import re
from typing import Any


def validate_header(metadata: Any) -> bool:
    """Apply the packet header predicate; callers own validation timing and errors."""
    return (
        isinstance(metadata, dict)
        and set(metadata) == {
            "schema_version", "record_type", "repository", "git_root", "head",
            "diff_base", "working_tree", "diff_from", "paths", "encoding",
        }
        and metadata.get("schema_version") == 1
        and metadata.get("record_type") == "provenant-git-evidence"
        and isinstance(metadata.get("repository"), str)
        and isinstance(metadata.get("git_root"), str)
        and isinstance(metadata.get("head"), str)
        and re.fullmatch(r"[0-9a-f]{40,64}", metadata["head"]) is not None
        and isinstance(metadata.get("diff_base"), str)
        and re.fullmatch(r"[0-9a-f]{40,64}", metadata["diff_base"]) is not None
        and metadata.get("working_tree") in {"clean", "dirty"}
        and isinstance(metadata.get("diff_from"), str)
        and bool(metadata["diff_from"])
        and isinstance(metadata.get("paths"), list)
        and all(isinstance(path, str) for path in metadata["paths"])
        and metadata.get("encoding") == "utf-8-replacement"
    )


class PacketScanner:
    """Scan chunks without retaining the body, leaving EOF policy to each reader."""

    def __init__(self, error_type: type[ValueError], max_header_bytes: int, *, validate_on_feed=False):
        self.error_type = error_type
        self.max_header_bytes = max_header_bytes
        self.validate_on_feed = validate_on_feed
        self.metadata: Any = None
        self.header = bytearray()
        self.status_prefix = b"--- status ---\n"
        self.body_prefix = bytearray()
        self.marker = b"\n--- diff ---\n"
        self.tail = b""

    def feed(self, chunk: bytes) -> None:
        remaining = chunk
        if self.metadata is None:
            self.header.extend(remaining)
            if len(self.header) > self.max_header_bytes and b"\n" not in self.header:
                raise self.error_type("Git evidence packet header exceeds its 64 KiB limit")
            separator = self.header.find(b"\n")
            if separator < 0:
                return
            if separator > self.max_header_bytes:
                raise self.error_type("Git evidence packet header exceeds its 64 KiB limit")
            self.metadata = json.loads(bytes(self.header[:separator]).decode("utf-8"))
            if self.validate_on_feed:
                try:
                    valid = validate_header(self.metadata)
                except (KeyError, TypeError):
                    valid = False
                if not valid:
                    raise self.error_type("Git evidence packet header is invalid")
            remaining = bytes(self.header[separator + 1:])
        self.body_prefix.extend(remaining[: len(self.status_prefix) - len(self.body_prefix)])
        if len(self.body_prefix) >= len(self.status_prefix) and bytes(self.body_prefix) != self.status_prefix:
            raise self.error_type("Git evidence packet is missing its status section")
        combined = self.tail + remaining
        if self.marker in combined:
            self.tail = self.marker
        else:
            self.tail = combined[-(len(self.marker) - 1):]

    @property
    def has_status(self) -> bool:
        return bytes(self.body_prefix) == self.status_prefix

    @property
    def has_diff(self) -> bool:
        return self.marker in self.tail

    def finish(self) -> Any:
        """Return parsed metadata; callers retain their distinct EOF rejection order."""
        return self.metadata
