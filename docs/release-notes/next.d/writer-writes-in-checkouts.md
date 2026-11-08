## Fixes
### Lanes and settlement

- Writer lanes launch again when `[domains.run] writer_writes` names a directory inside another checkout, such as the primary checkout's ignored `.agent-run/locks`. Since 1.6.16 such a project refused every writer with "confinement permission overlaps a protected path". The configured directory is now exempt from that checkout's write denial, and the rest of the checkout stays read-only. A `writer_writes` entry that is the checkout itself, encloses one or reaches Git metadata, and any graph write root inside another checkout, are still refused.
