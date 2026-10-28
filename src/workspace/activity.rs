//! Agent activity aggregation per workspace folder and across its sub-folders.
//!
//! Combines:
//! - Live sessions from `App::sessions`
//! - Traced sessions and 30-day token/cost telemetry from `traces.db` (read-only)
//! - On-disk transcript sessions (`history::discover_sessions`)
//! - Scheduled loops (`agents::schedule::load_registry`) and latest `loop_runs` outcomes

use super::{
    FolderActivity, LiveSessionItem, RecentSessionItem, RollupStats, ScheduledAgentItem,
    WorkspaceNode,
};
use crate::harness::Harness;
use crate::history::{self, AgentProvider, SessionSummary};
use crate::loops::registry::LoopEntry;
use crate::tracing::store::query::{self, SessionFilter, SessionStat};
use rusqlite::{Connection, params};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// 30 days in nanoseconds.
pub const WINDOW_30D_NS: i64 = 30 * 86_400 * 1_000_000_000;

/// Maximum recent sessions attached to one folder's dossier list.
pub const MAX_RECENT_PER_FOLDER: usize = 12;

/// Snapshot of a live session in `App::sessions` passed into the aggregator.
#[derive(Debug, Clone)]
pub struct LiveSessionSnapshot {
    pub session_idx: usize,
    pub session_id: usize,
    pub profile_name: String,
    pub harness: Option<Harness>,
    pub status_label: String,
    pub is_running: bool,
    pub dir: PathBuf,
}

/// Raw inputs loaded once per scan and distributed across all `WorkspaceNode`s.
#[derive(Debug, Default)]
pub struct ActivitySource {
    pub live: Vec<LiveSessionSnapshot>,
    pub traced_sessions: Vec<SessionStat>,
    pub history_sessions: Vec<SessionSummary>,
    pub loops: Vec<LoopEntry>,
    pub loop_last_outcomes: HashMap<String, (String, Option<String>)>,
    pub now_ns: i64,
}

impl ActivitySource {
    /// Loads activity from the read-only trace store, on-disk session history,
    /// and the loop registry.
    pub fn load(
        trace_db_path: Option<&Path>,
        loops_file: Option<&Path>,
        library_root: &Path,
        live: Vec<LiveSessionSnapshot>,
    ) -> Self {
        let now_ns = crate::tracing::store::now_ns();
        let since_30d = now_ns.saturating_sub(WINDOW_30D_NS);

        let mut traced_sessions = Vec::new();
        let mut loop_last_outcomes = HashMap::new();
        if let Some(db_path) = trace_db_path
            && let Ok(conn) = crate::tracing::store::open_ro(db_path)
        {
            if let Ok(rows) = query::list_sessions(
                &conn,
                &SessionFilter {
                    project_slug: None,
                    since_ns: Some(since_30d),
                    limit: 500,
                },
            ) {
                traced_sessions = rows;
            }
            loop_last_outcomes = query_latest_loop_outcomes(&conn);
        }

        let default_loops = crate::loops::registry::registry_path()
            .unwrap_or_else(|| library_root.join("loops.json"));
        let loops_path = loops_file.unwrap_or(&default_loops);
        let (reg, _) = crate::agents::schedule::load_registry(loops_path, library_root);

        let claude_dir = history::default_claude_dir();
        let agy_dir = history::default_antigravity_dir();
        let history_sessions =
            history::discover_sessions(claude_dir.as_deref(), agy_dir.as_deref(), None, true);

        ActivitySource {
            live,
            traced_sessions,
            history_sessions,
            loops: reg.loops,
            loop_last_outcomes,
            now_ns,
        }
    }

    /// Canonicalized directories that have any recorded or live agent activity.
    pub fn active_dirs(&self) -> HashSet<PathBuf> {
        let mut dirs = HashSet::new();
        for l in &self.live {
            dirs.insert(canon(&l.dir));
        }
        for s in &self.traced_sessions {
            if let Some(cwd) = &s.cwd {
                dirs.insert(canon(Path::new(cwd)));
            }
        }
        for h in &self.history_sessions {
            if let Some(cwd) = &h.cwd {
                dirs.insert(canon(cwd));
            }
        }
        for lp in &self.loops {
            dirs.insert(canon(&lp.workspace));
        }
        dirs
    }

    /// Populates `node.activity` for every node in `nodes`.
    pub fn populate(&self, nodes: &mut [WorkspaceNode]) {
        // Normalize all session records into a unified list sorted newest-first.
        let unified_sessions = self.unified_recent_sessions();
        let canon_loops: Vec<(PathBuf, &LoopEntry)> = self
            .loops
            .iter()
            .map(|l| (canon(&l.workspace), l))
            .collect();
        let canon_live: Vec<(PathBuf, &LiveSessionSnapshot)> =
            self.live.iter().map(|l| (canon(&l.dir), l)).collect();

        for node in nodes.iter_mut() {
            let folder = &node.path;
            let mut live_items = Vec::new();
            for (dir, l) in &canon_live {
                if dir == folder || dir.starts_with(folder) {
                    live_items.push(LiveSessionItem {
                        session_idx: l.session_idx,
                        session_id: l.session_id,
                        profile_name: l.profile_name.clone(),
                        harness: l.harness,
                        status_label: l.status_label.clone(),
                        is_running: l.is_running,
                        dir: dir.clone(),
                        rel_subpath: relative_subpath(folder, dir),
                    });
                }
            }

            let mut scheduled_items = Vec::new();
            for (ws, lp) in &canon_loops {
                if ws == folder || ws.starts_with(folder) {
                    let (last_outcome, last_run_at) = self
                        .loop_last_outcomes
                        .get(&lp.id)
                        .map(|(o, t)| (Some(o.clone()), t.clone()))
                        .unwrap_or((None, None));
                    scheduled_items.push(ScheduledAgentItem {
                        loop_id: lp.id.clone(),
                        agent_name: lp.agent.clone(),
                        pattern: lp.pattern.clone(),
                        harness: lp.harness.clone(),
                        interval_s: lp.interval_s,
                        paused: lp.paused(),
                        last_outcome,
                        last_run_at,
                        workspace: ws.clone(),
                        rel_subpath: relative_subpath(folder, ws),
                    });
                }
            }

            let mut stats = RollupStats::default();
            let mut by_harness_map: BTreeMap<String, usize> = BTreeMap::new();
            let mut recent_items = Vec::new();

            for u in &unified_sessions {
                if &u.cwd == folder || u.cwd.starts_with(folder) {
                    let is_direct = &u.cwd == folder;
                    if is_direct {
                        stats.direct_sessions_30d += 1;
                    }
                    stats.subtree_sessions_30d += 1;
                    if let Some(tok) = u.total_tokens {
                        stats.total_tokens_30d += tok;
                    }
                    if let Some(cost) = u.total_cost_usd {
                        stats.total_cost_usd_30d += cost;
                    }
                    stats.last_active_ns = Some(
                        stats
                            .last_active_ns
                            .map_or(u.last_seen_ns, |prev| prev.max(u.last_seen_ns)),
                    );
                    *by_harness_map.entry(u.provider.clone()).or_insert(0) += 1;

                    if recent_items.len() < MAX_RECENT_PER_FOLDER {
                        let mut item = u.clone();
                        item.rel_subpath = relative_subpath(folder, &u.cwd);
                        recent_items.push(item);
                    }
                }
            }

            stats.by_harness = by_harness_map.into_iter().collect();
            node.activity = FolderActivity {
                live_sessions: live_items,
                scheduled_agents: scheduled_items,
                recent_sessions: recent_items,
                stats,
            };
        }
    }

    fn unified_recent_sessions(&self) -> Vec<RecentSessionItem> {
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut items: Vec<RecentSessionItem> = Vec::new();

        for s in &self.traced_sessions {
            let Some(cwd_str) = &s.cwd else { continue };
            let cwd = canon(Path::new(cwd_str));
            seen_ids.insert(s.session_id.clone());
            let title = s
                .title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or("(untitled session)")
                .to_string();
            let cost = s.total_cost_usd.or(s.reported_cost_usd);
            items.push(RecentSessionItem {
                session_id: s.session_id.clone(),
                provider: s.provider.clone(),
                title: one_line(&title, 72),
                last_seen_ns: s.last_seen_ns,
                age_label: format_relative_age(self.now_ns, s.last_seen_ns),
                turn_count: s.turn_count,
                total_tokens: s.total_tokens,
                total_cost_usd: cost,
                cwd,
                rel_subpath: None,
            });
        }

        let since_30d = self.now_ns.saturating_sub(WINDOW_30D_NS);
        for h in &self.history_sessions {
            if seen_ids.contains(&h.session_id) {
                continue;
            }
            let Some(cwd_raw) = &h.cwd else { continue };
            let modified_ns = h
                .modified
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
                .unwrap_or(0);
            if modified_ns < since_30d {
                continue;
            }
            seen_ids.insert(h.session_id.clone());
            let provider = match h.provider {
                AgentProvider::Claude => "claude".to_string(),
                AgentProvider::Antigravity => "antigravity".to_string(),
            };
            items.push(RecentSessionItem {
                session_id: h.session_id.clone(),
                provider,
                title: one_line(&h.title, 72),
                last_seen_ns: modified_ns,
                age_label: format_relative_age(self.now_ns, modified_ns),
                turn_count: h.turn_count as i64,
                total_tokens: None,
                total_cost_usd: None,
                cwd: canon(cwd_raw),
                rel_subpath: None,
            });
        }

        items.sort_by_key(|a| std::cmp::Reverse(a.last_seen_ns));
        items
    }
}

fn query_latest_loop_outcomes(conn: &Connection) -> HashMap<String, (String, Option<String>)> {
    let mut out = HashMap::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT loop_id, outcome, started_ns, scheduled_ns
         FROM loop_runs
         ORDER BY scheduled_ns DESC
         LIMIT 200",
    ) else {
        return out;
    };
    let now_ns = crate::tracing::store::now_ns();
    let Ok(rows) = stmt.query_map(params![], |r| {
        let loop_id: String = r.get(0)?;
        let outcome: String = r.get(1)?;
        let started: Option<i64> = r.get(2)?;
        let scheduled: i64 = r.get(3)?;
        Ok((loop_id, outcome, started.unwrap_or(scheduled)))
    }) else {
        return out;
    };
    for row in rows.flatten() {
        out.entry(row.0)
            .or_insert_with(|| (row.1, Some(format_relative_age(now_ns, row.2))));
    }
    out
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

fn relative_subpath(parent: &Path, child: &Path) -> Option<String> {
    let rel = child.strip_prefix(parent).ok()?;
    if rel.as_os_str().is_empty() {
        None
    } else {
        Some(rel.to_string_lossy().into_owned())
    }
}

fn one_line(s: &str, max_chars: usize) -> String {
    let joined = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = joined.chars().count();
    if count <= max_chars {
        joined
    } else {
        let mut head: String = joined.chars().take(max_chars.saturating_sub(1)).collect();
        head.push('…');
        head
    }
}

pub fn format_relative_age(now_ns: i64, then_ns: i64) -> String {
    if then_ns <= 0 || now_ns <= then_ns {
        return "just now".into();
    }
    let secs = ((now_ns - then_ns) / 1_000_000_000).max(0);
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

pub fn format_interval(secs: u64) -> String {
    if secs == 0 {
        return "on demand".into();
    }
    if secs.is_multiple_of(86_400) {
        format!("{}d", secs / 86_400)
    } else if secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}
