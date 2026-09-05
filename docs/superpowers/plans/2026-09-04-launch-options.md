# Per-harness launch options: implementation plan

Spec: `../specs/2026-09-04-launch-options-design.md`. Each task ends green (`cargo clippy --all-targets`, `cargo test --no-fail-fast`).

### Task 1: Mapping (`src/harness.rs`, `src/lib.rs`)

**Interfaces:** `Harness { Claude, Codex, Antigravity }` with `detect(command) -> Option<Harness>` and `as_str()`; `Resume { Off, Last, Id(String) }`; `LaunchOptions { model: Option<String>, bypass_approvals: bool, resume: Resume, one_shot: Option<String> }` with `is_empty()` and `render(harness) -> Rendered { leading, trailing }`; `compose(profile_args, rendered) -> Vec<String>`.

- [x] **Step 1:** Tests: every option per harness; unset renders nothing; Codex subcommands lead; the prompt is positional and last; one-shot composes with resume (`exec resume --last <prompt>`); detection by basename and absolute path; a non-harness command yields nothing.
- [x] **Step 2:** Implement.

### Task 2: Profile defaults (`src/config.rs`, `profiles.example.toml`)

- [x] **Step 1:** Tests: `model` and `bypass_approvals` parse, are optional, and default to unset.
- [x] **Step 2:** Implement; document both keys.

### Task 3: Dialog (`src/app.rs`, `src/ui.rs`)

**Interfaces:** `DialogField` gains `Model`, `Approvals`, `Resume`, `OneShot`; `DialogState` gains the matching values plus `harness: Option<Harness>` and `fields()`/`launch_options()`; submit renders the options into `profile.args`. No separate on/off for the one-shot: a blank prompt is off, so `Space` stays typable.

- [x] **Step 1:** Tests: tab order covers the new fields and skips them for a non-harness profile; typing edits model and prompt; toggles cycle; profile defaults pre-fill and per-launch edits win; empty prompt with One-shot on is refused.
- [x] **Step 2:** Implement; render the fields.

### Task 4: Spawn and the resume paths (`src/app.rs`)

- [x] **Step 1:** Tests: composed argv order; a traced `--continue` launch still skips id injection; the history viewer and trace browser resume through the mapping, including Codex.
- [x] **Step 2:** Implement; spec status. Both resume paths now share `harness::resume_args`, which is what gives Codex resume (`resume <id>`) where the trace browser previously refused it.
