# Loop Constraints

- Never push to a shared branch; never merge. A human merges.
- One fix per run; at most three attempts, then stop and ask (exit code 2).
- Never disable a test.
- Least-privilege tool scope: read tools first, write only the state file at L1.
- A circuit breaker stops the loop after the same error three times.
