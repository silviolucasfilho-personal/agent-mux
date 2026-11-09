//! Schema DDL, versioned through `PRAGMA user_version`. Migrations are
//! append-only: never edit a shipped entry, add a new one.

pub const SCHEMA_VERSION: i32 = 26;

pub const MIGRATIONS: &[&str] = &[
    V1, V2, V3, V4, V5, V6, V7, V8, V9, V10, V11, V12, V13, V14, V15, V16, V17, V18, V19, V20, V21,
    V22, V23, V24, V25, V26,
];

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

// v15: the FTS update triggers fire only when the text changed. The
// observation upsert always names `input` and `output` in its SET clause
// (`COALESCE(excluded.input, input)`), so `AFTER UPDATE OF input, output`
// fired on every re-upsert: an in-flight tool closing, a hook pin, an
// Antigravity usage row — each deleted and re-inserted the FTS entry for
// text that had not changed. `WHEN` keeps the index work to real edits.
// Replay-safe: the triggers are dropped and recreated.
const V15: &str = r#"
DROP TRIGGER IF EXISTS observations_fts_au;
CREATE TRIGGER observations_fts_au AFTER UPDATE OF input, output ON observations
WHEN old.input IS NOT new.input OR old.output IS NOT new.output BEGIN
  INSERT INTO observations_fts (observations_fts, rowid, input, output) VALUES ('delete', old.rid, old.input, old.output);
  INSERT INTO observations_fts (rowid, input, output) VALUES (new.rid, new.input, new.output);
END;
DROP TRIGGER IF EXISTS traces_fts_au;
CREATE TRIGGER traces_fts_au AFTER UPDATE OF input, output ON traces
WHEN old.input IS NOT new.input OR old.output IS NOT new.output BEGIN
  INSERT INTO traces_fts (traces_fts, rowid, input, output) VALUES ('delete', old.rid, old.input, old.output);
  INSERT INTO traces_fts (rowid, input, output) VALUES (new.rid, new.input, new.output);
END;
"#;

// v16: Inter-agent communication & task coordination bus (`src/bus/`).
// Replay-safe: tables and indexes use IF NOT EXISTS.
const V16: &str = r#"
CREATE TABLE IF NOT EXISTS agent_messages (
  id            TEXT PRIMARY KEY,
  workspace     TEXT NOT NULL,
  from_launch   TEXT NOT NULL,
  to_launch     TEXT,
  topic         TEXT NOT NULL DEFAULT 'message',
  body          TEXT NOT NULL,
  reply_to      TEXT,
  created_ns    INTEGER NOT NULL,
  delivered_ns  INTEGER,
  acked_ns      INTEGER
);
CREATE INDEX IF NOT EXISTS agent_messages_inbox ON agent_messages (workspace, to_launch, created_ns ASC);
CREATE INDEX IF NOT EXISTS agent_messages_from ON agent_messages (from_launch, created_ns DESC);

CREATE TABLE IF NOT EXISTS agent_tasks (
  id              TEXT PRIMARY KEY,
  workspace       TEXT NOT NULL,
  created_by      TEXT NOT NULL,
  parent_task_id  TEXT,
  assigned_to     TEXT,
  target_harness  TEXT,
  target_agent    TEXT,
  title           TEXT NOT NULL,
  instructions    TEXT NOT NULL,
  files_json      TEXT NOT NULL DEFAULT '[]',
  isolation       TEXT NOT NULL DEFAULT 'shared' CHECK (isolation IN ('shared','worktree')),
  status          TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open','claimed','running','done','failed','cancelled')),
  result_json     TEXT,
  error           TEXT,
  worktree        TEXT,
  branch          TEXT,
  created_ns      INTEGER NOT NULL,
  updated_ns      INTEGER NOT NULL,
  ended_ns        INTEGER
);
CREATE INDEX IF NOT EXISTS agent_tasks_workspace ON agent_tasks (workspace, status, created_ns ASC);
CREATE INDEX IF NOT EXISTS agent_tasks_assigned ON agent_tasks (assigned_to, status);

CREATE TABLE IF NOT EXISTS agent_leases (
  workspace     TEXT NOT NULL,
  pattern       TEXT NOT NULL,
  owner_launch  TEXT NOT NULL,
  task_id       TEXT REFERENCES agent_tasks (id) ON DELETE CASCADE,
  acquired_ns   INTEGER NOT NULL,
  expires_ns    INTEGER NOT NULL,
  PRIMARY KEY (workspace, pattern)
);
CREATE INDEX IF NOT EXISTS agent_leases_owner ON agent_leases (owner_launch);
"#;

// v17: the swarm's own state (`src/bus/swarm.rs`). Which launch leads a
// workspace's swarm, the goal and the parameters it was started with. Before
// this the coordinator was recorded as a `swarm-coordinator` broadcast on
// `agent_messages`, which every worker then received as unread mail and whose
// body doubled as the goal; those rows carried nothing else, so they go.
// Replay-safe: IF NOT EXISTS, and the DELETE matches nothing the second time.
const V17: &str = r#"
CREATE TABLE IF NOT EXISTS swarm_state (
  workspace          TEXT PRIMARY KEY,
  coordinator_launch TEXT NOT NULL,
  goal               TEXT,
  max_workers        INTEGER,
  isolation          TEXT CHECK (isolation IN ('shared','worktree')),
  started_ns         INTEGER NOT NULL,
  updated_ns         INTEGER NOT NULL
);
DELETE FROM agent_messages WHERE topic = 'swarm-coordinator';
"#;

// v18: the bus tables feed `trace_changes`, so a screen that shows them (the
// Swarm Control Room) can ask `latest_change_seq` whether anything moved
// instead of re-reading the board, the leases and the feed every tick.
// Replay-safe: IF NOT EXISTS throughout.
const V18: &str = r#"
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_agent_messages_insert
AFTER INSERT ON agent_messages
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('bus_message', NEW.id, NULL, NEW.from_launch, 'insert');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_agent_messages_update
AFTER UPDATE ON agent_messages
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('bus_message', NEW.id, NULL, NEW.from_launch, 'update');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_agent_tasks_insert
AFTER INSERT ON agent_tasks
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('bus_task', NEW.id, NULL, NEW.assigned_to, 'insert');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_agent_tasks_update
AFTER UPDATE ON agent_tasks
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('bus_task', NEW.id, NULL, NEW.assigned_to, 'update');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_agent_leases_insert
AFTER INSERT ON agent_leases
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('bus_lease', NEW.workspace || char(0) || NEW.pattern, NULL, NEW.owner_launch, 'insert');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_agent_leases_delete
AFTER DELETE ON agent_leases
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('bus_lease', OLD.workspace || char(0) || OLD.pattern, NULL, OLD.owner_launch, 'delete');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_state_insert
AFTER INSERT ON swarm_state
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm', NEW.workspace, NULL, NEW.coordinator_launch, 'insert');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_state_update
AFTER UPDATE ON swarm_state
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm', NEW.workspace, NULL, NEW.coordinator_launch, 'update');
END;

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_state_delete
AFTER DELETE ON swarm_state
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm', OLD.workspace, NULL, OLD.coordinator_launch, 'delete');
END;
"#;

// v19: the swarm's plan gate and its review of finished branches
// (`bus::store::set_plan_gate`, `record_review`). Side tables rather than
// columns on `swarm_state`, so the migration stays replay-safe (no
// `ADD COLUMN IF NOT EXISTS` in SQLite). Both feed `trace_changes` so the
// swarm screen refreshes on a decision.
const V19: &str = r#"
CREATE TABLE IF NOT EXISTS swarm_plans (
  workspace   TEXT PRIMARY KEY,
  gate        INTEGER NOT NULL DEFAULT 1,
  status      TEXT NOT NULL DEFAULT 'drafting' CHECK (status IN ('drafting','approved','sent-back')),
  standby     INTEGER NOT NULL DEFAULT 0,
  decided_ns  INTEGER,
  updated_ns  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS swarm_reviews (
  task_id     TEXT PRIMARY KEY,
  workspace   TEXT NOT NULL,
  verdict     TEXT NOT NULL CHECK (verdict IN ('merged','discarded')),
  branch      TEXT,
  detail      TEXT,
  decided_ns  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS swarm_reviews_workspace ON swarm_reviews (workspace);

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_plans_insert
AFTER INSERT ON swarm_plans
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_plan', NEW.workspace, NULL, NULL, 'insert');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_plans_update
AFTER UPDATE ON swarm_plans
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_plan', NEW.workspace, NULL, NULL, 'update');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_plans_delete
AFTER DELETE ON swarm_plans
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_plan', OLD.workspace, NULL, NULL, 'delete');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_reviews_insert
AFTER INSERT ON swarm_reviews
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_review', NEW.task_id, NULL, NULL, 'insert');
END;
"#;

// v20: OpenCode as a fourth provider. `sessions.provider` is a CHECK
// constraint, which SQLite cannot alter in place, so the table is rebuilt
// with the same columns and `'opencode'` admitted; the rows, the two
// indexes, the three `trace_changes` triggers and the `session_stats` view
// (which SQLite validates on every schema change, so it has to go first
// and come back last, as defined in v1) are restored as they were. The
// migration runner (`store::migrate`) switches foreign keys off around the
// migrations, so dropping the table under the `traces` and `launches` rows
// that reference `sessions (key)` is allowed, and since they reference it
// by name they point at the rebuilt table afterwards. Replay-safe: a second run copies the rows into a
// fresh `sessions_v20`, swaps again, and every index, trigger and view is
// dropped before it is created.
const V20: &str = r#"
CREATE TABLE IF NOT EXISTS sessions_v20 (
  key             TEXT PRIMARY KEY,
  provider        TEXT NOT NULL CHECK (provider IN ('claude','codex','antigravity','opencode')),
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
INSERT OR IGNORE INTO sessions_v20 SELECT key, provider, session_id, user_id, cwd, project_slug, transcript_path, title, first_seen_ns, last_seen_ns, extra FROM sessions;
DROP VIEW IF EXISTS session_stats;
DROP TRIGGER IF EXISTS trg_trace_changes_sessions_insert;
DROP TRIGGER IF EXISTS trg_trace_changes_sessions_update;
DROP TRIGGER IF EXISTS trg_trace_changes_sessions_delete;
DROP INDEX IF EXISTS sessions_last_seen;
DROP INDEX IF EXISTS sessions_project;
DROP TABLE sessions;
ALTER TABLE sessions_v20 RENAME TO sessions;
CREATE INDEX IF NOT EXISTS sessions_last_seen ON sessions (last_seen_ns DESC);
CREATE INDEX IF NOT EXISTS sessions_project   ON sessions (project_slug, last_seen_ns DESC);
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
"#;

// v21: agent-mux as the swarm's orchestrator
// (`docs/superpowers/specs/2026-11-02-swarm-orchestrator-design.md`).
// `swarms` is one swarm's run (a workspace has at most one live: ended_ns
// NULL), `swarm_tasks` extends the board's `agent_tasks` rows with the
// plan's key, order, dependencies and acceptance. The coordinator-session
// swarms of v17/v19 (`swarm_state`, `swarm_plans`) are ended: their open
// tasks cancelled, their leases released, their rows deleted; the tables
// stay, unused. Replay-safe: every statement is idempotent.
const V21: &str = r#"
CREATE TABLE IF NOT EXISTS swarms (
  id              TEXT PRIMARY KEY,
  workspace       TEXT NOT NULL,
  goal            TEXT NOT NULL,
  status          TEXT NOT NULL CHECK (status IN
                  ('planning','review','running','paused','integrating','finished')),
  planner_harness TEXT NOT NULL,
  planner_launch  TEXT,
  plan_summary    TEXT,
  plan_error      TEXT,
  max_workers     INTEGER NOT NULL,
  isolation       TEXT NOT NULL CHECK (isolation IN ('shared','worktree')),
  gate            INTEGER NOT NULL DEFAULT 1,
  check_cmd       TEXT,
  check_result    TEXT,
  owner_run       TEXT,
  started_ns      INTEGER NOT NULL,
  updated_ns      INTEGER NOT NULL,
  ended_ns        INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS swarms_live ON swarms (workspace) WHERE ended_ns IS NULL;

CREATE TABLE IF NOT EXISTS swarm_tasks (
  task_id         TEXT PRIMARY KEY REFERENCES agent_tasks (id) ON DELETE CASCADE,
  swarm_id        TEXT NOT NULL REFERENCES swarms (id) ON DELETE CASCADE,
  plan_key        TEXT NOT NULL,
  position        INTEGER NOT NULL,
  depends_on_json TEXT NOT NULL DEFAULT '[]',
  acceptance      TEXT,
  proposed        INTEGER NOT NULL DEFAULT 0,
  attempts        INTEGER NOT NULL DEFAULT 0,
  nudged_ns       INTEGER,
  UNIQUE (swarm_id, plan_key)
);
CREATE INDEX IF NOT EXISTS swarm_tasks_swarm ON swarm_tasks (swarm_id, position);

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarms_insert
AFTER INSERT ON swarms
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_run', NEW.id, NULL, NULL, 'insert');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarms_update
AFTER UPDATE ON swarms
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_run', NEW.id, NULL, NULL, 'update');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_tasks_insert
AFTER INSERT ON swarm_tasks
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_task', NEW.task_id, NULL, NULL, 'insert');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_tasks_update
AFTER UPDATE ON swarm_tasks
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_task', NEW.task_id, NULL, NULL, 'update');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_tasks_delete
AFTER DELETE ON swarm_tasks
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_task', OLD.task_id, NULL, NULL, 'delete');
END;

UPDATE agent_tasks
   SET status = 'cancelled',
       error = COALESCE(error, 'ended by the v21 migration: swarms are now run by agent-mux'),
       updated_ns = CAST((julianday('now') - 2440587.5) * 86400000000000 AS INTEGER),
       ended_ns = CAST((julianday('now') - 2440587.5) * 86400000000000 AS INTEGER)
 WHERE status IN ('open','claimed','running')
   AND workspace IN (SELECT workspace FROM swarm_state UNION SELECT workspace FROM swarm_plans);
DELETE FROM agent_leases
 WHERE workspace IN (SELECT workspace FROM swarm_state UNION SELECT workspace FROM swarm_plans);
DELETE FROM swarm_state;
DELETE FROM swarm_plans;
"#;

// v22: the swarm's coordinator agent
// (`docs/superpowers/specs/2026-11-02-swarm-agents-layout-design.md` §3, §7).
// A side table rather than columns on `swarms`, so the migration stays
// replay-safe (no `ADD COLUMN IF NOT EXISTS` in SQLite): the coordinator's
// live launch, the workers' default harness (`swarms.planner_harness` is
// the coordinator's), and the batching of the events it is told about.
const V22: &str = r#"
CREATE TABLE IF NOT EXISTS swarm_coordinators (
  swarm_id        TEXT PRIMARY KEY REFERENCES swarms (id) ON DELETE CASCADE,
  launch          TEXT,
  worker_harness  TEXT,
  events_seen_ns  INTEGER NOT NULL DEFAULT 0,
  last_wake_ns    INTEGER,
  updated_ns      INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_coordinators_insert
AFTER INSERT ON swarm_coordinators
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_coordinator', NEW.swarm_id, NULL, NEW.launch, 'insert');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_coordinators_update
AFTER UPDATE ON swarm_coordinators
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_coordinator', NEW.swarm_id, NULL, NEW.launch, 'update');
END;
"#;

// v23: several live swarms per workspace. The unique `swarms_live` index
// held a workspace to one swarm, so a stuck swarm kept a new one from
// starting; a swarm is now found by its id (the tree's row, a task's
// `swarm_tasks` row, a coordinator's launch), and a plain index keeps the
// workspace's lookup fast. `swarm_coordinators.worker_harness` holds the
// workers' pool, comma separated, its first harness the default.
const V23: &str = r#"
DROP INDEX IF EXISTS swarms_live;
CREATE INDEX IF NOT EXISTS swarms_workspace_live ON swarms (workspace, ended_ns);
"#;

// v24: the coordinator-first swarm
// (`docs/superpowers/specs/2026-11-05-swarm-coordinator-first-design.md`
// §8). Side tables rather than `ADD COLUMN`, as v14 and v22, so a replay is
// harmless. `swarm_settings` holds the gate mode (`chat`: the coordinator
// relays the operator's yes from its chat; `off`: a plan without errors
// runs at once) and whether the swarm runs headless; the old `swarms.gate`
// flag is backfilled into it. `swarm_dispatch` is the engine's per-task
// bookkeeping: when a ready task was offered to the coordinator and who
// asked for its worker (`coordinator`, `dispatcher` after the grace period,
// `operator`). `swarm_decisions` records every operator-level decision with
// who relayed it (`chat`, `tui`, `cli`) and the operator's words when the
// coordinator relayed them. `swarm_events` is the swarm's log: what Activity
// derived before, with its actor, read by the screen, the CLI and the
// Langfuse export.
const V24: &str = r#"
CREATE TABLE IF NOT EXISTS swarm_settings (
  swarm_id    TEXT PRIMARY KEY REFERENCES swarms (id) ON DELETE CASCADE,
  gate_mode   TEXT NOT NULL DEFAULT 'chat' CHECK (gate_mode IN ('chat','off')),
  headless    INTEGER NOT NULL DEFAULT 0,
  updated_ns  INTEGER NOT NULL
);
INSERT OR IGNORE INTO swarm_settings (swarm_id, gate_mode, headless, updated_ns)
  SELECT id, CASE WHEN gate = 0 THEN 'off' ELSE 'chat' END, 0, updated_ns FROM swarms;

CREATE TABLE IF NOT EXISTS swarm_dispatch (
  task_id       TEXT PRIMARY KEY REFERENCES agent_tasks (id) ON DELETE CASCADE,
  offered_ns    INTEGER,
  launched_by   TEXT CHECK (launched_by IN ('coordinator','dispatcher','operator')),
  requested_ns  INTEGER,
  updated_ns    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS swarm_decisions (
  id          INTEGER PRIMARY KEY,
  swarm_id    TEXT NOT NULL REFERENCES swarms (id) ON DELETE CASCADE,
  task_id     TEXT,
  verb        TEXT NOT NULL,
  by          TEXT NOT NULL CHECK (by IN ('chat','tui','cli')),
  said        TEXT,
  launch      TEXT,
  created_ns  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS swarm_decisions_swarm ON swarm_decisions (swarm_id, created_ns);

CREATE TABLE IF NOT EXISTS swarm_events (
  id          INTEGER PRIMARY KEY,
  swarm_id    TEXT NOT NULL REFERENCES swarms (id) ON DELETE CASCADE,
  task_id     TEXT,
  kind        TEXT NOT NULL,
  actor       TEXT,
  detail      TEXT,
  created_ns  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS swarm_events_swarm ON swarm_events (swarm_id, id);

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_dispatch_insert
AFTER INSERT ON swarm_dispatch
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_task', NEW.task_id, NULL, NULL, 'update');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_dispatch_update
AFTER UPDATE ON swarm_dispatch
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_task', NEW.task_id, NULL, NULL, 'update');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_decisions_insert
AFTER INSERT ON swarm_decisions
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_decision', NEW.swarm_id, NULL, NEW.launch, 'insert');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_events_insert
AFTER INSERT ON swarm_events
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_event', NEW.swarm_id, NULL, NEW.actor, 'insert');
END;
"#;

// v25: a review of each finished task (`bus::swarm::review`). The swarm's
// review setting (`on` with a harness, `None` meaning one other than the
// worker's, and the agent, `reviewer` by default; or `off`) and, per task,
// whether the plan dismissed its review and which board task reviews it.
// Side tables, as v24, so a replay is harmless; a swarm with no settings
// row (every swarm from before) has reviews off.
const V25: &str = r#"
CREATE TABLE IF NOT EXISTS swarm_review_settings (
  swarm_id    TEXT PRIMARY KEY REFERENCES swarms (id) ON DELETE CASCADE,
  mode        TEXT NOT NULL CHECK (mode IN ('on','off')),
  harness     TEXT,
  agent       TEXT,
  updated_ns  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS swarm_task_reviews (
  task_id      TEXT PRIMARY KEY REFERENCES agent_tasks (id) ON DELETE CASCADE,
  skip         INTEGER NOT NULL DEFAULT 0,
  review_task  TEXT,
  updated_ns   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS swarm_task_reviews_review ON swarm_task_reviews (review_task);

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_review_settings_insert
AFTER INSERT ON swarm_review_settings
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm', NEW.swarm_id, NULL, NULL, 'update');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_review_settings_update
AFTER UPDATE ON swarm_review_settings
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm', NEW.swarm_id, NULL, NULL, 'update');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_task_reviews_insert
AFTER INSERT ON swarm_task_reviews
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_task', NEW.task_id, NULL, NULL, 'update');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_task_reviews_update
AFTER UPDATE ON swarm_task_reviews
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_task', NEW.task_id, NULL, NULL, 'update');
END;
"#;

// v26: the swarm recovers on its own and gates its merges
// (`bus::swarm::recovery`, `bus::swarm::gate`). `swarm_task_recovery` is
// the engine's per-task bookkeeping for automatic retries: when a pass
// first saw the task's worker gone (`lost_ns`), how many automatic
// retries this round took and why the last one happened, and `held_ns`
// when the operator stopped the worker (no automatic retry then).
// `swarm_merge_gates` is a branch's merge queued behind the swarm's check
// command: `queued`, `running`, `passed` (merged), `failed`, the workspace
// commit it was tested on and the check's result. `swarm_merge_settings`
// turns the gate on per swarm (`on`: the swarm's check command; or a
// command of its own); no row means off. Side tables, as v24 and v25, so a
// replay is harmless.
const V26: &str = r#"
CREATE TABLE IF NOT EXISTS swarm_task_recovery (
  task_id     TEXT PRIMARY KEY REFERENCES agent_tasks (id) ON DELETE CASCADE,
  lost_ns     INTEGER,
  retries     INTEGER NOT NULL DEFAULT 0,
  retried_ns  INTEGER,
  reason      TEXT,
  held_ns     INTEGER,
  updated_ns  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS swarm_merge_gates (
  task_id     TEXT PRIMARY KEY REFERENCES agent_tasks (id) ON DELETE CASCADE,
  state       TEXT NOT NULL CHECK (state IN ('queued','running','passed','failed')),
  queued_ns   INTEGER NOT NULL,
  started_ns  INTEGER,
  ended_ns    INTEGER,
  base        TEXT,
  detail      TEXT,
  result      TEXT,
  updated_ns  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS swarm_merge_settings (
  swarm_id    TEXT PRIMARY KEY REFERENCES swarms (id) ON DELETE CASCADE,
  mode        TEXT NOT NULL CHECK (mode IN ('on','off')),
  command     TEXT,
  updated_ns  INTEGER NOT NULL
);

CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_task_recovery_insert
AFTER INSERT ON swarm_task_recovery
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_task', NEW.task_id, NULL, NULL, 'update');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_task_recovery_update
AFTER UPDATE ON swarm_task_recovery
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_task', NEW.task_id, NULL, NULL, 'update');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_merge_gates_insert
AFTER INSERT ON swarm_merge_gates
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_task', NEW.task_id, NULL, NULL, 'update');
END;
CREATE TRIGGER IF NOT EXISTS trg_trace_changes_swarm_merge_gates_update
AFTER UPDATE ON swarm_merge_gates
BEGIN
  INSERT INTO trace_changes(entity_kind, entity_id, session_key, launch_id, operation)
  VALUES ('swarm_task', NEW.task_id, NULL, NULL, 'update');
END;
"#;
