#!/usr/bin/env bash
# Opt-in, live three-provider trace review. It never runs in CI.
set -euo pipefail

if [[ "${AGENT_MUX_LIVE:-}" != "1" ]]; then
  echo "Refusing live provider run. Re-run with AGENT_MUX_LIVE=1." >&2
  exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin="${AGENT_MUX_BIN:-$root/target/debug/agent-mux}"
[[ -x "$bin" ]] || cargo build --manifest-path "$root/Cargo.toml"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/agent-mux-live.XXXXXX")"
keep="${KEEP_AGENT_MUX_LIVE_ARTIFACTS:-0}"
cleanup() {
  status=$?
  if [[ "$keep" == "1" || $status -ne 0 ]]; then
    echo "Live trace artifacts: $tmp" >&2
  else
    rm -rf "$tmp"
  fi
  exit "$status"
}
trap cleanup EXIT

project="$tmp/project"
home="$tmp/home"
db="$tmp/traces.db"
claude_dir="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
agy_dir="${AGENT_MUX_LIVE_AGY_DIR:-$HOME/.gemini/antigravity-cli}"
mkdir -p "$project" "$home/.agent-mux" "$home/.codex/skills" "$project/.claude/skills" "$project/.claude/agents" "$project/.agents/skills" "$project/.agents/workflows"
git -C "$project" init -q
printf 'trace matrix fixture\n' >"$project/fixture.txt"

write_skill() {
  path=$1
  name=$2
  mkdir -p "$(dirname "$path")"
  printf '%s\n' '---' "name: $name" 'description: Use when asked to "exercise alpha" or "exercise beta" in the trace matrix.' '---' 'Read fixture.txt, then write your name to the corresponding result file.' >"$path"
}
for base in "$project/.claude/skills" "$home/.codex/skills" "$project/.agents/skills"; do
  write_skill "$base/alpha/SKILL.md" alpha
  write_skill "$base/beta/SKILL.md" beta
done
printf '%s\n' '---' 'name: verifier' 'description: Verify fixture files for the trace matrix.' 'tools: Read, Write' '---' 'Inspect fixture.txt and report its contents.' >"$project/.claude/agents/verifier.md"
printf '%s\n' '---' 'name: verifier' 'description: Verify fixture files for the trace matrix.' '---' 'Inspect fixture.txt and report its contents.' >"$project/.agents/workflows/verifier.md"
printf '%s\n' 'Use the alpha and beta skills when explicitly named. Delegate verification to a subagent.' >"$project/AGENTS.md"

# Link the existing Codex credential; it is never copied, logged, or modified.
real_codex_home="${CODEX_HOME:-${HOME}/.codex}"
if [[ -f "$real_codex_home/auth.json" ]]; then
  ln -s "$real_codex_home/auth.json" "$home/.codex/auth.json"
fi

cat >"$project/profiles.toml" <<EOF
[tracing]
db_path = "$db"
content_mode = "full"
poll_interval_ms = 100
flush_interval_ms = 50
claude_dir = "$claude_dir"
codex_dir = "$home/.codex"
antigravity_dir = "$agy_dir"
hooks = "auto"

[[profiles]]
name = "Claude matrix"
command = "claude"
args = ["--permission-mode", "acceptEdits", "--allowedTools", "Read,Edit,Write,Agent"]

[[profiles]]
name = "Codex matrix"
command = "codex"
args = ["--sandbox", "workspace-write"]

[[profiles]]
name = "Agy matrix"
command = "agy"
args = ["--sandbox", "--mode", "accept-edits"]
EOF

prompt='Inside this fixture project only: exercise alpha and beta, then delegate verification of fixture.txt to one subagent. Do not use the network, do not access files outside this project, and report completion.'
if ! claude auth status 2>/dev/null | grep -q '"loggedIn": true'; then
  echo "Claude Code is not authenticated for $claude_dir. Run 'claude auth login' in this shell before the live matrix." >&2
  exit 1
fi
run_one() {
  harness=$1
  (
    cd "$project"
    case "$harness" in
      claude)
        "$bin" run --experiment live-trace-matrix --variant "$harness" --harness "$harness" --cwd "$project" --prompt "$prompt" --check 'test -f fixture.txt' --timeout 180
        ;;
      agy)
        "$bin" run --experiment live-trace-matrix --variant "$harness" --harness "$harness" --cwd "$project" --prompt "$prompt" --check 'test -f fixture.txt' --timeout 180
        ;;
      *)
        HOME="$home" CODEX_HOME="$home/.codex" "$bin" run --experiment live-trace-matrix --variant "$harness" --harness "$harness" --cwd "$project" --prompt "$prompt" --check 'test -f fixture.txt' --timeout 180
        ;;
    esac
  )
}
verify() {
  (
    cd "$project"
    HOME="$home" CODEX_HOME="$home/.codex" "$bin" trace loops
    HOME="$home" CODEX_HOME="$home/.codex" "$bin" trace skills
    HOME="$home" CODEX_HOME="$home/.codex" "$bin" trace agents
  )
  sqlite3 "$db" "SELECT provider, COUNT(*) AS turns FROM traces GROUP BY provider;"
  sqlite3 "$db" "SELECT skill, turns_loaded, turns_unused FROM skill_stats ORDER BY skill;"
  sqlite3 "$db" "SELECT agent_type, invocations FROM agent_stats ORDER BY agent_type;"
}
require_matrix() {
  harness=$1
  provider=$harness
  [[ "$provider" == "agy" ]] && provider="antigravity"
  turns="$(sqlite3 "$db" "SELECT COUNT(*) FROM traces t JOIN sessions s ON s.key = t.session_key WHERE s.provider = '$provider';")"
  skills="$(sqlite3 "$db" "SELECT COUNT(DISTINCT j.value) FROM traces t JOIN sessions s ON s.key = t.session_key, json_each(t.skills) j WHERE s.provider = '$provider' AND j.value IN ('alpha', 'beta');")"
  agents="$(sqlite3 "$db" "SELECT COUNT(*) FROM observations o JOIN traces t ON t.id = o.trace_id JOIN sessions s ON s.key = t.session_key WHERE s.provider = '$provider' AND o.type = 'agent';")"
  if [[ "$turns" -lt 1 || "$skills" -lt 2 || "$agents" -lt 1 ]]; then
    echo "$harness trace is incomplete: turns=$turns skills=$skills agents=$agents" >&2
    return 1
  fi
}

for harness in claude codex agy; do
  for attempt in 1 2; do
    echo "== $harness (attempt $attempt) =="
    if run_one "$harness" && require_matrix "$harness"; then
      break
    fi
    [[ "$attempt" == 2 ]] && { echo "$harness failed after two attempts" >&2; exit 1; }
  done
done
verify
echo "Live provider matrix completed. Database: $db"
