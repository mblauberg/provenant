# v2.4.1 upgrade corpus

These are the schema-1 `Reservation`, `GuardianLock`, and `SettlementJournal`
shapes from v2.4.1 `crates/supervisor/src/custody{,/liveness}.rs`. They model
retained custody from a never-spawned attempt and a separate settled attempt. `seed.py` creates the
old worktree slot and attempt layout and replaces the device and inode fields
with identities of the newly created files. Those values cannot be copied
between filesystems. The schema-1 guardian is the retained sibling evidence
that caused #3681; it must fence its own slot without refusing another launch.
The corpus is synthetic, contains no host state or credentials, and does not
claim to be a complete backup of the old RunStore database.
