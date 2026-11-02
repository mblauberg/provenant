Run-store schema 7 adds terminal decision evidence and independent settlement
receipts. The migration creates a timestamped SQLite copy before upgrading;
downgrade is unsupported, so restoring an earlier version requires that copy or
a forward repair.
