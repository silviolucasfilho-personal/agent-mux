# Skills view: a dedicated modal for browsing, installing and launching skills

Status: Implemented on 2026-09-15 (see the plan of the same date). Open questions resolved: native skills are listed as read-only rows; the row's harness is the launch harness and the separate picker was removed; uninstall is per row.
Date: 2026-09-15
Baseline: `f5e5166` (onboarding README, analysis `is_error` fix, skill launch store path).

## 1. Problem and outcome

Today the Skills sidebar section hijacks the main pane: focusing a skill replaces the selected session's terminal with the package preview, or with the Heimdall telemetry briefing. Skill telemetry is split across three places: the sidebar preview, the `K` inventory pane inside the Trace Browser, and the `trace skills` command. None of them shows where a skill is installed per harness or when it was last run.

Outcome: the main pane always shows the selected session. Skills get their own modal, the **Skills view**, opened with `S` from Control mode, built like the Trace Browser (`T`). It lists every skill grouped by harness, shows where each is installed and whether the installation is current, shows when and how it was executed (launched sessions and turns that loaded it), and lets the user install, uninstall, launch or attach without leaving the view. The Skills sidebar section and the Trace Browser `K` pane are removed; the Heimdall briefing moves into the view as a tab.

Out of scope: editing skill packages, harness-native skill authoring, network installs, and any change to the `agent-mux skill` or `trace skills` command output. The CLI keeps working; the view reuses its data functions.

## 2. Existing implementation to reuse

| Concern | Reuse from | Notes |
| --- | --- | --- |
| Package discovery and parsing | `skill::load_skills` (`src/skill/mod.rs`) | Compiled-in Heimdall plus `~/.agent-mux/skills` packages. |
| Install state per harness | `skill::install::status`, `install`, `uninstall` (`src/skill/install.rs`) | `InstallStatus { harness, dir, installed, managed, current }`. |
| Harness-native definitions | `inventory::inventory_all(cwd, home)` (`src/tracing/inventory.rs`) filtered to `Kind::Skill` | Scope `project`, `home`, `plugin:<name>`; path; triggers. |
| Usage statistics | `query::skill_stats`, `query::traces_with_skill`, `query::prompt_rows`, `inventory::skill_reports` (`src/tracing/store/query.rs`) | `turns_loaded`, `turns_unused`, `missed`, cost, `first_ns`, `last_ns`. |
| Launch and singleton behaviour | `App::launch_skill`, `build_skill_launch_with_db` | The row's harness is the launch harness; the old picker (`SkillLauncherState`) is removed. |
| Briefing | `App::refresh_briefing_if_needed`, `draw_trace_briefing_preview` | The refresh gate changes from "Skills section focused" to "Skills view open on the Briefing tab". |
| Modal plumbing | `TraceBrowserState`, `draw_trace_browser`, `handle_browser_key`, mouse routing in `App::handle_mouse` | Same read-only connection pattern (`store::open_ro`), same 500 ms live refresh throttle, same `Esc` unwind style. |

Confirmed gaps in the baseline that this spec closes:

- The store has no durable link between a launch and the skill that started it. `App::launch_skill` sets `Session.skill_id` in memory only; Heimdall's `reference/agents.md` falls back to `launches.profile LIKE '<name> (%'`, which breaks for a renamed package or a user profile with the same name.
- `skill::install::status` reports agent-mux packages only; a skill that exists in a harness directory without a manifest is "present, not managed" with no further detail, and harness-native skills that are not agent-mux packages are visible only through `trace skills`.
- Nothing shows execution history for a skill: neither which sessions ran it nor when it last loaded.

## 3. Design decision and alternatives

Chosen: a three-pane modal with the same shape as the Trace Browser, fed by the existing skill, install, inventory and store functions, plus one new store column of evidence (`launches.metadata.skill_id`).

| Approach | Benefit | Limitation |
| --- | --- | --- |
| Keep the sidebar section, make the preview a split pane | Smallest change | Still steals terminal space; does not solve the per-harness install and execution questions |
| Dedicated modal (chosen) | Consistent with `T`, room for grouping and detail, no impact on the terminal pane | One more mode and key table to maintain |
| Fold skills into the Trace Browser as a fourth pane | One modal | The browser is about turns; install and launch actions do not belong there and the `K` pane already shows the mismatch |

## 4. User experience

### 4.1 Entry and layout

`S` in Control mode opens the view (any sidebar section, sidebar hidden or not). The status-bar hints for Control mode gain `[S] skills`. The help overlay gains a "Skills view" section.

```text
┌─ Skills (7) [harness: all] ────────┬─ heimdall · Claude Code ─────────────────────────────────┐
│ Claude Code                        │ Details | Executions | Briefing            (Tab to switch) │
│ > ⚡ heimdall        installed ✓   │                                                          │
│   ⚡ my-notes        stale         │ Package     Heimdall (built-in)  id heimdall             │
│   ⚙ commit-helper   native · home │ Harnesses   claude, codex, agy   default agy             │
│ Codex CLI                          │ Capabilities trace.read                                  │
│   ⚡ heimdall        not installed │ Description  Use when the user asks for a briefing …     │
│   ⚙ pr-review       native · plugin│                                                          │
│ Antigravity                        │ Installed    ~/.claude/skills/heimdall  (managed, current)│
│   ⚡ heimdall        running [agy] │ Files        SKILL.md, reference/sessions.md, …          │
│                                    │ Last run     2026-09-14 22:10 (agy, exited 0, $0.34)     │
│                                    │ Runs / turns 5 sessions · 41 turns loaded · 9 unused     │
└────────────────────────────────────┴──────────────────────────────────────────────────────────┘
 [Tab] tab  [↑/↓] select  [Enter] launch/attach  [i] install  [u] uninstall  [1-3] harness  [r] rescan  [Esc] close
```

- **Left pane, Skills.** Rows grouped under one header per harness in the fixed order Claude Code, Codex CLI, Antigravity. Under each header, every skill visible to that harness: agent-mux packages that declare the harness, and harness-native skill definitions found by the inventory (`⚙` icon, tagged `native · <scope>`). A package appears once per declared harness because installation and execution are per harness. Headers are not selectable; `↑/↓` and `j/k` skip them. The right-hand column shows the install state (`installed ✓`, `stale`, `not managed`, `not installed`), or `running [harness]` in green when a live session carries that skill id. `1`, `2`, `3` (and `c`, `x`, `a`) filter the list to one harness; pressing the same key again clears the filter; the title shows `[harness: all|claude|codex|agy]`.
- **Right pane, detail tabs.** `Tab`/`BackTab` (and `←/→`) cycle Details → Executions → Briefing. Briefing exists only for packages declaring `trace.read`; for other rows the cycle has two tabs.
  - **Details:** package name, id, origin (built-in, user directory path, or native definition path), harnesses and default, capabilities, description, startup prompt; then for the selected harness the install directory, managed/current state, the manifest hash prefix, and the file list; for native definitions the scope, path, declared tools and trigger phrases; then a one-line summary of the store statistics (`turns_loaded`, `turns_unused`, missed triggers, cost, first and last load).
  - **Executions:** two lists. **Sessions** launched with this skill on this harness, newest first: started time, harness, cwd, termination and exit code, turns, cost, and a `●` when the launch is still live. **Turns that loaded the skill** (any harness), newest first: started time, session key prefix, ordinal, latency, cost, and whether the skill was attributed to any observation in that turn. `Enter` on a session row attaches to it when it is live; `T` on either row opens the Trace Browser positioned on that session (and turn), replacing the Skills view.
  - **Briefing:** the current Heimdall telemetry briefing rendering, unchanged, refreshed at most once per second while this tab is visible.

### 4.2 Actions

| Key | Action |
| --- | --- |
| `Enter` (Skills pane) | Package row: if a live session carries the skill id, attach to it (warn when the row's harness differs, as today); otherwise install or refresh into the row's harness and launch there directly. The harness is already chosen by the row, so the picker is not shown. Native row: no launch; notice `native skills are launched by their harness`. |
| `i` | Install or refresh the package into the row's harness (`skill::install`, `force = false`). A foreign directory produces the existing error as a notice; `I` forces. |
| `u` | Uninstall from the row's harness after a `y/n` confirmation using the existing `draw_confirm` overlay. Refuses a running skill. |
| `r` | Rescan packages, inventory and install state; re-query the store. |
| `1`/`2`/`3`, `c`/`x`/`a` | Toggle the harness filter. |
| `Tab`, `BackTab`, `←`, `→` | Cycle detail tabs. |
| `j`/`k`, `↑`/`↓` | Move within the focused list (Skills pane, or the Executions lists when the right pane is focused via `Enter` from Skills… see below). |
| `Enter` (right pane, Executions) | Attach to a live session row. |
| `T` | Open the Trace Browser on the selected execution's session. |
| `PageUp`/`PageDown`, `Home`, `End`, mouse wheel | Scroll the right pane. |
| `Esc`, `q` | Unwind: clear a harness filter, then close the view. |

As implemented: `→` focuses the right pane and `←` returns to the list; `Enter` in the list launches or attaches, `Enter` in the Executions tab attaches to a live launch. `Esc` returns focus to the Skills pane before clearing a filter or closing. The mouse wheel moves the focused list or scrolls the detail pane; clicks are not handled.

### 4.3 Removed surfaces

- The `SidebarSection::Skills` variant, `draw_skills_sidebar`, `draw_generic_agent_preview` as a main-pane branch, and the `h` key. `Tab` cycles Active → History → Active. The sidebar layout becomes `[Percentage(40) Active, Min(4) History]`.
- The Trace Browser's `K` pane (`skills_pane`, `draw_skills_pane`, `filter_by_skill`). The "turns that loaded a skill" list now lives in the Skills view's Executions tab, and `T` from there opens the Trace Browser on a chosen turn.
- The main-pane trace briefing preview as a sidebar-driven branch; `draw_trace_briefing_preview` is kept and rendered inside the Briefing tab.

The `SkillLauncher` mode and `draw_skill_launcher` were removed: the row's harness is the launch harness, so no separate picker is needed.

## 5. Data model and queries

### 5.1 New evidence: skill id on the launch row

`LaunchPlan` and `MapSettings` gain `skill_id: Option<String>`. `App::launch_skill` sets it before `spawn_traced_with_env`; `map::launch_started`/`launch_adopted`/`launch_ended` write it into `launches.metadata` as `{"skill_id": "<id>", "skill_harness": "<harness>"}`. The upsert already merges metadata with `json_patch`, so no schema migration is required. Restored skill sessions (`persistence.rs` keeps `skill_id`) pass the same value on respawn.

Existing rows without the key are still recognized by the profile-name fallback `profile = '<display name> (<harness>)'` so history predating this change appears, labelled `(matched by name)` in the Executions tab.

### 5.2 New store queries (`src/tracing/store/query.rs`)

```sql
-- skill_launches(conn, skill_id, display_name, harness_provider, limit)
SELECT l.id, l.provider, l.profile, l.cwd, l.started_ns, l.ended_ns, l.termination, l.exit_code,
       l.reported_cost_usd, l.session_key,
       json_extract(l.metadata, '$.skill_id') IS NOT NULL AS by_id,
       (SELECT COUNT(*) FROM traces t WHERE t.launch_id = l.id) AS turns,
       (SELECT SUM(ts.total_cost_usd) FROM trace_stats ts WHERE ts.launch_id = l.id) AS total_cost_usd,
       EXISTS (SELECT 1 FROM runs r WHERE r.id = l.run_id AND r.ended_ns IS NULL) AND l.ended_ns IS NULL AS live
FROM launches l
WHERE (json_extract(l.metadata, '$.skill_id') = ?1 OR (json_extract(l.metadata, '$.skill_id') IS NULL AND l.profile = ?2))
  AND (?3 IS NULL OR l.provider = ?3)
ORDER BY l.started_ns DESC LIMIT ?4;
```

`?2` is `format!("{display_name} ({harness})")`, matching `skill::launch::session_name`. `traces_with_skill` is reused unchanged for the turns list, with a 200-row cap. The attribution flag per turn is `EXISTS (SELECT 1 FROM observations o WHERE o.trace_id = trace_stats.id AND o.skill = ?1)`, added as one column to a new variant `traces_with_skill_detail` rather than changing the existing function's signature.

Skill statistics come from `skill_stats` (existing view) through `inventory::skill_reports`, which already joins definitions, stats and missed triggers. The view reuses `SkillReport` so the numbers match `trace skills` exactly.

### 5.3 View state

```rust
pub struct SkillsViewState {
    conn: Option<rusqlite::Connection>,   // read-only, None when tracing is off
    error: Option<String>,
    packages: Vec<SkillDefinition>,       // skill::load_skills
    native: Vec<inventory::Definition>,   // inventory_all filtered to Kind::Skill
    rows: Vec<SkillRow>,                  // flattened, grouped by harness, headers included
    selected: usize,                      // index into rows, never a header
    harness_filter: Option<Harness>,
    focus: SkillsPane,                    // Skills | Detail
    tab: SkillsTab,                       // Details | Executions | Briefing
    executions: SkillExecutions,          // launches + turns for the selected row
    selected_execution: usize,
    scroll_offset: usize,
    viewport_rows: Cell<usize>,
    last_refresh: Instant,
    install: HashMap<(String, Harness), InstallStatus>,
    reports: Vec<inventory::SkillReport>,
    cwd: PathBuf, home: PathBuf, db_path: Option<PathBuf>,
}

pub enum SkillRow {
    Header(Harness),
    Package { id: String, harness: Harness },
    Native { index: usize, harness: Harness },
}
```

Loading order on open and on `r`: packages → inventory → install status per (package, harness) → store reports → executions for the selected row. Store failures set `error` and leave package and install data usable, mirroring `trace skills`.

Refresh while open: the 250 ms tick calls `refresh_if_live` at most every 500 ms; it re-queries only the executions of the selected row and the live-session markers (cheap), never the filesystem. The Briefing tab reuses the existing one-second gate.

### 5.4 Configuration and persistence

No new configuration. `sessions.json` is unchanged. The `hide_sidebar` behaviour is unchanged.

## 6. Rendering

`draw_skills_view(f, app, state)` in `src/ui.rs`: geometry `width = (w*96/100).clamp(60, 200)`, `height = (h*92/100).clamp(18, 60)`, centered, `Clear`, body split `[Percentage(34), Min(0)]`, footer one line. Headers render in bold yellow; package rows use the package icon, native rows `⚙`; the state column is right-aligned and coloured green (`installed ✓`, `running`), yellow (`stale`, `not managed`), grey (`not installed`). Tab labels render in the right pane's title with the active tab bold. Empty states: `no skills found\n\n~/.agent-mux/skills/` and, on the Executions tab, `no recorded executions on <harness>` plus the hint `launch it with Enter, or agent-mux trace import --discover for past sessions`.

Times are formatted with `fmt_time` (local time), costs with `fmt_cost`, durations with `fmt_ms`, consistent with the Trace Browser.

## 7. Key routing and modes

- `Mode::SkillsView(Box<SkillsViewState>)` is added; `Action::OpenSkillsView` is produced by `S` in Control mode.
- `App::handle_skills_key` mirrors `handle_browser_key`; mouse routing gets a `Mode::SkillsView` branch before the Control/Attached check so clicks never leak to the pane beneath.
- `DispatchCtx.sidebar_section` loses the `Skills` case; `dispatch` arms for `h` and the Skills-specific `Enter`/`r` are removed.
- The Control-mode hint strings become: Active: `[b] sidebar  [j/k] select  [Enter] attach  [Tab] history  [n] new  [l] logs  [S] skills  [t] trace  [T] traces  [?] help  [q] quit`; History: `[b] sidebar  [j/k] select  [Enter/r] restart  [Tab] active  [a] all  [n] new  [l] logs  [S] skills  [?] help  [q] quit`; sidebar hidden: add `[S] skills`.

## 8. Verification

Tests to add or change:

- `tests/skill_ui.rs`: replace the sidebar-section tests with: `S` opens the view; rows are grouped and ordered by harness; header rows are skipped by navigation; the harness filter toggles; `Enter` on a package installs and launches on the row's harness and marks the row `running`; `Enter` on a running row attaches; `i`/`u` change install state and the row label; `Esc` unwind order; `Tab` cycles two or three tabs depending on `trace.read`.
- `tests/skill_package.rs`: `launch_skill` stamps `metadata.skill_id` and `skill_harness` on the launch row (read back through `query::skill_launches`).
- `tests/trace_store.rs`: `skill_launches` matches by id first and by profile name only when the id is absent; live flag follows `runs.ended_ns` and `launches.ended_ns`.
- `tests/trace_inventory.rs`: the browser `K` assertions are removed; the same expectations move to the Skills view's Details tab.
- `src/ui.rs` unit tests: help overlay lists `S`, `i`, `u`, `1-3`; the Control hints mention `[S] skills`; the sidebar has two sections.
- `tests/persistent_sessions.rs`: a restored skill session still shows `running` in the view.

Manual check: open `S` with tracing disabled (store error shown, packages and install state still usable), with an empty store, and with a `~/.agent-mux/skills/heimdall` shadow package.

## 9. Documentation

When implemented, update in the same change:

- `README.md`: section 4 (layout diagram, sidebar description, panel-to-data map, Control-mode keys, remove 4.4's picker-first flow and the `K` paragraph in 4.6), add a "Skills view" subsection with the pane descriptions, keys and the `skill_launches` query; section 8 (SQL catalog row for `skill_launches` and `traces_with_skill_detail`); section 11 (launch flow now records `skill_id` on the launch row); section 14 (test table and troubleshooting rows).
- `docs/skills.md` section 4 ("Launching from the sidebar" becomes "Launching from the Skills view").
- `skills/heimdall/reference/agents.md`: replace the `l.profile LIKE ?1 || ' (%'` query with `json_extract(l.metadata, '$.skill_id')`.
- `AGENTS.md`: the one-line description of how Heimdall is launched.

## 10. Open questions for review

1. Should native harness skills be listed at all, or only agent-mux packages? Listing them makes the view the single answer to "what can this harness run", at the cost of rows the user cannot install or launch from here.
2. Keep the `H` harness-picker path, or rely solely on the row's harness?
3. Should `u` on a package that is also installed on other harnesses offer "uninstall everywhere"? The spec keeps it per row.
