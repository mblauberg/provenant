# ADR 0019: Separate product, user and machine-local installation ownership

**Status:** Accepted

Changed: 2026-10-26, #1754.

## Context

An installer that owns both product files and user edits silently reverts one
on update. Machine paths committed as shared configuration also break another
host's installation.

## Decision

Separate product content, user-owned configuration and machine-local derived
state. The config crate owns managed installation and its receipt.

- Product files are immutable release content, addressed by source digest.
  Installation materialises managed files and links from that release.
- User configuration and user-authored instructions remain user-owned.
  Seeding a missing file does not authorise overwriting later user edits.
- Absolute installation paths, generated client links and the managed-file
  receipt are machine-local derived state, outside committed portable intent.

The installer refuses unmanaged conflicts and preserves user-authored targets.
It removes only unchanged receipt-owned entries. Exact desired content is a
no-op, including after an interrupted install before receipt publication.
Publish the managed receipt after applying entries under the install lock;
a later run reconciles an interrupted installation. This is not a multi-path
transaction.

## Consequences

Updates have one explicit writer per file class. A user changes product
behaviour through the product or a supported configuration surface, rather
than editing managed release files. The retired instance YAML, product-pointer
and rename-registry formats are not current installation contracts. Current
owners are described in the [state design](../specs/state.md).
