# Loop Run Log — {{PROJECT}}

One JSON line per completed run, appended by agent-mux below the marker. Entries older than 30 days are pruned. Runs that were blocked before they started are kept in agent-mux's store, not here.

## Format

```json
{
  "run_id": "2026-09-16T08:00:00Z",
  "pattern": "daily-triage",
  "duration_s": 41,
  "items_found": 9,
  "actions_taken": 1,
  "escalations": 2,
  "tokens_estimate": 48210,
  "outcome": "report-only | fix-proposed | escalated | no-op | failed",
  "readiness_score": 82,
  "level": "L1",
  "harness": "claude",
  "launch_id": "…",
  "source": "agent-mux"
}
```

`tokens_estimate` is what the trace store measured for the run, never a guess.

## Recent Runs

<!-- Loop appends below this line -->
