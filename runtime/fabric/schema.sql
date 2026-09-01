-- Fabric storage. One file, opened directly by every agent process.
--
-- There is no daemon and no capability token. On a single-user machine every
-- agent already runs as the same uid and can read every other agent's files, so
-- a mediating process could only inconvenience an honest caller. The security
-- boundary is the mode on this file's directory.
--
-- Identity is derived, never provisioned: ordinary registered worktrees share
-- their primary checkout; other Git and non-Git roots stand alone. The
-- agent_id comes from the provider environment. Nothing needs provisioning.

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS agents (
  project    TEXT NOT NULL,
  agent_id   TEXT NOT NULL,
  provider   TEXT NOT NULL,
  first_seen INTEGER NOT NULL,
  last_seen  INTEGER NOT NULL,
  PRIMARY KEY (project, agent_id)
);

CREATE TABLE IF NOT EXISTS messages (
  message_id      TEXT PRIMARY KEY,
  project         TEXT NOT NULL,
  sender_id       TEXT NOT NULL,
  body            TEXT NOT NULL,
  kind            TEXT NOT NULL DEFAULT 'note',
  conversation_id TEXT NOT NULL,
  reply_to        TEXT REFERENCES messages(message_id),
  task_id         TEXT,
  output_path     TEXT,
  created_at      INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS messages_by_conversation
  ON messages(project, conversation_id, created_at);

-- One row per recipient. `read_at` is the explicit acknowledgement timestamp;
-- old databases retain their already-read deliveries as acknowledged.
CREATE TABLE IF NOT EXISTS deliveries (
  message_id   TEXT NOT NULL REFERENCES messages(message_id) ON DELETE CASCADE,
  project      TEXT NOT NULL,
  recipient_id TEXT NOT NULL,
  read_at      INTEGER,
  PRIMARY KEY (message_id, recipient_id)
);

CREATE INDEX IF NOT EXISTS deliveries_unread
  ON deliveries(project, recipient_id, read_at);

-- One durable claim per delivery. An expired claim can be replaced atomically
-- by a later inbox read, so redelivery needs no daemon or scheduler.
CREATE TABLE IF NOT EXISTS delivery_claims (
  message_id   TEXT NOT NULL REFERENCES messages(message_id) ON DELETE CASCADE,
  project      TEXT NOT NULL,
  recipient_id TEXT NOT NULL,
  claim_id     TEXT NOT NULL,
  claimed_at   INTEGER NOT NULL,
  expires_at   INTEGER NOT NULL,
  PRIMARY KEY (message_id, recipient_id)
);

CREATE INDEX IF NOT EXISTS delivery_claims_expiry
  ON delivery_claims(project, recipient_id, expires_at);

CREATE TABLE IF NOT EXISTS teams (
  project    TEXT NOT NULL,
  team_id    TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (project, team_id)
);

CREATE TABLE IF NOT EXISTS team_members (
  project  TEXT NOT NULL,
  team_id  TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  PRIMARY KEY (project, team_id, agent_id)
);

CREATE TABLE IF NOT EXISTS tasks (
  project    TEXT NOT NULL,
  task_id    TEXT NOT NULL,
  objective  TEXT NOT NULL,
  owner_id   TEXT,
  state      TEXT NOT NULL DEFAULT 'open',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (project, task_id)
);

CREATE TABLE IF NOT EXISTS task_dependencies (
  project    TEXT NOT NULL,
  task_id    TEXT NOT NULL,
  depends_on TEXT NOT NULL,
  PRIMARY KEY (project, task_id, depends_on)
);

-- Oversight. Append-only, never read by the tool itself, only by the user.
-- This is the "what are my agents doing" surface, and it is one table because
-- that is all the question needs.
CREATE TABLE IF NOT EXISTS activity (
  seq      INTEGER PRIMARY KEY AUTOINCREMENT,
  project  TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  at       INTEGER NOT NULL,
  kind     TEXT NOT NULL,
  detail   TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS activity_by_project ON activity(project, seq);
