//! Schema DDL, versioned through `PRAGMA user_version`. Migrations are
//! append-only: never edit a shipped entry, add a new one.

pub const SCHEMA_VERSION: i32 = 14;

pub const MIGRATIONS: &[&str] = &[V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11, V12, V13, V14];

// Preserve historical IDs and score targets; rebuilding a legacy session
// uses a separate database rather than silently replacing its history.
const V5: &str = r#"
UPDATE sessions SET extra = json_set(extra, '$.legacy_capture', 1)
WHERE EXISTS (SELECT 1 FROM traces WHERE traces.session_key = sessions.key);
CREATE INDEX observations_parent ON observations(parent_id);
CREATE INDEX traces_native_turn ON traces(session_key, json_extract(metadata, '$.native_turn_id'));
DROP VIEW trace_stats;
CREATE VIEW trace_stats AS
SELECT t.*,
       datetime(t.start_ns / 1000000000, 'unixepoch', 'localtime') AS started_at,
       (MAX(COALESCE(t.end_ns, t.start_ns), COALESCE(MAX(COALESCE(o.end_ns, o.start_ns)), t.start_ns), t.start_ns) - t.start_ns) / 1000000 AS latency_ms,
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
       GROUP_CONCAT(DISTINCT o.model)                 AS models,
       COALESCE(SUM(o.type = 'tool' AND trim(COALESCE(o.input, '')) <> ''), 0)
         - COUNT(DISTINCT CASE WHEN o.type = 'tool' AND trim(COALESCE(o.input, '')) <> ''
                               THEN o.name || char(0) || o.input END) AS retries,
       COALESCE(SUM(o.status_message = 'declined by the user'), 0) AS declined
FROM traces t LEFT JOIN observations o ON o.trace_id = t.id
GROUP BY t.rid;
"#;

// v6 keeps v5 readable while separating what the provider said from what
// agent-mux normalized and priced.  `usage` is retained solely as a legacy
// compatibility column; new writes fill the four explicit maps below.
const V6: &str = r#"
ALTER TABLE observations ADD COLUMN provided_usage TEXT;
ALTER TABLE observations ADD COLUMN provided_cost TEXT;
ALTER TABLE observations ADD COLUMN usage_details TEXT;
ALTER TABLE observations ADD COLUMN cost_details TEXT;
UPDATE observations SET provided_usage = usage WHERE usage IS NOT NULL;
UPDATE observations
SET usage_details = json_object(
  'input', input_tokens,
  'output', output_tokens,
  'cache_read_input_tokens', cache_read_tokens,
  'input_cache_creation_5m', cache_write_tokens,
  'input_cache_creation_1h', cache_write_1h_tokens,
  'output_reasoning_tokens', reasoning_tokens,
  'total', total_tokens
)
WHERE usage IS NOT NULL;
UPDATE observations
SET cost_details = json_object(
  'input', input_cost_usd,
  'output', output_cost_usd,
  'cache_read_input_tokens', cache_read_cost_usd,
  'input_cache_creation_5m', cache_write_cost_usd,
  'total', total_cost_usd
)
WHERE total_cost_usd IS NOT NULL;
CREATE INDEX observations_provided_usage ON observations(provided_usage) WHERE provided_usage IS NOT NULL;
"#;

const V7: &str = r#"
CREATE TABLE scores_v7 (
  id           INTEGER PRIMARY KEY,
  target       TEXT NOT NULL CHECK (target IN ('trace','observation','session','launch')),
  target_id    TEXT NOT NULL,
  name         TEXT NOT NULL,
  data_type    TEXT NOT NULL CHECK (data_type IN ('numeric','categorical','boolean','text','correction')),
  source       TEXT NOT NULL CHECK (source IN ('api','eval','annotation')),
  value        REAL,
  string_value TEXT,
  comment      TEXT,
  created_ns   INTEGER NOT NULL,
  CHECK ((data_type = 'numeric' AND value IS NOT NULL) OR (data_type != 'numeric' AND string_value IS NOT NULL))
);
INSERT INTO scores_v7 (id, target, target_id, name, data_type, source, value, comment, created_ns)
SELECT id, target, target_id, name, 'numeric', 'annotation', value, comment, created_ns FROM scores;
DROP TABLE scores;
ALTER TABLE scores_v7 RENAME TO scores;
CREATE INDEX scores_by_target ON scores (target, target_id);
DROP VIEW trace_stats;
CREATE VIEW trace_stats AS
SELECT t.*,
       datetime(t.start_ns / 1000000000, 'unixepoch', 'localtime') AS started_at,
       (MAX(COALESCE(t.end_ns, t.start_ns), COALESCE(MAX(COALESCE(o.end_ns, o.start_ns)), t.start_ns), t.start_ns) - t.start_ns) / 1000000 AS latency_ms,
       COUNT(o.rid) AS observation_count,
       COALESCE(SUM(o.type = 'generation'), 0) AS generation_count,
       COALESCE(SUM(o.type IN ('tool','agent')), 0) AS tool_count,
       COALESCE(SUM(o.level = 'ERROR'), 0) AS error_count,
       COALESCE(SUM(o.end_ns IS NULL), 0) AS open_count,
       SUM(o.input_tokens) AS input_tokens,
       SUM(o.output_tokens) AS output_tokens,
       SUM(o.cache_read_tokens) AS cache_read_tokens,
       SUM(o.cache_write_tokens) AS cache_write_tokens,
       SUM(o.total_tokens) AS total_tokens,
       SUM(o.total_cost_usd) AS total_cost_usd,
       COALESCE(SUM(o.type = 'generation' AND o.usage_details IS NOT NULL AND o.total_cost_usd IS NULL), 0) AS unpriced_generations,
       GROUP_CONCAT(DISTINCT o.model) AS models,
       COALESCE(SUM(o.type = 'tool' AND trim(COALESCE(o.input, '')) <> ''), 0)
         - COUNT(DISTINCT CASE WHEN o.type = 'tool' AND trim(COALESCE(o.input, '')) <> '' THEN o.name || char(0) || o.input END) AS retries,
       COALESCE(SUM(o.status_message = 'declined by the user'), 0) AS declined
FROM traces t LEFT JOIN observations o ON o.trace_id = t.id
GROUP BY t.rid;
"#;

// The column was only a workaround for positional turn ids. v8 removes it
// after all new capture paths use native/content-derived keys.
const V8: &str = r#"
ALTER TABLE traces DROP COLUMN ordinal_salted;
"#;

const V9: &str = r#"
UPDATE observations SET level = 'ERROR' WHERE is_error = 1;
DROP VIEW loop_stats;
DROP VIEW agent_stats;
ALTER TABLE observations DROP COLUMN is_error;
CREATE VIEW loop_stats AS
SELECT t.id AS trace_id, t.session_key, t.ordinal,
       COALESCE(SUM(o.type = 'tool'), 0) AS tool_calls,
       COUNT(DISTINCT CASE WHEN o.type = 'tool' THEN o.name END) AS distinct_tools,
       COALESCE(SUM(o.type = 'tool' AND o.level = 'ERROR'), 0) AS tool_errors,
       COALESCE(SUM(o.status_message = 'declined by the user'), 0) AS declined,
       COALESCE(SUM(o.type = 'agent'), 0) AS subagents,
       COALESCE(json_extract(t.metadata, '$.compacted'), 0) AS compacted,
       SUM(CASE WHEN o.type = 'generation' THEN o.input_tokens END) AS input_tokens,
       SUM(CASE WHEN o.type = 'generation' THEN o.cache_read_tokens END) AS cache_read_tokens
FROM traces t LEFT JOIN observations o ON o.trace_id = t.id GROUP BY t.rid;
CREATE VIEW agent_stats AS
SELECT agent_type, COUNT(*) AS invocations, AVG(dur_ms) AS mean_ms, MAX(dur_ms) AS max_ms,
       SUM(tokens) AS tokens, SUM(cost) AS cost, SUM(failed) AS failures
FROM (
  SELECT COALESCE(json_extract(a.metadata, '$.agent_type'), a.name) AS agent_type,
         (COALESCE(a.end_ns, a.start_ns) - a.start_ns) / 1000000 AS dur_ms,
         COALESCE(a.total_tokens, 0) + COALESCE((SELECT SUM(c.total_tokens) FROM observations c WHERE c.parent_id = a.id), 0) AS tokens,
         COALESCE(a.total_cost_usd, 0) + COALESCE((SELECT SUM(c.total_cost_usd) FROM observations c WHERE c.parent_id = a.id), 0) AS cost,
         (a.level = 'ERROR' OR EXISTS (SELECT 1 FROM observations c WHERE c.parent_id = a.id AND c.level = 'ERROR')) AS failed
  FROM observations a WHERE a.type = 'agent'
) GROUP BY agent_type;
"#;

// v10 was issued by the discarded workbench editor experiment.  It added
// tables unrelated to trace capture, while leaving every tracing table,
// view, and index at the v9 shape.  Treat it as a compatibility marker so a
// trace-only build can continue using an existing v10 store without either
// downgrading it or recreating the user's trace history.
const V10: &str = r#"
SELECT 1;
"#;

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
       datetime(t.start_ns / 1000000000, 'unixepoch', 'localtime') AS started_at,
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
       datetime(s.last_seen_ns / 1000000000, 'unixepoch', 'localtime') AS last_seen_at,
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

/// Workbench, phase 1: derived views over existing rows. Nothing is
/// stored that a trace does not already say.
const V3: &str = r#"
DROP VIEW trace_stats;
CREATE VIEW trace_stats AS
SELECT t.*,
       datetime(t.start_ns / 1000000000, 'unixepoch', 'localtime') AS started_at,
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
       GROUP_CONCAT(DISTINCT o.model)                 AS models,
       COALESCE(SUM(o.type = 'tool' AND trim(COALESCE(o.input, '')) <> ''), 0)
         - COUNT(DISTINCT CASE WHEN o.type = 'tool' AND trim(COALESCE(o.input, '')) <> ''
                               THEN o.name || char(0) || o.input END) AS retries,
       COALESCE(SUM(o.status_message = 'declined by the user'), 0) AS declined
FROM traces t LEFT JOIN observations o ON o.trace_id = t.id
GROUP BY t.rid;

CREATE VIEW loop_stats AS
SELECT t.id                                             AS trace_id,
       t.session_key,
       t.ordinal,
       COALESCE(SUM(o.type = 'tool'), 0)                AS tool_calls,
       COUNT(DISTINCT CASE WHEN o.type = 'tool' THEN o.name END) AS distinct_tools,
       COALESCE(SUM(o.type = 'tool' AND o.is_error), 0) AS tool_errors,
       COALESCE(SUM(o.status_message = 'declined by the user'), 0) AS declined,
       COALESCE(SUM(o.type = 'agent'), 0)               AS subagents,
       COALESCE(json_extract(t.metadata, '$.compacted'), 0) AS compacted,
       SUM(CASE WHEN o.type = 'generation' THEN o.input_tokens END)      AS input_tokens,
       SUM(CASE WHEN o.type = 'generation' THEN o.cache_read_tokens END) AS cache_read_tokens
FROM traces t LEFT JOIN observations o ON o.trace_id = t.id
GROUP BY t.rid;

CREATE VIEW skill_stats AS
WITH loaded AS (
  SELECT t.id AS trace_id, t.start_ns, j.value AS skill
  FROM traces t, json_each(t.skills) j
),
used AS (
  SELECT o.trace_id, o.skill,
         SUM(o.type = 'generation')          AS generations,
         SUM(o.type IN ('tool', 'agent'))    AS tools,
         SUM(o.total_tokens)                 AS tokens,
         SUM(o.total_cost_usd)               AS cost
  FROM observations o WHERE o.skill IS NOT NULL
  GROUP BY o.trace_id, o.skill
)
SELECT l.skill,
       COUNT(DISTINCT l.trace_id)            AS turns_loaded,
       COALESCE(SUM(u.generations), 0)       AS generations,
       COALESCE(SUM(u.tools), 0)             AS tools,
       SUM(u.tokens)                         AS tokens,
       SUM(u.cost)                           AS cost,
       COALESCE(SUM(u.trace_id IS NULL), 0)  AS turns_unused,
       MIN(l.start_ns)                       AS first_ns,
       MAX(l.start_ns)                       AS last_ns
FROM loaded l LEFT JOIN used u ON u.trace_id = l.trace_id AND u.skill = l.skill
GROUP BY l.skill;

CREATE VIEW agent_stats AS
SELECT agent_type,
       COUNT(*)      AS invocations,
       AVG(dur_ms)   AS mean_ms,
       MAX(dur_ms)   AS max_ms,
       SUM(tokens)   AS tokens,
       SUM(cost)     AS cost,
       SUM(failed)   AS failures
FROM (
  SELECT COALESCE(json_extract(a.metadata, '$.agent_type'), a.name) AS agent_type,
         (COALESCE(a.end_ns, a.start_ns) - a.start_ns) / 1000000     AS dur_ms,
         COALESCE(a.total_tokens, 0)
           + COALESCE((SELECT SUM(c.total_tokens) FROM observations c WHERE c.parent_id = a.id), 0) AS tokens,
         COALESCE(a.total_cost_usd, 0)
           + COALESCE((SELECT SUM(c.total_cost_usd) FROM observations c WHERE c.parent_id = a.id), 0) AS cost,
         (a.is_error OR EXISTS (SELECT 1 FROM observations c WHERE c.parent_id = a.id AND c.is_error)) AS failed
  FROM observations a WHERE a.type = 'agent'
)
GROUP BY agent_type;
"#;

/// Workbench, phase 2: experiments (a task run under variants, each run a
/// labelled launch) and scores (a person's verdict on a turn or launch).
const V4: &str = r#"
CREATE TABLE experiments (
  id         TEXT PRIMARY KEY,
  name       TEXT NOT NULL UNIQUE,
  prompt     TEXT NOT NULL,
  cwd        TEXT,
  check_cmd  TEXT,
  created_ns INTEGER NOT NULL,
  notes      TEXT
);
CREATE TABLE experiment_runs (
  launch_id     TEXT PRIMARY KEY REFERENCES launches (id),
  experiment_id TEXT NOT NULL REFERENCES experiments (id),
  variant       TEXT NOT NULL,
  outcome       TEXT NOT NULL CHECK (outcome IN ('pass','fail','unknown')),
  detail        TEXT NOT NULL DEFAULT '{}',
  recorded_ns   INTEGER NOT NULL
);
CREATE INDEX experiment_runs_by_experiment ON experiment_runs (experiment_id, variant);
CREATE TABLE scores (
  id         INTEGER PRIMARY KEY,
  target     TEXT NOT NULL CHECK (target IN ('trace','session','launch')),
  target_id  TEXT NOT NULL,
  name       TEXT NOT NULL,
  value      REAL NOT NULL,
  comment    TEXT,
  created_ns INTEGER NOT NULL
);
CREATE INDEX scores_by_target ON scores (target, target_id);
"#;

const V11: &str = r#"
CREATE TABLE IF NOT EXISTS trace_changes (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  entity_kind TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  session_key TEXT,
  launch_id TEXT,
  operation TEXT NOT NULL CHECK(operation IN ('insert','update','delete'))
);

CREATE INDEX IF NOT EXISTS idx_trace_changes_seq ON trace_changes(seq);
CREATE INDEX IF NOT EXISTS idx_trace_changes_session ON trace_changes(session_key);
CREATE INDEX IF NOT EXISTS idx_trace_changes_entity ON trace_changes(entity_kind, entity_id);

INSERT OR IGNORE INTO meta(key, value) VALUES ('store_uuid', lower(hex(randomblob(16))));

-- Triggers for sessions
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_sessions_insert
AFTER INSERT ON sessions
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('session', NEW.key, NEW.key, NULL, 'insert');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_sessions_update
AFTER UPDATE ON sessions
WHEN NEW.last_seen_ns IS NOT OLD.last_seen_ns OR NEW.provider IS NOT OLD.provider OR NEW.cwd IS NOT OLD.cwd
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('session', NEW.key, NEW.key, NULL, 'update');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_sessions_delete
AFTER DELETE ON sessions
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('session', OLD.key, OLD.key, NULL, 'delete');
END;

-- Triggers for launches
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_launches_insert
AFTER INSERT ON launches
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('launch', NEW.id, NEW.session_key, NEW.id, 'insert');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_launches_update
AFTER UPDATE ON launches
WHEN NEW.ended_ns IS NOT OLD.ended_ns OR NEW.reported_cost_usd IS NOT OLD.reported_cost_usd OR NEW.termination IS NOT OLD.termination
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('launch', NEW.id, NEW.session_key, NEW.id, 'update');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_launches_delete
AFTER DELETE ON launches
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('launch', OLD.id, OLD.session_key, OLD.id, 'delete');
END;

-- Triggers for traces
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_traces_insert
AFTER INSERT ON traces
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('trace', NEW.id, NEW.session_key, NEW.launch_id, 'insert');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_traces_update
AFTER UPDATE ON traces
WHEN NEW.end_ns IS NOT OLD.end_ns OR NEW.status IS NOT OLD.status OR NEW.session_cost_usd IS NOT OLD.session_cost_usd OR NEW.output IS NOT OLD.output
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('trace', NEW.id, NEW.session_key, NEW.launch_id, 'update');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_traces_delete
AFTER DELETE ON traces
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('trace', OLD.id, OLD.session_key, OLD.launch_id, 'delete');
END;

-- Triggers for observations
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_observations_insert
AFTER INSERT ON observations
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES (
    'observation',
    NEW.id,
    (SELECT session_key FROM traces WHERE id = NEW.trace_id),
    (SELECT launch_id FROM traces WHERE id = NEW.trace_id),
    'insert'
  );
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_observations_update
AFTER UPDATE ON observations
WHEN NEW.end_ns IS NOT OLD.end_ns OR NEW.level IS NOT OLD.level OR NEW.total_tokens IS NOT OLD.total_tokens OR NEW.total_cost_usd IS NOT OLD.total_cost_usd OR NEW.output IS NOT OLD.output
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES (
    'observation',
    NEW.id,
    (SELECT session_key FROM traces WHERE id = NEW.trace_id),
    (SELECT launch_id FROM traces WHERE id = NEW.trace_id),
    'update'
  );
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_observations_delete
AFTER DELETE ON observations
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES (
    'observation',
    OLD.id,
    (SELECT session_key FROM traces WHERE id = OLD.trace_id),
    (SELECT launch_id FROM traces WHERE id = OLD.trace_id),
    'delete'
  );
END;
"#;

// Loop Engineering: one row per scheduled loop run (`agent-mux loop …`,
// the Loops sidebar). Blocked runs are rows too, so the day's caps count
// them; they never reach `loop-run-log.md`.
const V12: &str = r#"
CREATE TABLE IF NOT EXISTS loop_runs (
  id               TEXT PRIMARY KEY,
  loop_id          TEXT NOT NULL,
  workspace        TEXT NOT NULL,
  pattern          TEXT NOT NULL,
  harness          TEXT NOT NULL,
  level            TEXT NOT NULL CHECK (level IN ('L1','L2','L3')),
  effective_level  TEXT NOT NULL CHECK (effective_level IN ('L1','L2','L3')),
  launch_id        TEXT REFERENCES launches (id),
  scheduled_ns     INTEGER NOT NULL,
  started_ns       INTEGER,
  ended_ns         INTEGER,
  outcome          TEXT NOT NULL CHECK (outcome IN ('report-only','fix-proposed','escalated','no-op','blocked','failed')),
  items_found      INTEGER,
  actions_taken    INTEGER,
  escalations      INTEGER,
  tokens           INTEGER,
  cost_usd         REAL,
  readiness_score  INTEGER,
  worktree         TEXT,
  branch           TEXT,
  decision         TEXT CHECK (decision IN ('applied','rejected')),
  decided_ns       INTEGER,
  detail           TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS loop_runs_by_loop ON loop_runs (loop_id, scheduled_ns DESC);
CREATE INDEX IF NOT EXISTS loop_runs_inbox ON loop_runs (outcome) WHERE decision IS NULL;
CREATE VIEW IF NOT EXISTS loop_run_stats AS
SELECT loop_id, pattern, workspace,
       COUNT(*)                                            AS runs,
       SUM(outcome = 'fix-proposed')                       AS fixes_proposed,
       SUM(outcome = 'escalated')                          AS escalated,
       SUM(outcome = 'blocked')                            AS blocked,
       SUM(outcome = 'failed')                             AS failed,
       COALESCE(SUM(tokens), 0)                            AS tokens,
       COALESCE(SUM(cost_usd), 0)                          AS cost_usd,
       MAX(started_ns)                                     AS last_started_ns
FROM loop_runs GROUP BY loop_id;
"#;

// v13: Workflows. One row per run and one per session (`workflow_steps`),
// written by the App; `launches.metadata` carries `workflow_run_id`,
// `workflow_step` and `workflow_phase` for the same sessions.
const V13: &str = r#"
CREATE TABLE IF NOT EXISTS workflow_runs (
  id             TEXT PRIMARY KEY,
  workflow       TEXT NOT NULL,
  source         TEXT NOT NULL,
  document_hash  TEXT NOT NULL,
  document       TEXT NOT NULL,
  workspace      TEXT NOT NULL,
  harness        TEXT NOT NULL,
  profile        TEXT NOT NULL,
  args           TEXT NOT NULL DEFAULT 'null',
  budget_tokens  INTEGER,
  started_ns     INTEGER NOT NULL,
  ended_ns       INTEGER,
  status         TEXT NOT NULL CHECK (status IN ('running','finished','failed','cancelled','budget-exhausted')),
  sessions       INTEGER NOT NULL DEFAULT 0,
  tokens         INTEGER,
  cost_usd       REAL,
  result         TEXT NOT NULL DEFAULT 'null',
  error          TEXT,
  resumed_from   TEXT
);
CREATE INDEX IF NOT EXISTS workflow_runs_recent ON workflow_runs (started_ns DESC);
CREATE TABLE IF NOT EXISTS workflow_steps (
  run_id         TEXT NOT NULL REFERENCES workflow_runs (id),
  session        TEXT NOT NULL,
  step_id        TEXT NOT NULL,
  item           TEXT NOT NULL DEFAULT 'null',
  launch_id      TEXT,
  phase          TEXT NOT NULL,
  harness        TEXT NOT NULL,
  kind           TEXT NOT NULL,
  started_ns     INTEGER,
  ended_ns       INTEGER,
  tokens         INTEGER,
  cost_usd       REAL,
  worktree       TEXT,
  changed_files  TEXT NOT NULL DEFAULT 'null',
  result         TEXT NOT NULL DEFAULT 'null',
  PRIMARY KEY (run_id, session)
);
"#;

// v14: which pattern text a loop run executed. `loop_runs.pattern` is the
// pattern id, and `~/.agent-mux/loops/registry.toml` can replace that id's
// text between two runs, so the id alone cannot tell two runs apart;
// `workflow_runs.document_hash` already pins a workflow document this way.
//
// A side table rather than `ALTER TABLE loop_runs ADD COLUMN`: SQLite has
// no `ADD COLUMN IF NOT EXISTS`, and every migration from v11 on is
// replay-safe (`open` re-runs `version..SCHEMA_VERSION` against whatever
// schema the file actually has). Rows written before this migration simply
// have no entry: their pattern text was never recorded, and backfilling
// from today's registry would assert something untrue.
const V14: &str = r#"
CREATE TABLE IF NOT EXISTS loop_run_patterns (
  run_id        TEXT PRIMARY KEY REFERENCES loop_runs (id) ON DELETE CASCADE,
  pattern_hash  TEXT NOT NULL
);
"#;
