# Loop Budget — agent-mux

Caps are enforced by agent-mux before a run starts; the numbers here are the same ones in its registry.

## Daily limits

| Loop | Max runs/day | Max tokens/day | Max sub-agent spawns/run |
| --- | --- | --- | --- |
| PR Babysitter | 288 | 2.0M | 0 |

## On budget exceed

- At 80 % of the daily token cap the next run is report-only: it writes the state file and stops.
- At 100 % the next run does not start; it is recorded as blocked and the loop waits for the next UTC day.
- Three failed runs in a row, or the same error three times, pause the loop until a human resumes it.

## Kill switch

- Write the literal `loop-pause-all` into the state file or `LOOP.md`, or press `K` in agent-mux.
- Resume only after a human removes the literal and resumes the loop in agent-mux.
