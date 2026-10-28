-- Host-owned associations. Child run stores are lane-writable ingress.
CREATE TABLE run_chairs (
    project TEXT NOT NULL,
    run TEXT NOT NULL,
    record BLOB NOT NULL,
    PRIMARY KEY(project, run)
) STRICT;
