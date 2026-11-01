//! The Langfuse screen: inspects remote traces and observations from
//! Langfuse's public REST API, browses execution trees, and deep-links into
//! the Langfuse Web UI.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;

use crate::config::ResolvedLangfuse;
use crate::tracing::langfuse::api::{
    LangfuseObservation, LangfuseTraceDetail, LangfuseTraceSummary, ListTracesParams, get_trace,
    list_traces, resolve_web_url,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LangfusePane {
    #[default]
    Traces,
    Tree,
    Detail,
}

impl LangfusePane {
    pub fn next(self) -> Self {
        match self {
            Self::Traces => Self::Tree,
            Self::Tree => Self::Detail,
            Self::Detail => Self::Traces,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            Self::Traces => Self::Detail,
            Self::Tree => Self::Traces,
            Self::Detail => Self::Tree,
        }
    }
}

/// An item in the flattened observation tree.
#[derive(Debug, Clone)]
pub struct ObservationTreeNode {
    pub observation_id: String,
    pub name: String,
    pub obs_type: String,
    pub model: Option<String>,
    pub depth: usize,
    pub is_expanded: bool,
    pub has_children: bool,
    pub duration_s: Option<f64>,
    pub tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchStatus {
    Idle,
    Loading(String),
    Error(String),
}

/// Events sent from background worker thread to the view.
pub enum WorkerEvent {
    TracesLoaded {
        page: u64,
        traces: Vec<LangfuseTraceSummary>,
        total: Option<u64>,
        project_id: Option<String>,
    },
    TraceDetailLoaded {
        detail: Box<LangfuseTraceDetail>,
    },
    Error(String),
}

/// What a key action asks of the App.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    None,
    Close,
    OpenUrl(String),
}

pub struct LangfuseView {
    pub langfuse: Option<ResolvedLangfuse>,
    pub pane: LangfusePane,
    pub zoomed: bool,
    pub status: FetchStatus,
    pub project_id: Option<String>,

    pub traces: Vec<LangfuseTraceSummary>,
    pub selected_trace: usize,
    pub trace_scroll: usize,

    pub tree_nodes: Vec<ObservationTreeNode>,
    pub selected_node: usize,
    pub tree_scroll: usize,

    pub cached_details: HashMap<String, LangfuseTraceDetail>,
    pub current_detail: Option<LangfuseTraceDetail>,
    pub detail_scroll: usize,

    pub filter_input: Option<String>,
    pub active_filter: Option<String>,
    pub session_filter: Option<String>,

    pub page: u64,
    pub total_pages: u64,
    pub total_items: Option<u64>,

    worker_rx: Option<Receiver<WorkerEvent>>,
    worker_tx: Sender<WorkerEvent>,
}

impl std::fmt::Debug for LangfuseView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LangfuseView")
            .field("configured", &self.langfuse.is_some())
            .field("pane", &self.pane)
            .field("traces", &self.traces.len())
            .field("selected_trace", &self.selected_trace)
            .field("status", &self.status)
            .finish()
    }
}

impl LangfuseView {
    pub fn new(langfuse: Option<ResolvedLangfuse>, session_filter: Option<String>) -> Self {
        let (tx, rx) = channel();
        let project_id = langfuse.as_ref().and_then(|lf| lf.project_id.clone());
        let mut view = LangfuseView {
            langfuse,
            pane: LangfusePane::Traces,
            zoomed: false,
            status: FetchStatus::Idle,
            project_id,
            traces: Vec::new(),
            selected_trace: 0,
            trace_scroll: 0,
            tree_nodes: Vec::new(),
            selected_node: 0,
            tree_scroll: 0,
            cached_details: HashMap::new(),
            current_detail: None,
            detail_scroll: 0,
            filter_input: None,
            active_filter: None,
            session_filter,
            page: 1,
            total_pages: 1,
            total_items: None,
            worker_rx: Some(rx),
            worker_tx: tx,
        };
        view.refresh();
        view
    }

    pub fn text_entry_active(&self) -> bool {
        self.filter_input.is_some()
    }

    /// Pumps any completed background worker events.
    pub fn poll(&mut self) {
        let mut events = Vec::new();
        if let Some(rx) = &self.worker_rx {
            while let Ok(evt) = rx.try_recv() {
                events.push(evt);
            }
        }

        for evt in events {
            match evt {
                WorkerEvent::TracesLoaded {
                    page,
                    traces,
                    total,
                    project_id,
                } => {
                    self.status = FetchStatus::Idle;
                    self.page = page;
                    self.total_items = total;
                    if let Some(t) = total {
                        self.total_pages = t.div_ceil(50);
                    }
                    if self.project_id.is_none() && project_id.is_some() {
                        self.project_id = project_id;
                    }
                    self.traces = traces;
                    if self.selected_trace >= self.traces.len() && !self.traces.is_empty() {
                        self.selected_trace = self.traces.len() - 1;
                    }
                    self.load_selected_trace_detail();
                }
                WorkerEvent::TraceDetailLoaded { detail } => {
                    self.status = FetchStatus::Idle;
                    let id = detail.id.clone();
                    self.cached_details.insert(id.clone(), *detail.clone());
                    if self
                        .traces
                        .get(self.selected_trace)
                        .is_some_and(|t| t.id == id)
                    {
                        self.current_detail = Some(*detail);
                        self.rebuild_tree();
                    }
                }
                WorkerEvent::Error(err) => {
                    self.status = FetchStatus::Error(err);
                }
            }
        }
    }

    /// Triggers a fresh load of traces matching current filters and page.
    pub fn refresh(&mut self) {
        let Some(lf) = self.langfuse.clone() else {
            self.status = FetchStatus::Error("Langfuse is not configured".into());
            return;
        };

        self.status = FetchStatus::Loading("Fetching traces...".into());
        let tx = self.worker_tx.clone();
        let page = self.page;
        let session_id = self.session_filter.clone();
        let query = self.active_filter.clone();

        thread::spawn(move || {
            let params = ListTracesParams {
                page: Some(page),
                limit: Some(50),
                session_id,
                name: query,
                order_by: Some("timestamp.desc".into()),
                ..Default::default()
            };

            match list_traces(&lf, &params) {
                Ok(resp) => {
                    let first_project_id = resp.data.iter().find_map(|t| t.project_id.clone());
                    let _ = tx.send(WorkerEvent::TracesLoaded {
                        page: resp.meta.page,
                        traces: resp.data,
                        total: resp.meta.total_items,
                        project_id: first_project_id,
                    });
                }
                Err(err) => {
                    let _ = tx.send(WorkerEvent::Error(err));
                }
            }
        });
    }

    fn load_selected_trace_detail(&mut self) {
        let Some(trace) = self.traces.get(self.selected_trace) else {
            self.current_detail = None;
            self.tree_nodes.clear();
            return;
        };

        let trace_id = trace.id.clone();
        if let Some(cached) = self.cached_details.get(&trace_id) {
            self.current_detail = Some(cached.clone());
            self.rebuild_tree();
            return;
        }

        let Some(lf) = self.langfuse.clone() else {
            return;
        };
        let tx = self.worker_tx.clone();
        self.status = FetchStatus::Loading(format!("Loading trace {trace_id}..."));

        thread::spawn(move || match get_trace(&lf, &trace_id) {
            Ok(detail) => {
                let _ = tx.send(WorkerEvent::TraceDetailLoaded {
                    detail: Box::new(detail),
                });
            }
            Err(err) => {
                let _ = tx.send(WorkerEvent::Error(err));
            }
        });
    }

    fn rebuild_tree(&mut self) {
        self.tree_nodes.clear();
        self.selected_node = 0;
        self.tree_scroll = 0;
        let Some(detail) = &self.current_detail else {
            return;
        };

        // Root trace node
        self.tree_nodes.push(ObservationTreeNode {
            observation_id: detail.id.clone(),
            name: detail.name.clone().unwrap_or_else(|| "trace".into()),
            obs_type: "TRACE".into(),
            model: None,
            depth: 0,
            is_expanded: true,
            has_children: !detail.observations.is_empty(),
            duration_s: detail.latency,
            tokens: None,
            cost_usd: detail.total_cost,
            is_error: false,
        });

        // Group observations by parent ID
        let mut by_parent: HashMap<Option<String>, Vec<&LangfuseObservation>> = HashMap::new();
        for obs in &detail.observations {
            by_parent
                .entry(obs.parent_observation_id.clone())
                .or_default()
                .push(obs);
        }

        // Recursively build tree nodes
        fn append_children(
            parent_id: Option<String>,
            depth: usize,
            by_parent: &HashMap<Option<String>, Vec<&LangfuseObservation>>,
            out: &mut Vec<ObservationTreeNode>,
        ) {
            if let Some(children) = by_parent.get(&parent_id) {
                for child in children {
                    let child_id = child.id.clone();
                    let has_children = by_parent.contains_key(&Some(child_id.clone()));
                    let is_error =
                        child.level.as_deref() == Some("ERROR") || child.status_message.is_some();
                    let tokens = child.usage.as_ref().and_then(|u| u.total_tokens);

                    out.push(ObservationTreeNode {
                        observation_id: child_id.clone(),
                        name: child
                            .name
                            .clone()
                            .unwrap_or_else(|| child.observation_type.clone()),
                        obs_type: child.observation_type.clone(),
                        model: child.model.clone(),
                        depth,
                        is_expanded: true,
                        has_children,
                        duration_s: child.latency,
                        tokens,
                        cost_usd: child.calculated_total_cost,
                        is_error,
                    });

                    append_children(Some(child_id), depth + 1, by_parent, out);
                }
            }
        }

        // Add root-level observations (parent is None or parent is the trace id)
        append_children(None, 1, &by_parent, &mut self.tree_nodes);
        append_children(Some(detail.id.clone()), 1, &by_parent, &mut self.tree_nodes);
    }

    /// Returns the currently highlighted observation object, if any.
    pub fn selected_observation(&self) -> Option<&LangfuseObservation> {
        let node = self.tree_nodes.get(self.selected_node)?;
        self.current_detail
            .as_ref()?
            .observations
            .iter()
            .find(|o| o.id == node.observation_id)
    }

    /// Resolves the URL for web launch based on current selection.
    pub fn current_web_url(&self) -> Option<String> {
        let lf = self.langfuse.as_ref()?;
        let project_id = self.project_id.as_deref();
        let trace = self.traces.get(self.selected_trace);
        let trace_id = trace.map(|t| t.id.as_str());
        let session_id = trace
            .and_then(|t| t.session_id.as_deref())
            .or(self.session_filter.as_deref());

        Some(resolve_web_url(&lf.host, project_id, trace_id, session_id))
    }

    /// Handles keyboard events.
    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        self.poll();

        // If filter text entry is active:
        if let Some(input) = &mut self.filter_input {
            match key.code {
                KeyCode::Esc => {
                    self.filter_input = None;
                    return Outcome::None;
                }
                KeyCode::Enter => {
                    let text = input.trim().to_string();
                    self.active_filter = if text.is_empty() { None } else { Some(text) };
                    self.filter_input = None;
                    self.page = 1;
                    self.refresh();
                    return Outcome::None;
                }
                KeyCode::Backspace => {
                    input.pop();
                    return Outcome::None;
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    return Outcome::None;
                }
                _ => return Outcome::None,
            }
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Outcome::Close,
            KeyCode::Tab => {
                self.pane = self.pane.next();
                Outcome::None
            }
            KeyCode::BackTab => {
                self.pane = self.pane.prev();
                Outcome::None
            }
            KeyCode::Char('z') => {
                self.zoomed = !self.zoomed;
                Outcome::None
            }
            KeyCode::Char('w') | KeyCode::Char('o') => {
                if let Some(url) = self.current_web_url() {
                    Outcome::OpenUrl(url)
                } else {
                    Outcome::None
                }
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cached_details.clear();
                self.refresh();
                Outcome::None
            }
            KeyCode::Char('/') => {
                self.filter_input = Some(self.active_filter.clone().unwrap_or_default());
                Outcome::None
            }
            KeyCode::Char('n') => {
                if self.page < self.total_pages {
                    self.page += 1;
                    self.refresh();
                }
                Outcome::None
            }
            KeyCode::Char('p') => {
                if self.page > 1 {
                    self.page -= 1;
                    self.refresh();
                }
                Outcome::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                Outcome::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                Outcome::None
            }
            KeyCode::PageUp => {
                self.move_selection(-10);
                Outcome::None
            }
            KeyCode::PageDown => {
                self.move_selection(10);
                Outcome::None
            }
            KeyCode::Home | KeyCode::Char('g') => {
                self.move_to_boundary(true);
                Outcome::None
            }
            KeyCode::End | KeyCode::Char('G') => {
                self.move_to_boundary(false);
                Outcome::None
            }
            _ => Outcome::None,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.pane {
            LangfusePane::Traces => {
                if self.traces.is_empty() {
                    return;
                }
                let new_idx = (self.selected_trace as isize + delta)
                    .clamp(0, self.traces.len() as isize - 1)
                    as usize;
                if new_idx != self.selected_trace {
                    self.selected_trace = new_idx;
                    self.load_selected_trace_detail();
                }
            }
            LangfusePane::Tree => {
                if self.tree_nodes.is_empty() {
                    return;
                }
                self.selected_node = (self.selected_node as isize + delta)
                    .clamp(0, self.tree_nodes.len() as isize - 1)
                    as usize;
            }
            LangfusePane::Detail => {
                self.detail_scroll = (self.detail_scroll as isize + delta).max(0) as usize;
            }
        }
    }

    fn move_to_boundary(&mut self, top: bool) {
        match self.pane {
            LangfusePane::Traces => {
                if !self.traces.is_empty() {
                    self.selected_trace = if top { 0 } else { self.traces.len() - 1 };
                    self.load_selected_trace_detail();
                }
            }
            LangfusePane::Tree => {
                if !self.tree_nodes.is_empty() {
                    self.selected_node = if top { 0 } else { self.tree_nodes.len() - 1 };
                }
            }
            LangfusePane::Detail => {
                self.detail_scroll = if top { 0 } else { 200 };
            }
        }
    }
}
