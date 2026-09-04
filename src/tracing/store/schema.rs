//! Schema DDL, versioned through `PRAGMA user_version`. Migrations are
//! append-only: never edit a shipped entry, add a new one.

pub const SCHEMA_VERSION: i32 = 2;

pub const MIGRATIONS: &[&str] = &[V1, V2];

const V1: &str = r#"
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE runs (
  id                TEXT PRIMARY KEY,
  pid               INTEGER,
  agent_mux_version TEXT NOT NULL,
  started_ns        INTEGER NOT NULL,
  heartbeat_ns      INTEGER NOT NULL,
  ended_ns          INTEGER,
  termination       TEXT
);

CREATE TABLE sessions (
  key             TEXT PRIMARY KEY,
  provider        TEXT NOT NULL CHECK (provider IN ('claude','codex','antigravity')),
  session_id      TEXT NOT NULL,
  user_id         TEXT,
  cwd             TEXT,
  project_slug    TEXT,
  transcript_path TEXT,
  title           TEXT,
  first_seen_ns   INTEGER NOT NULL,
  last_seen_ns    INTEGER NOT NULL,
  extra           TEXT NOT NULL DEFAULT '{}',
  UNIQUE (provider, session_id)
);
CREATE INDEX sessions_last_seen ON sessions (last_seen_ns DESC);
CREATE INDEX sessions_project   ON sessions (project_slug, last_seen_ns DESC);

CREATE TABLE launches (
  id                     TEXT PRIMARY KEY,
  run_id                 TEXT NOT NULL REFERENCES runs (id),
  agent_mux_session      INTEGER NOT NULL,
  profile                TEXT NOT NULL,
  provider               TEXT NOT NULL,
  cwd                    TEXT NOT NULL,
  project_slug           TEXT NOT NULL,
  content_mode           TEXT NOT NULL CHECK (content_mode IN ('metadata','full')),
  correlation_plan       TEXT NOT NULL,
  correlation            TEXT,
  session_key            TEXT REFERENCES sessions (key),
  injected_session_id    INTEGER NOT NULL DEFAULT 0,
  attached               INTEGER NOT NULL DEFAULT 0,
  started_ns             INTEGER NOT NULL,
  ended_ns               INTEGER,
  termination            TEXT,
  exit_code              INTEGER,
  parse_errors           INTEGER NOT NULL DEFAULT 0,
  dropped_ops            INTEGER NOT NULL DEFAULT 0,
  reported_cost_usd      REAL,
  reported_lines_added   INTEGER,
  reported_lines_removed INTEGER,
  agent_mux_version      TEXT NOT NULL,
  user_id                TEXT,
  release                TEXT,
  environment            TEXT,
  tags                   TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX launches_started ON launches (started_ns DESC);
CREATE INDEX launches_session ON launches (session_key);

CREATE TABLE traces (
  rid                    INTEGER PRIMARY KEY,
  id                     TEXT NOT NULL UNIQUE,
  session_key            TEXT NOT NULL REFERENCES sessions (key),
  launch_id              TEXT REFERENCES launches (id),
  ordinal                INTEGER NOT NULL,
  name                   TEXT NOT NULL,
  status                 TEXT NOT NULL CHECK (status IN ('open','closed','aborted')),
  start_ns               INTEGER NOT NULL,
  end_ns                 INTEGER,
  input                  TEXT,
  output                 TEXT,
  thinking               TEXT,
  skills                 TEXT NOT NULL DEFAULT '[]',
  reported_duration_ms   INTEGER,
  reported_message_count INTEGER,
  session_cost_usd       REAL,
  timing_approx          INTEGER NOT NULL DEFAULT 0,
  ordinal_salted         INTEGER NOT NULL DEFAULT 0,
  closed_by              TEXT,
  metadata               TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX traces_session ON traces (session_key, ordinal);
CREATE INDEX traces_launch  ON traces (launch_id);
CREATE INDEX traces_start   ON traces (start_ns DESC);
CREATE INDEX traces_open    ON traces (status) WHERE status = 'open';

CREATE TABLE observations (
  rid                   INTEGER PRIMARY KEY,
  id                    TEXT NOT NULL UNIQUE,
  trace_id              TEXT NOT NULL REFERENCES traces (id),
  parent_id             TEXT,
  type                  TEXT NOT NULL CHECK (type IN ('generation','tool','agent','event','span')),
  name                  TEXT NOT NULL,
  kind                  TEXT,
  start_ns              INTEGER NOT NULL,
  end_ns                INTEGER,
  level                 TEXT NOT NULL DEFAULT 'DEFAULT' CHECK (level IN ('DEBUG','DEFAULT','WARNING','ERROR')),
  status_message        TEXT,
  model                 TEXT,
  model_id              TEXT,
  input                 TEXT,
  output                TEXT,
  thinking              TEXT,
  usage                 TEXT,
  input_tokens          INTEGER,
  output_tokens         INTEGER,
  cache_read_tokens     INTEGER,
  cache_write_tokens    INTEGER,
  cache_write_1h_tokens INTEGER,
  reasoning_tokens      INTEGER,
  total_tokens          INTEGER,
  input_cost_usd        REAL,
  output_cost_usd       REAL,
  cache_read_cost_usd   REAL,
  cache_write_cost_usd  REAL,
  total_cost_usd        REAL,
  tool_id               TEXT,
  tool_name             TEXT,
  skill                 TEXT,
  mcp_server            TEXT,
  path                  TEXT,
  is_error              INTEGER NOT NULL DEFAULT 0,
  ts_approx             INTEGER NOT NULL DEFAULT 0,
  metadata              TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX observations_trace ON observations (trace_id, start_ns);
CREATE INDEX observations_start ON observations (start_ns DESC);
CREATE INDEX observations_model ON observations (model) WHERE type = 'generation';
CREATE INDEX observations_tool  ON observations (tool_name) WHERE type IN ('tool','agent');
CREATE INDEX observations_open  ON observations (trace_id) WHERE end_ns IS NULL;

CREATE TABLE models (
  id                   TEXT PRIMARY KEY,
  provider             TEXT NOT NULL,
  match                TEXT NOT NULL,
  input_per_m          REAL NOT NULL,
  output_per_m         REAL NOT NULL,
  cache_read_per_m     REAL,
  cache_write_per_m    REAL,
  cache_write_1h_per_m REAL,
  reasoning_per_m      REAL,
  source               TEXT NOT NULL CHECK (source IN ('builtin','config','user')),
  updated_at           TEXT NOT NULL
);

CREATE VIRTUAL TABLE observations_fts USING fts5 (input, output, content = 'observations', content_rowid = 'rid', tokenize = 'unicode61');
CREATE TRIGGER observations_fts_ai AFTER INSERT ON observations BEGIN
  INSERT INTO observations_fts (rowid, input, output) VALUES (new.rid, new.input, new.output);
END;
CREATE TRIGGER observations_fts_ad AFTER DELETE ON observations BEGIN
  INSERT INTO observations_fts (observations_fts, rowid, input, output) VALUES ('delete', old.rid, old.input, old.output);
END;
CREATE TRIGGER observations_fts_au AFTER UPDATE OF input, output ON observations BEGIN
  INSERT INTO observations_fts (observations_fts, rowid, input, output) VALUES ('delete', old.rid, old.input, old.output);
  INSERT INTO observations_fts (rowid, input, output) VALUES (new.rid, new.input, new.output);
END;

CREATE VIRTUAL TABLE traces_fts USING fts5 (input, output, content = 'traces', content_rowid = 'rid', tokenize = 'unicode61');
CREATE TRIGGER traces_fts_ai AFTER INSERT ON traces BEGIN
  INSERT INTO traces_fts (rowid, input, output) VALUES (new.rid, new.input, new.output);
END;
CREATE TRIGGER traces_fts_ad AFTER DELETE ON traces BEGIN
  INSERT INTO traces_fts (traces_fts, rowid, input, output) VALUES ('delete', old.rid, old.input, old.output);
END;
CREATE TRIGGER traces_fts_au AFTER UPDATE OF input, output ON traces BEGIN
  INSERT INTO traces_fts (traces_fts, rowid, input, output) VALUES ('delete', old.rid, old.input, old.output);
  INSERT INTO traces_fts (rowid, input, output) VALUES (new.rid, new.input, new.output);
END;

CREATE VIEW trace_stats AS
SELECT t.*,
       datetime(t.start_ns / 1000000000, 'unixepoch') AS started_at,
       (COALESCE(t.end_ns, MAX(COALESCE(o.end_ns, o.start_ns)), t.start_ns) - t.start_ns) / 1000000 AS latency_ms,
       COUNT(o.rid)                                   AS observation_count,
       COALESCE(SUM(o.type = 'generation'), 0)        AS generation_count,
       COALESCE(SUM(o.type IN ('tool','agent')), 0)   AS tool_count,
       COALESCE(SUM(o.is_error), 0)                   AS error_count,
       COALESCE(SUM(o.end_ns IS NULL), 0)             AS open_count,
       SUM(o.input_tokens)                            AS input_tokens,
       SUM(o.output_tokens)                           AS output_tokens,
       SUM(o.cache_read_tokens)                       AS cache_read_tokens,
       SUM(o.cache_write_tokens)                      AS cache_write_tokens,
       SUM(o.total_tokens)                            AS total_tokens,
       SUM(o.total_cost_usd)                          AS total_cost_usd,
       COALESCE(SUM(o.type = 'generation' AND o.usage IS NOT NULL AND o.total_cost_usd IS NULL), 0) AS unpriced_generations,
       GROUP_CONCAT(DISTINCT o.model)                 AS models
FROM traces t LEFT JOIN observations o ON o.trace_id = t.id
GROUP BY t.rid;

CREATE VIEW session_stats AS
SELECT s.*,
       datetime(s.last_seen_ns / 1000000000, 'unixepoch') AS last_seen_at,
       COUNT(ts.rid)                                  AS turn_count,
       COALESCE(SUM(ts.status = 'open'), 0)           AS open_turns,
       MIN(ts.start_ns)                               AS first_turn_ns,
       MAX(COALESCE(ts.end_ns, ts.start_ns))          AS last_turn_ns,
       (MAX(COALESCE(ts.end_ns, ts.start_ns)) - MIN(ts.start_ns)) / 1000000 AS duration_ms,
       COALESCE(SUM(ts.observation_count), 0)         AS observation_count,
       COALESCE(SUM(ts.tool_count), 0)                AS tool_count,
       COALESCE(SUM(ts.error_count), 0)               AS error_count,
       SUM(ts.input_tokens)                           AS input_tokens,
       SUM(ts.output_tokens)                          AS output_tokens,
       SUM(ts.cache_read_tokens)                      AS cache_read_tokens,
       SUM(ts.cache_write_tokens)                     AS cache_write_tokens,
       SUM(ts.total_tokens)                           AS total_tokens,
       SUM(ts.total_cost_usd)                         AS total_cost_usd,
       COALESCE(SUM(ts.unpriced_generations), 0)      AS unpriced_generations,
       (SELECT MAX(reported_cost_usd) FROM launches l WHERE l.session_key = s.key) AS reported_cost_usd
FROM sessions s LEFT JOIN trace_stats ts ON ts.session_key = s.key
GROUP BY s.key;
"#;

/// Hook channel: one row per CLI lifecycle hook invocation.
const V2: &str = r#"
ALTER TABLE launches ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';
CREATE TABLE hook_events (
  id              INTEGER PRIMARY KEY,
  key             TEXT NOT NULL UNIQUE,
  provider        TEXT NOT NULL,
  session_id      TEXT NOT NULL,
  launch_id       TEXT,
  event           TEXT NOT NULL,
  ts_ns           INTEGER NOT NULL,
  cwd             TEXT,
  transcript_path TEXT,
  turn_key        TEXT,
  tool_use_id     TEXT,
  tool_name       TEXT,
  agent_id        TEXT,
  agent_type      TEXT,
  step_index      INTEGER,
  model           TEXT,
  is_error        INTEGER NOT NULL DEFAULT 0,
  payload         TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX hook_events_launch  ON hook_events (launch_id, id);
CREATE INDEX hook_events_session ON hook_events (provider, session_id, id);
CREATE INDEX hook_events_ts      ON hook_events (ts_ns DESC);
"#;
