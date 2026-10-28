CREATE TABLE viewer_requests (
    project TEXT NOT NULL, viewer TEXT NOT NULL, request TEXT NOT NULL,
    seen_revision INTEGER, snoozed_until TEXT, dismissed INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(project, viewer, request)
);
CREATE TABLE viewer_summaries (
    project TEXT NOT NULL, viewer TEXT NOT NULL, session TEXT NOT NULL,
    revision INTEGER NOT NULL,
    PRIMARY KEY(project, viewer, session)
);
