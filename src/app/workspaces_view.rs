//! The Workspaces Explorer screen (`Mode::Workspaces`, F1 → "Open workspaces"):
//! scans the workspace tree from the launch directory (or any navigated root),
//! shows the detected technology stack, project summary and customization
//! files for each folder and sub-folder, rolls up agent activity across the
//! folder and its sub-folders, and launches or resumes an LLM agent directly
//! in the selected folder.

use crate::keymap::{self, Verb};
use crate::workspace::activity::ActivitySource;
use crate::workspace::scan::scan_workspace;
use crate::workspace::summary_cache::CachedSummary;
use crate::workspace::{
    ProjectSummary, SCAN_DEPTH, SummarySource, WorkspaceNode, cli::run_headless_summarize,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError, channel};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    #[default]
    Tree,
    Agents,
}

/// A selectable row in the right-hand "Agents in this folder" pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentItemRef {
    Live(usize),
    Scheduled(usize),
    Persona(usize),
    Recent(usize),
}

/// What a key in `Mode::Workspaces` asks `App` to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    None,
    Close,
    Reload,
    ChangeRoot(PathBuf),
    /// `Enter` on a folder: launch a new session immediately in `dir` with the default profile.
    LaunchDefaultInDir(PathBuf),
    /// `n` on a folder: open the New Session dialog pre-filled with `dir`.
    NewSessionDialogInDir(PathBuf),
    /// Launch a project persona in `dir`.
    LaunchPersonaInDir {
        agent: String,
        dir: PathBuf,
    },
    /// Attach to an active session in `App::sessions`.
    AttachSession(usize),
    /// Resume a past session in `cwd`.
    ResumeSession {
        provider: String,
        session_id: String,
        cwd: PathBuf,
    },
    /// Open `Mode::RunsView` focused on a loop.
    OpenLoopRuns(String),
    /// Trigger an immediate run of a scheduled loop.
    RunLoopNow(String),
    /// Open the scheduled loop editor.
    EditLoop(String),
    /// Open the persona editor.
    EditPersona(String),
    /// Open `README.md` / `AGENTS.md` / `CLAUDE.md` in `Mode::Markdown`.
    OpenMarkdown(PathBuf),
}

type SummarizeResult = (PathBuf, Result<CachedSummary, String>);

pub struct WorkspacesView {
    pub root: PathBuf,
    pub library_root: PathBuf,
    pub default_harness: String,
    pub nodes: Vec<WorkspaceNode>,
    pub filtered_indices: Vec<usize>,
    pub selected_tree: usize,
    pub selected_agent: usize,
    pub pane: Pane,
    pub filtering: bool,
    pub filter_text: String,
    pub summarizing: Option<PathBuf>,
    job: Option<Receiver<SummarizeResult>>,
    pub notice: Option<String>,
    pub error: Option<String>,
}

impl std::fmt::Debug for WorkspacesView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspacesView")
            .field("root", &self.root)
            .field("nodes_len", &self.nodes.len())
            .field("selected_tree", &self.selected_tree)
            .field("selected_agent", &self.selected_agent)
            .field("pane", &self.pane)
            .field("filtering", &self.filtering)
            .field("filter_text", &self.filter_text)
            .finish()
    }
}

impl WorkspacesView {
    pub fn open(
        root: PathBuf,
        library_root: PathBuf,
        default_harness: String,
        source: &ActivitySource,
    ) -> Self {
        let canon_root = root.canonicalize().unwrap_or(root);
        let mut nodes = scan_workspace(
            &canon_root,
            SCAN_DEPTH,
            Some(&library_root),
            &source.active_dirs(),
        );
        source.populate(&mut nodes);
        let filtered_indices = (0..nodes.len()).collect();
        WorkspacesView {
            root: canon_root,
            library_root,
            default_harness,
            nodes,
            filtered_indices,
            selected_tree: 0,
            selected_agent: 0,
            pane: Pane::Tree,
            filtering: false,
            filter_text: String::new(),
            summarizing: None,
            job: None,
            notice: None,
            error: None,
        }
    }

    pub fn rescan(&mut self, source: &ActivitySource) {
        let prev_path = self.selected_node().map(|n| n.path.clone());
        let mut nodes = scan_workspace(
            &self.root,
            SCAN_DEPTH,
            Some(&self.library_root),
            &source.active_dirs(),
        );
        source.populate(&mut nodes);
        self.nodes = nodes;
        self.recompute_filter();
        if let Some(prev) = prev_path
            && let Some(pos) = self
                .filtered_indices
                .iter()
                .position(|&idx| self.nodes[idx].path == prev)
        {
            self.selected_tree = pos;
        }
        self.clamp_selections();
    }

    pub fn set_root(&mut self, new_root: PathBuf, source: &ActivitySource) {
        self.root = new_root.canonicalize().unwrap_or(new_root);
        self.filter_text.clear();
        self.filtering = false;
        self.selected_tree = 0;
        self.selected_agent = 0;
        self.pane = Pane::Tree;
        self.rescan(source);
    }

    pub fn selected_node(&self) -> Option<&WorkspaceNode> {
        let idx = *self.filtered_indices.get(self.selected_tree)?;
        self.nodes.get(idx)
    }

    /// All selectable items in the right-hand Agents list for the currently
    /// selected folder.
    pub fn agent_items_for_selected(&self) -> Vec<AgentItemRef> {
        let Some(node) = self.selected_node() else {
            return Vec::new();
        };
        let mut items = Vec::new();
        for i in 0..node.activity.live_sessions.len() {
            items.push(AgentItemRef::Live(i));
        }
        for i in 0..node.activity.scheduled_agents.len() {
            items.push(AgentItemRef::Scheduled(i));
        }
        for i in 0..node.customization.project_personas.len() {
            items.push(AgentItemRef::Persona(i));
        }
        for i in 0..node.activity.recent_sessions.len() {
            items.push(AgentItemRef::Recent(i));
        }
        items
    }

    pub fn selected_agent_item(&self) -> Option<AgentItemRef> {
        let items = self.agent_items_for_selected();
        items.get(self.selected_agent).cloned()
    }

    pub fn recompute_filter(&mut self) {
        let q = self.filter_text.trim().to_ascii_lowercase();
        if q.is_empty() {
            self.filtered_indices = (0..self.nodes.len()).collect();
        } else {
            self.filtered_indices = self
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, n)| node_matches_query(n, &q))
                .map(|(i, _)| i)
                .collect();
        }
        self.clamp_selections();
    }

    fn clamp_selections(&mut self) {
        if self.filtered_indices.is_empty() {
            self.selected_tree = 0;
            self.selected_agent = 0;
            self.pane = Pane::Tree;
            return;
        }
        if self.selected_tree >= self.filtered_indices.len() {
            self.selected_tree = self.filtered_indices.len() - 1;
        }
        let agent_len = self.agent_items_for_selected().len();
        if agent_len == 0 {
            self.selected_agent = 0;
            if self.pane == Pane::Agents {
                self.pane = Pane::Tree;
            }
        } else if self.selected_agent >= agent_len {
            self.selected_agent = agent_len - 1;
        }
    }

    /// Polls any running background LLM summary job.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.job else {
            return false;
        };
        match rx.try_recv() {
            Ok((folder, result)) => {
                self.job = None;
                self.summarizing = None;
                match result {
                    Ok(cached) => {
                        if let Some(node) = self.nodes.iter_mut().find(|n| n.path == folder) {
                            node.summary = ProjectSummary {
                                text: cached.purpose,
                                source: SummarySource::CachedLlm {
                                    harness: cached.harness,
                                    generated_at: cached.generated_at,
                                    stale: false,
                                },
                                architecture: cached.architecture,
                                entry_points: cached.entry_points,
                            };
                        }
                        self.notice = Some(format!("Updated LLM summary for {}", folder.display()));
                        self.error = None;
                    }
                    Err(e) => {
                        self.error = Some(format!("LLM summary failed: {e}"));
                    }
                }
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                self.summarizing = None;
                true
            }
        }
    }

    pub fn start_summarize_selected(&mut self) {
        if self.job.is_some() {
            return;
        }
        let Some(node) = self.selected_node() else {
            return;
        };
        let folder = node.path.clone();
        let node_name = node.name.clone();
        let harness = self.default_harness.clone();
        let lib = self.library_root.clone();
        let (tx, rx) = channel();
        self.summarizing = Some(folder.clone());
        self.notice = Some(format!("Summarizing {node_name} with {harness}…"));
        self.error = None;
        self.job = Some(rx);
        std::thread::spawn(move || {
            let res = run_headless_summarize(&folder, &harness, &lib).map(|(c, _)| c);
            let _ = tx.send((folder, res));
        });
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        self.error = None;
        self.notice = None;

        if self.filtering {
            return self.handle_filter_key(key);
        }

        let aliased = keymap::list_alias(key);
        match aliased.code {
            KeyCode::Tab | KeyCode::BackTab => {
                if self.pane == Pane::Tree && !self.agent_items_for_selected().is_empty() {
                    self.pane = Pane::Agents;
                } else {
                    self.pane = Pane::Tree;
                }
                return Outcome::None;
            }
            KeyCode::Char('/') if key.modifiers.is_empty() => {
                self.filtering = true;
                return Outcome::None;
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                self.move_selection(1);
                return Outcome::None;
            }
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.move_selection(-1);
                return Outcome::None;
            }
            KeyCode::PageDown => {
                self.move_selection(8);
                return Outcome::None;
            }
            KeyCode::PageUp => {
                self.move_selection(-8);
                return Outcome::None;
            }
            KeyCode::Home => {
                self.jump_start();
                return Outcome::None;
            }
            KeyCode::End => {
                self.jump_end();
                return Outcome::None;
            }
            KeyCode::Right | KeyCode::Char('l') if key.modifiers.is_empty() => {
                if self.pane == Pane::Tree
                    && let Some(node) = self.selected_node()
                    && node.depth > 0
                    && !node.is_project
                {
                    return Outcome::ChangeRoot(node.path.clone());
                } else if self.pane == Pane::Tree && !self.agent_items_for_selected().is_empty() {
                    self.pane = Pane::Agents;
                }
                return Outcome::None;
            }
            KeyCode::Left | KeyCode::Char('h') if key.modifiers.is_empty() => {
                if self.pane == Pane::Agents {
                    self.pane = Pane::Tree;
                } else if let Some(parent) = self.root.parent() {
                    return Outcome::ChangeRoot(parent.to_path_buf());
                }
                return Outcome::None;
            }
            _ => {}
        }

        if let Some(verb) = keymap::verb(key) {
            match verb {
                Verb::Back => {
                    if self.pane == Pane::Agents {
                        self.pane = Pane::Tree;
                        return Outcome::None;
                    }
                    if !self.filter_text.is_empty() {
                        self.filter_text.clear();
                        self.recompute_filter();
                        return Outcome::None;
                    }
                    return Outcome::Close;
                }
                Verb::Reload => return Outcome::Reload,
                Verb::New => {
                    if let Some(node) = self.selected_node() {
                        return Outcome::NewSessionDialogInDir(node.path.clone());
                    }
                }
                Verb::Open => return self.activate_open(),
                Verb::Run => return self.activate_run(),
                Verb::Edit => return self.activate_edit(),
                Verb::Save => {
                    self.start_summarize_selected();
                    return Outcome::None;
                }
                _ => {}
            }
        }

        Outcome::None
    }

    fn handle_filter_key(&mut self, key: &KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc => {
                self.filtering = false;
                self.filter_text.clear();
                self.recompute_filter();
            }
            KeyCode::Enter | KeyCode::Down | KeyCode::Tab => {
                self.filtering = false;
            }
            KeyCode::Backspace => {
                self.filter_text.pop();
                self.recompute_filter();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter_text.clear();
                self.recompute_filter();
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.filter_text.push(c);
                self.recompute_filter();
            }
            _ => {}
        }
        Outcome::None
    }

    fn move_selection(&mut self, delta: isize) {
        match self.pane {
            Pane::Tree => {
                if self.filtered_indices.is_empty() {
                    return;
                }
                let max = (self.filtered_indices.len() - 1) as isize;
                let next = (self.selected_tree as isize + delta).clamp(0, max) as usize;
                if next != self.selected_tree {
                    self.selected_tree = next;
                    self.selected_agent = 0;
                }
            }
            Pane::Agents => {
                let items = self.agent_items_for_selected();
                if items.is_empty() {
                    return;
                }
                let max = (items.len() - 1) as isize;
                self.selected_agent = (self.selected_agent as isize + delta).clamp(0, max) as usize;
            }
        }
    }

    fn jump_start(&mut self) {
        match self.pane {
            Pane::Tree => {
                self.selected_tree = 0;
                self.selected_agent = 0;
            }
            Pane::Agents => self.selected_agent = 0,
        }
    }

    fn jump_end(&mut self) {
        match self.pane {
            Pane::Tree => {
                if !self.filtered_indices.is_empty() {
                    self.selected_tree = self.filtered_indices.len() - 1;
                    self.selected_agent = 0;
                }
            }
            Pane::Agents => {
                let len = self.agent_items_for_selected().len();
                if len > 0 {
                    self.selected_agent = len - 1;
                }
            }
        }
    }

    fn activate_open(&self) -> Outcome {
        let Some(node) = self.selected_node() else {
            return Outcome::None;
        };
        if self.pane == Pane::Tree {
            return Outcome::LaunchDefaultInDir(node.path.clone());
        }
        match self.selected_agent_item() {
            Some(AgentItemRef::Live(i)) => node
                .activity
                .live_sessions
                .get(i)
                .map(|l| Outcome::AttachSession(l.session_idx))
                .unwrap_or(Outcome::None),
            Some(AgentItemRef::Scheduled(i)) => node
                .activity
                .scheduled_agents
                .get(i)
                .map(|s| Outcome::OpenLoopRuns(s.loop_id.clone()))
                .unwrap_or(Outcome::None),
            Some(AgentItemRef::Persona(i)) => node
                .customization
                .project_personas
                .get(i)
                .map(|p| Outcome::LaunchPersonaInDir {
                    agent: p.name.clone(),
                    dir: node.path.clone(),
                })
                .unwrap_or(Outcome::None),
            Some(AgentItemRef::Recent(i)) => node
                .activity
                .recent_sessions
                .get(i)
                .map(|r| Outcome::ResumeSession {
                    provider: r.provider.clone(),
                    session_id: r.session_id.clone(),
                    cwd: r.cwd.clone(),
                })
                .unwrap_or(Outcome::None),
            None => Outcome::LaunchDefaultInDir(node.path.clone()),
        }
    }

    fn activate_run(&self) -> Outcome {
        let Some(node) = self.selected_node() else {
            return Outcome::None;
        };
        if self.pane == Pane::Agents {
            match self.selected_agent_item() {
                Some(AgentItemRef::Live(i)) => {
                    if let Some(l) = node.activity.live_sessions.get(i) {
                        return Outcome::AttachSession(l.session_idx);
                    }
                }
                Some(AgentItemRef::Scheduled(i)) => {
                    if let Some(s) = node.activity.scheduled_agents.get(i) {
                        return Outcome::RunLoopNow(s.loop_id.clone());
                    }
                }
                Some(AgentItemRef::Persona(i)) => {
                    if let Some(p) = node.customization.project_personas.get(i) {
                        return Outcome::LaunchPersonaInDir {
                            agent: p.name.clone(),
                            dir: node.path.clone(),
                        };
                    }
                }
                Some(AgentItemRef::Recent(i)) => {
                    if let Some(r) = node.activity.recent_sessions.get(i) {
                        return Outcome::ResumeSession {
                            provider: r.provider.clone(),
                            session_id: r.session_id.clone(),
                            cwd: r.cwd.clone(),
                        };
                    }
                }
                None => {}
            }
        }

        if let Some(latest) = node.activity.latest_resumable() {
            return Outcome::ResumeSession {
                provider: latest.provider.clone(),
                session_id: latest.session_id.clone(),
                cwd: latest.cwd.clone(),
            };
        }
        if let Some(live) = node.activity.live_sessions.first() {
            return Outcome::AttachSession(live.session_idx);
        }
        Outcome::LaunchDefaultInDir(node.path.clone())
    }

    fn activate_edit(&self) -> Outcome {
        let Some(node) = self.selected_node() else {
            return Outcome::None;
        };
        if self.pane == Pane::Agents {
            match self.selected_agent_item() {
                Some(AgentItemRef::Scheduled(i)) => {
                    if let Some(s) = node.activity.scheduled_agents.get(i) {
                        return Outcome::EditLoop(s.loop_id.clone());
                    }
                }
                Some(AgentItemRef::Persona(i)) => {
                    if let Some(p) = node.customization.project_personas.get(i) {
                        return Outcome::EditPersona(p.name.clone());
                    }
                }
                _ => {}
            }
        }
        if let Some(doc) = &node.customization.primary_doc {
            return Outcome::OpenMarkdown(doc.clone());
        }
        Outcome::None
    }
}

fn node_matches_query(node: &WorkspaceNode, query: &str) -> bool {
    let haystacks = [
        node.name.as_str(),
        node.rel_path.as_str(),
        node.summary.text.as_str(),
    ];
    query.split_whitespace().all(|word| {
        haystacks
            .iter()
            .any(|h| h.to_ascii_lowercase().contains(word))
            || node
                .stack
                .languages
                .iter()
                .any(|l| l.to_ascii_lowercase().contains(word))
            || node
                .stack
                .frameworks
                .iter()
                .any(|f| f.to_ascii_lowercase().contains(word))
            || node
                .stack
                .tools
                .iter()
                .any(|t| t.to_ascii_lowercase().contains(word))
            || node
                .customization
                .project_personas
                .iter()
                .any(|p| p.name.to_ascii_lowercase().contains(word))
            || node
                .activity
                .scheduled_agents
                .iter()
                .any(|a| a.loop_id.to_ascii_lowercase().contains(word))
    })
}

/// Formats `path` with `~/` when under the user's home directory.
pub fn tilde_path(path: &Path) -> String {
    let home = crate::skill::install::home_dir();
    match path.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => {
            if rest.as_os_str().is_empty() {
                "~".into()
            } else {
                format!("~/{}", rest.display())
            }
        }
        _ => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn workspaces_view_navigates_filters_and_emits_launch_and_resume_outcomes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let api = root.join("api");
        let web = root.join("web");
        fs::create_dir_all(&api).unwrap();
        fs::create_dir_all(&web).unwrap();
        fs::write(
            api.join("Cargo.toml"),
            "[package]\nname = \"api\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(api.join("AGENTS.md"), "# API rules\n\nRun cargo test.\n").unwrap();
        fs::write(
            web.join("package.json"),
            r#"{"name":"web","dependencies":{"react":"19.0.0"}}"#,
        )
        .unwrap();

        let api_canon = api.canonicalize().unwrap();
        let now_ns = 1_700_000_000_000_000_000i64;
        let source = ActivitySource {
            live: Vec::new(),
            traced_sessions: vec![crate::tracing::store::query::SessionStat {
                key: "claude:s-api".into(),
                provider: "claude".into(),
                session_id: "s-api".into(),
                title: Some("Fix API handler".into()),
                cwd: Some(api_canon.to_string_lossy().into_owned()),
                project_slug: None,
                transcript_path: None,
                first_seen_ns: now_ns - 10_000,
                last_seen_ns: now_ns - 5_000,
                turn_count: 3,
                open_turns: 0,
                duration_ms: Some(1000),
                observation_count: 5,
                tool_count: 2,
                error_count: 0,
                input_tokens: Some(1000),
                output_tokens: Some(500),
                cache_read_tokens: None,
                cache_write_tokens: None,
                total_tokens: Some(1500),
                total_cost_usd: Some(0.05),
                reported_cost_usd: None,
                unpriced_generations: 0,
                correlation: None,
            }],
            history_sessions: Vec::new(),
            loops: Vec::new(),
            loop_last_outcomes: std::collections::HashMap::new(),
            now_ns,
        };

        let mut view = WorkspacesView::open(
            root.to_path_buf(),
            root.join(".lib"),
            "claude".into(),
            &source,
        );
        assert_eq!(view.nodes.len(), 3); // root, api, web

        // Move down to `api`
        assert_eq!(view.handle_key(&key(KeyCode::Down)), Outcome::None);
        assert_eq!(view.selected_node().unwrap().name, "api");

        // `Enter` launches default agent in `api`
        assert_eq!(
            view.handle_key(&key(KeyCode::Enter)),
            Outcome::LaunchDefaultInDir(api_canon.clone())
        );

        // `n` opens New Session dialog in `api`
        assert_eq!(
            view.handle_key(&key(KeyCode::Char('n'))),
            Outcome::NewSessionDialogInDir(api_canon.clone())
        );

        // `r` resumes the latest session in `api`
        assert_eq!(
            view.handle_key(&key(KeyCode::Char('r'))),
            Outcome::ResumeSession {
                provider: "claude".into(),
                session_id: "s-api".into(),
                cwd: api_canon.clone(),
            }
        );

        // `e` opens AGENTS.md in Markdown viewer
        assert_eq!(
            view.handle_key(&key(KeyCode::Char('e'))),
            Outcome::OpenMarkdown(api_canon.join("AGENTS.md"))
        );

        // Filter by "react" -> narrows to `web`
        view.handle_key(&key(KeyCode::Char('/')));
        for ch in "react".chars() {
            view.handle_key(&key(KeyCode::Char(ch)));
        }
        view.handle_key(&key(KeyCode::Enter));
        assert_eq!(view.filtered_indices.len(), 1);
        assert_eq!(view.selected_node().unwrap().name, "web");
    }
}
