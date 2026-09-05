# Per-harness launch options

**Status:** Implemented (plan: `../plans/2026-09-04-launch-options.md`, all tasks landed).
**Request (2026-09-04):** add launch options to the new-session dialog, adapted per harness — non-interactive, model, bypass approvals, resume — with "if the model is not set just not pass the param".

## Purpose

A profile today is a fixed command line. Choosing a model, skipping approval prompts, resuming, or firing a one-shot prompt means editing `profiles.toml` or typing the flags by hand — and the flags differ per CLI, in shape as well as name. This puts the four options in the launch dialog and keeps one place that knows how each harness spells them.

## Decisions

1. **Unset means absent.** Every option renders nothing unless it is set: no `--model` when the field is blank (each CLI picks its own default), no approval flag on `normal`, no resume flag on `off`. Nothing is ever passed "empty".
2. **One mapping, three callers.** `src/harness.rs` owns the command→harness detection and the rendering of options into argv. The launch dialog, the history viewer, and the trace browser all use it instead of hardcoding flags, which is also how Codex resume gets fixed — `app.rs` currently refuses it.
3. **Subcommands are structural, not flags.** Codex spells one-shot and resume as subcommands that must lead the command line (`codex exec …`, `codex resume …`), so rendering returns a *leading* part and a *trailing* part; final argv is `leading + profile.args + trailing`, with a positional prompt last. Claude and agy spell both as ordinary flags.
4. **Verified against the installed CLIs, not from memory.** `claude --help`, `codex --help`/`codex resume --help`/`codex exec --help`, and `agy --help` on this machine (codex-cli 0.153.2) settle the table below; three entries differ from the request's table and the verified spelling wins.
5. **Model and approvals can be profile defaults; resume and one-shot cannot.** `[[profiles]] model` and `bypass_approvals` are launch-shaped settings worth persisting, and the dialog pre-fills from them and overrides per launch. Resuming and a one-shot prompt are decisions about *this* launch, so they live only in the dialog.
6. **Tracing keeps working.** Rendered options land in the profile's args before planning, so the trace planner sees `-p` / `--resume` / `--continue` and already does the right thing: it skips `--session-id` injection for those forms, extracts an explicit id when one is given, and the hook channel announces the session for the rest.

## The mapping

| Concept | Claude | Codex | Antigravity |
|---|---|---|---|
| One-shot prompt | `-p <prompt>` | `exec <prompt>` *(leading)* | `-p <prompt>` |
| Model | `--model <id>` | `--model <id>` | `--model <id>` |
| Bypass approvals | `--dangerously-skip-permissions` | `--yolo` | `--dangerously-skip-permissions` |
| Resume last | `--continue` | `resume --last` *(leading)* | `--continue` |
| Resume by id | `--resume <id>` | `resume <id>` *(leading)* | `--conversation <id>` |

Corrections to the requested table, each checked against `--help`:

- agy's `-c` is an alias for **`--continue`**, not `--conversation`; resuming a specific agy conversation is `--conversation <id>`.
- "Resume" splits in two. Bare `claude --resume` opens an interactive picker rather than resuming the last session, so *resume last* is `--continue` on Claude and agy and `resume --last` on Codex, while *resume by id* keeps `--resume <id>` / `--conversation <id>` / `resume <id>`.
- Codex accepts `--yolo`, though its help lists only `--dangerously-bypass-approvals-and-sandbox`; the alias is used because it is what was asked for and it works, with the canonical name noted in the code.

One-shot and resume compose where the CLI allows it: Claude and agy take `-p` alongside `--continue`, and Codex spells the same thing `exec resume --last <prompt>`.

## Dialog

Four fields after Content Mode, reached with `Tab`, each naming the flag the selected CLI actually uses:

```
── codex options ──
Model:            gpt-5.6 (--model, blank = unset)
Approvals:        [!] bypass (--yolo)
Resume:           [Last session] (resume --last)
One-shot prompt:  fix the failing test (codex exec)
```

`Model` and `One-shot prompt` are text inputs like `Directory`; `Approvals` and `Resume` toggle with `Space`. There is no separate on/off for the one-shot: an empty prompt *is* off, which keeps the "unset means absent" rule uniform and leaves `Space` free to type a space inside the prompt. A profile whose command is not a known harness hides all four — there is nothing to render them into — and `Tab` skips them. The subfolder list gives up rows first on a short terminal so the fields below it stay on screen.

## Config

```toml
[[profiles]]
name = "Claude Code"
command = "claude"
model = "claude-opus-5"      # optional; omitted from the command line when unset
bypass_approvals = false     # --dangerously-skip-permissions / --yolo
```

## Testing

- **Rendering**: each option per harness, unset options render nothing, subcommands lead, the prompt is positional and last, one-shot + resume compose, and an unknown command renders nothing.
- **Detection**: `claude`, `codex`, `agy` by basename and via an absolute path; anything else is not a harness.
- **Dialog**: new fields cycle and type, profile defaults pre-fill and per-launch edits win, a blank field renders nothing, fields hide (and `Tab` skips them) for a non-harness profile.
- **Spawn**: the composed argv is `leading + profile.args + trailing`; a traced Claude launch with `--continue` still skips id injection.
- **Resume paths**: the history viewer and trace browser render through the same mapping, and Codex resume works where it previously refused.

## Out of scope

`--ask-for-approval <policy>` levels beyond on/off, agy's `--prompt-interactive` (a one-shot that stays interactive — a natural follow-up), output formats for print mode, and persisting resume/one-shot as profile defaults.

## Files

| File | Change |
|---|---|
| `src/harness.rs` (new) | `Harness`, `LaunchOptions`, `Resume`, rendering, tests. |
| `src/lib.rs` | `pub mod harness;` |
| `src/config.rs` | `Profile.model`, `Profile.bypass_approvals`. |
| `src/app.rs` | dialog fields, keys, submit, and the two resume paths. |
| `src/ui.rs` | render the four fields. |
| `profiles.example.toml` | document the two new profile keys. |
