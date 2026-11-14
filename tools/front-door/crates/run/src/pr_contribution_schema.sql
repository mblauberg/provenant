CREATE TABLE links (
 attempt TEXT NOT NULL, repository TEXT NOT NULL, number INTEGER NOT NULL,
 weight INTEGER, PRIMARY KEY(attempt, repository, number)
) STRICT;
CREATE INDEX links_pr ON links(repository, number, attempt);
CREATE TABLE allocations (attempt TEXT PRIMARY KEY) STRICT;
CREATE TABLE branch_attempts (
 attempt TEXT PRIMARY KEY, project TEXT NOT NULL, branch TEXT NOT NULL,
 role TEXT NOT NULL, model TEXT, account TEXT,
 generation INTEGER NOT NULL DEFAULT 0, queued_at INTEGER,
 outcome TEXT, finished_at INTEGER
) STRICT;
CREATE INDEX branch_attempts_branch ON branch_attempts(project, branch);
CREATE TABLE branch_prs (
 project TEXT NOT NULL, branch TEXT NOT NULL, repository TEXT NOT NULL,
 number INTEGER NOT NULL, head TEXT NOT NULL,
 generation INTEGER NOT NULL, closed INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(project,branch,repository,number,generation)
) STRICT;
CREATE TABLE branch_generations (
 project TEXT NOT NULL, branch TEXT NOT NULL, generation INTEGER NOT NULL,
 PRIMARY KEY(project,branch)
) STRICT;
CREATE TABLE truncations (attempt TEXT PRIMARY KEY) STRICT;
CREATE TABLE reassignments (
 attempt TEXT NOT NULL, repository TEXT NOT NULL, number INTEGER NOT NULL,
 PRIMARY KEY(attempt, repository, number)
) STRICT;
PRAGMA user_version=5;
