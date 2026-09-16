# Loop Run Log — agent-mux

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

{"run_id":"2026-09-16T09:38:54Z","pattern":"pr-babysitter","duration_s":54,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":118320,"outcome":"report-only","harness":"claude","launch_id":"ac953677-1309-4cf7-b173-0e2210c6a1ab","level":"L1","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T09:41:33Z","pattern":"pr-babysitter","duration_s":3,"items_found":0,"actions_taken":0,"escalations":0,"tokens_estimate":0,"outcome":"no-op","harness":"claude","launch_id":"1f00f049-e745-4460-ba7e-eb7c70657774","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T09:44:19Z","pattern":"pr-babysitter","duration_s":3,"items_found":0,"actions_taken":0,"escalations":0,"tokens_estimate":0,"outcome":"no-op","harness":"claude","launch_id":"14ce12e2-8894-42c4-9c6c-2e23af819d73","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T09:50:04Z","pattern":"pr-babysitter","duration_s":3,"items_found":0,"actions_taken":0,"escalations":0,"tokens_estimate":0,"outcome":"no-op","harness":"claude","launch_id":"7518b60c-4f2c-4ca9-aea6-de52d9ed7003","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T10:17:23Z","pattern":"pr-babysitter","duration_s":3,"items_found":0,"actions_taken":0,"escalations":0,"tokens_estimate":0,"outcome":"no-op","harness":"claude","launch_id":"9142c557-0c3a-4967-b65a-e629805a9aa5","level":"L2","readiness_score":100,"source":"agent-mux"}
