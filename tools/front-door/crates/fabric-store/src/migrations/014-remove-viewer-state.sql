-- Viewer presentation state (seen, snooze, dismiss and summary baselines) had
-- no reader beyond its own commands and is removed. Its rows carry nothing the
-- human inbox needs, so they are dropped with their tables.
DROP TABLE viewer_requests;
DROP TABLE viewer_summaries;
