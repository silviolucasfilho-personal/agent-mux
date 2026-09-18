# Shapes and when to use them

| Task shape | Document |
| --- | --- |
| Classify-and-act: decide what kind of task this is, then handle it accordingly | `route` with a classifier and `branches` |
| Fan-out-and-synthesize: split into parts, work each, merge | `fanout` then `single` with `input` |
| Adversarial verification: every result checked by independent skeptics | `verify = { votes = 3, keep = "refuted < 2" }` on the step |
| Generate-and-filter: many candidates, keep the best that pass | `fanout` + `dedupe_by` + `keep` + `verify` + `take` |
| Tournament: attempts from different angles, judged pairwise | `tournament` with `judge` |
| Loop until done: unknown amount of work | `until` with `dedupe_by` and `rounds_without_new` |

Quality patterns: give each verifier a different lens when a result can fail in several ways; add a completeness critic (`wf-critic`) as the last step of research; log nothing silently: `take`, `keep` and `dedupe_by` are reported by agent-mux, so prefer them over telling a skill to "keep the best ones".

Sizing: one `single` step for a quick look; `fanout` over 3 to 6 items for a review; `until` with 3-vote `verify` for an audit; `tournament` with `n = 3` or `4` for a design. Regular coding tasks do not need a panel of 5 reviewers.
