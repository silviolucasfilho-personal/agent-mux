#!/usr/bin/env bash
# Verifies agent-mux MCP server and harness compatibility against a temporary fixture DB.
set -euo pipefail

usage() {
  cat <<EOF
Usage: scripts/verify-agent-mcp.sh [options]

Options:
  --harness <claude|codex|agy|all>  Harness to verify (default: all)
  --db <PATH>                       Explicit fixture DB path (MUST be temporary)
  --keep                            Preserve temporary files upon exit
  -h, --help                        Show this help message
EOF
  exit 1
}

target_harness="all"
custom_db=""
keep=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --harness)
      target_harness="$2"
      shift 2
      ;;
    --db)
      custom_db="$2"
      shift 2
      ;;
    --keep)
      keep=1
      shift
      ;;
    -h|--help)
      usage
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      ;;
  esac
done

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="${AGENT_MUX_BIN:-$root/target/debug/agent-mux}"

if [[ ! -x "$bin" ]]; then
  echo "Building agent-mux binary..."
  cargo build --manifest-path "$root/Cargo.toml"
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/agent-mcp-verify.XXXXXX")"
tmp="$(cd "$tmp" && pwd -P)"
cleanup() {
  status=$?
  if [[ "$keep" == "1" || $status -ne 0 ]]; then
    echo "Temporary test artifacts kept at: $tmp" >&2
  else
    rm -rf "$tmp"
  fi
  exit "$status"
}
trap cleanup EXIT

# 1. Safety Check: Refuse to run against production database
real_db="${AGENT_MUX_TRACE_DB:-$HOME/.agent-mux/traces.db}"
if [[ -n "$custom_db" ]]; then
  if [[ "$custom_db" == "$real_db" || "$custom_db" == "$HOME/.agent-mux"* ]]; then
    echo "ERROR: Refusing to run against production trace store ($custom_db)." >&2
    echo "Verification requires an isolated fixture database." >&2
    exit 1
  fi
  db="$custom_db"
else
  db="$tmp/traces.db"
fi

echo "==> Setting up fixture database at: $db"
sqlite3 "$db" <<'SQL'
CREATE TABLE traces (
    id TEXT PRIMARY KEY,
    session_key TEXT,
    launch_id TEXT,
    provider TEXT,
    cwd TEXT,
    start_ns INTEGER,
    end_ns INTEGER,
    input TEXT,
    output TEXT,
    total_tokens INTEGER,
    total_cost_usd REAL
);
CREATE TABLE sessions (
    key TEXT PRIMARY KEY,
    provider TEXT,
    cwd TEXT,
    first_seen_ns INTEGER,
    last_seen_ns INTEGER
);
CREATE TABLE observations (
    id TEXT PRIMARY KEY,
    trace_id TEXT,
    name TEXT,
    type TEXT,
    start_ns INTEGER,
    end_ns INTEGER,
    is_error INTEGER,
    status_message TEXT,
    input TEXT,
    output TEXT,
    total_tokens INTEGER,
    total_cost_usd REAL,
    skill TEXT
);
CREATE VIEW session_stats AS
SELECT
    session_key,
    COUNT(id) AS turn_count,
    SUM(total_tokens) AS total_tokens,
    SUM(total_cost_usd) AS total_cost_usd,
    (SELECT COUNT(o.id) FROM observations o JOIN traces t ON t.id = o.trace_id WHERE t.session_key = traces.session_key AND o.type = 'tool') AS total_tools
FROM traces
GROUP BY session_key;
CREATE VIEW skill_stats AS
SELECT
    skill,
    COUNT(DISTINCT trace_id) AS turns_loaded,
    COUNT(id) AS tools,
    SUM(total_tokens) AS tokens,
    SUM(total_cost_usd) AS cost,
    0 AS turns_unused
FROM observations
WHERE skill IS NOT NULL AND skill != ''
GROUP BY skill;
SQL

now_ns="$(date +%s)000000000"
now_minus_hour="$(( $(date +%s) - 3600 ))000000000"

sqlite3 "$db" "INSERT INTO sessions VALUES('sess_verify','claude','$tmp',$now_minus_hour,$now_ns);"
sqlite3 "$db" "INSERT INTO traces VALUES('trace_verify','sess_verify','launch_verify','claude','$tmp',$now_minus_hour,$now_ns,'Review the changes','Changes reviewed',500,0.015);"
sqlite3 "$db" "INSERT INTO observations VALUES('obs_verify','trace_verify','replace_file_content','tool',$now_minus_hour,$now_ns,0,'ok','{\"TargetFile\":\"src/main.rs\"}','Updated',100,0.003,'dev-tools');"

echo "==> Verifying CLI trace briefing output..."
cli_output="$("$bin" trace briefing --db "$db" --workspace "$tmp" --json)"
echo "$cli_output" | grep -q '"schema_version": 1'
echo "$cli_output" | grep -q '"total_sessions": 1'
echo "  [OK] CLI briefing returned valid schema envelope."

echo "==> Verifying MCP Stdio Protocol parity..."
mcp_init_req='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test-client","version":"1.0"}}}'
mcp_call_req='{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agent_mux_get_briefing","arguments":{}}}'

mcp_responses=$(printf '%s\n%s\n' "$mcp_init_req" "$mcp_call_req" | "$bin" mcp serve --stdio --db "$db" --workspace "$tmp" 2>/dev/null)
echo "$mcp_responses" | grep -q '"serverInfo"'
echo "$mcp_responses" | grep -q '"structuredContent"'
echo "  [OK] MCP stdio server responded to initialize and tool execution."

verify_harness() {
  local h="$1"
  echo "==> Inspecting harness integration: $h"
  case "$h" in
    claude)
      if command -v claude >/dev/null 2>&1; then
        claude_ver="$(claude --version 2>/dev/null || echo "unknown")"
        echo "  [OK] Claude Code detected: $claude_ver ($(command -v claude))"
      else
        echo "  [SKIP] Claude Code (claude) not found on PATH. Remains unverified."
      fi
      ;;
    codex)
      if command -v codex >/dev/null 2>&1; then
        codex_ver="$(codex --version 2>/dev/null || echo "unknown")"
        echo "  [OK] Codex CLI detected: $codex_ver ($(command -v codex))"
      else
        echo "  [SKIP] Codex CLI (codex) not found on PATH. Remains unverified."
      fi
      ;;
    agy)
      if command -v agy >/dev/null 2>&1; then
        agy_ver="$(agy --version 2>/dev/null || echo "unknown")"
        echo "  [OK] Antigravity CLI detected: $agy_ver ($(command -v agy))"
      else
        echo "  [SKIP] Antigravity CLI (agy) not found on PATH. Remains unverified."
      fi
      ;;
  esac
}

if [[ "$target_harness" == "all" ]]; then
  verify_harness claude
  verify_harness codex
  verify_harness agy
else
  verify_harness "$target_harness"
fi

echo "==> All verification checks completed successfully."
