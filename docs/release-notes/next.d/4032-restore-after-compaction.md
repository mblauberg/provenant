## Additions

- The `session` skill now restores after compaction in three bounded steps: read the hook context, run one cursor-bounded state read (`provenant status --session-stream --cursor <cursor>`), then act against its owners. It reads tool state rather than its own driver or tool source and leaves the wait rule to `orchestrate`'s "Wait and recover" section, so a resumed chair re-orients without re-reading code or polling a wait. (#4032)
