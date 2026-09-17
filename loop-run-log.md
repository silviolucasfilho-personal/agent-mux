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
{"run_id":"2026-09-16T10:32:23Z","pattern":"pr-babysitter","duration_s":42,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":119118,"outcome":"report-only","harness":"claude","launch_id":"69e3eb15-331c-4906-8601-10ea4c273148","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T10:47:23Z","pattern":"pr-babysitter","duration_s":31,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":119035,"outcome":"report-only","harness":"claude","launch_id":"287d2a1c-8ed9-4fa3-b434-8cb038c60298","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T11:02:23Z","pattern":"pr-babysitter","duration_s":32,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":121001,"outcome":"report-only","harness":"claude","launch_id":"ab5a787e-1c4c-4402-ab75-07eefe7c17b5","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T11:17:23Z","pattern":"pr-babysitter","duration_s":56,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":288063,"outcome":"report-only","harness":"claude","launch_id":"9c5fc137-e42a-4b90-bbde-046621387552","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T11:32:23Z","pattern":"pr-babysitter","duration_s":56,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":233617,"outcome":"report-only","harness":"claude","launch_id":"095631f4-00a9-40de-8429-62131013449a","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T11:47:24Z","pattern":"pr-babysitter","duration_s":53,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":248380,"outcome":"report-only","harness":"claude","launch_id":"ecbc0ab0-4a3f-4aa0-9009-8180aaa0ca0e","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T12:02:24Z","pattern":"pr-babysitter","duration_s":36,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":120280,"outcome":"report-only","harness":"claude","launch_id":"2fd58e69-15df-4761-9f4d-98108deb3833","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T12:17:24Z","pattern":"pr-babysitter","duration_s":35,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":120929,"outcome":"report-only","harness":"claude","launch_id":"64d8f5fa-631f-4f26-b29a-d6664a07982e","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T12:32:24Z","pattern":"pr-babysitter","duration_s":73,"items_found":0,"actions_taken":0,"escalations":0,"tokens_estimate":333164,"outcome":"no-op","harness":"claude","launch_id":"ae4f7df5-5f73-45c6-b94d-02b9608e10b8","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T12:44:53Z","pattern":"pr-babysitter","duration_s":31,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":117855,"outcome":"report-only","harness":"claude","launch_id":"9d75f551-33a1-48ac-ac2f-77cd20a6e91b","level":"L1","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T12:47:14Z","pattern":"pr-babysitter","duration_s":25,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":93447,"outcome":"report-only","harness":"claude","launch_id":"2a8093e6-2031-4e3e-8d65-bdf676b8b3b0","level":"L1","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T14:15:55Z","pattern":"pr-babysitter","duration_s":32,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":91259,"outcome":"report-only","harness":"claude","launch_id":"3b2935c0-9d05-47ab-8b85-ecfc3d1670b3","level":"L1","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T15:27:11Z","pattern":"pr-babysitter","duration_s":25,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":92050,"outcome":"report-only","harness":"claude","launch_id":"6357442b-8203-451e-bf93-694ca3dfaa2f","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T15:48:21Z","pattern":"pr-babysitter","duration_s":22,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":114413,"outcome":"no-op","harness":"claude","launch_id":"30f16315-8016-419b-ba85-396ee0b63949","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T16:03:21Z","pattern":"pr-babysitter","duration_s":20,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":90664,"outcome":"no-op","harness":"claude","launch_id":"87a312fd-e500-4e12-81b4-7d2d0fbbe238","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T16:18:21Z","pattern":"pr-babysitter","duration_s":20,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":91063,"outcome":"no-op","harness":"claude","launch_id":"3bc82081-aae8-4948-b91a-88181fc6591a","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T16:33:21Z","pattern":"pr-babysitter","duration_s":18,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":95789,"outcome":"no-op","harness":"claude","launch_id":"870b6b0e-28f5-4d40-809d-ab4046458e8a","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T16:48:21Z","pattern":"pr-babysitter","duration_s":19,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":95984,"outcome":"no-op","harness":"claude","launch_id":"9deb7430-c08d-45ee-903e-50ede82c3890","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T17:03:21Z","pattern":"pr-babysitter","duration_s":19,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":96053,"outcome":"no-op","harness":"claude","launch_id":"97bb52fc-d7c1-4661-afbb-153dfb4d2e24","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T17:18:21Z","pattern":"pr-babysitter","duration_s":20,"items_found":0,"actions_taken":0,"escalations":0,"tokens_estimate":98344,"outcome":"report-only","harness":"claude","launch_id":"6bbd82f0-4551-4550-98d6-8b09f5e64065","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-16T17:33:21Z","pattern":"pr-babysitter","duration_s":31,"items_found":1,"actions_taken":0,"escalations":0,"tokens_estimate":106399,"outcome":"report-only","harness":"claude","launch_id":"a715f693-f8ff-436f-ac29-72e99354ae9e","level":"L2","readiness_score":100,"source":"agent-mux"}
{"run_id":"2026-09-17T17:35:17Z","pattern":"pr-babysitter","duration_s":36,"items_found":0,"actions_taken":0,"escalations":0,"tokens_estimate":137343,"outcome":"report-only","harness":"claude","launch_id":"d4dcdde3-13bf-46b9-a505-142fd422e641","level":"L2","readiness_score":100,"source":"agent-mux"}
