CREATE TABLE viewer_requests (
    project TEXT NOT NULL, viewer TEXT NOT NULL CHECK(length(CAST(viewer AS BLOB)) BETWEEN 1 AND 128), request TEXT NOT NULL,
    seen_revision INTEGER CHECK(seen_revision IS NULL OR (typeof(seen_revision) = 'integer' AND seen_revision > 0)), snoozed_until TEXT, dismissed INTEGER NOT NULL DEFAULT 0 CHECK(typeof(dismissed) = 'integer' AND dismissed IN (0,1)),
    PRIMARY KEY(project, viewer, request)
);
CREATE TABLE viewer_summaries (
    project TEXT NOT NULL, viewer TEXT NOT NULL CHECK(length(CAST(viewer AS BLOB)) BETWEEN 1 AND 128), session TEXT NOT NULL CHECK(length(CAST(session AS BLOB)) BETWEEN 1 AND 128),
    revision INTEGER NOT NULL CHECK(typeof(revision) = 'integer' AND revision > 0),
    PRIMARY KEY(project, viewer, session)
);
