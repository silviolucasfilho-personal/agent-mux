# Loop Budget — fixture-full

## Daily limits

| Loop | Max runs/day | Max tokens/day | Max sub-agent spawns/run |
|------|--------------|----------------|--------------------------|
| Daily Triage | 2 | 100k | 0 (L1) / 2 (L2) |

## On budget exceed

Pause the loop, append the run log, notify a human.

## Kill switch

- Command or issue label: `loop-pause-all`
