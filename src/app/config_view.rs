//! The Configuration view (`C`): every prompt, skill, loop pattern, loop
//! skill, loop agent and template agent-mux ships, with its source
//! (built-in, override, user), its validation state and its effective
//! text. Edits happen in the user's editor through `App::editor_request`;
//! resets, new items and the push into loop workspaces go through the
//! catalog (`crate::assets`). This module holds state and detail text only.

use crate::assets::{Asset, Catalog, Kind, Source, WorkspaceCopy};
use crate::loops::registry::Registry;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::cell::Cell;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigPane {
    List,
    Detail,
}

/// One row of the left pane. Headers are not selectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigRow {
    Header(Kind),
    /// Index into `Catalog::assets`.
    Item(usize),
}

/// A footer question waiting on the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    None,
    /// `R`: reset the selected item to the built-in text (or delete it).
    Reset,
    /// `u`: rewrite loop skills and agents in every registered workspace.
    Push,
    /// `n`: the name of a new item.
    NewName {
        kind: Kind,
        input: String,
    },
}

pub struct ConfigViewState {
    pub root: PathBuf,
    pub settings_path: Option<PathBuf>,
    pub catalog: Catalog,
    pub rows: Vec<ConfigRow>,
    /// Index into `rows`; never a header while an item exists.
    pub selected: usize,
    pub focus: ConfigPane,
    pub scroll_offset: usize,
    /// Right-pane interior height, written back by the renderer.
    pub viewport_rows: Cell<usize>,
    pub detail_lines: Vec<Line<'static>>,
    pub pending: Pending,
    /// Copies of the selected loop skill or agent in registered workspaces.
    pub copies: Vec<WorkspaceCopy>,
}

impl ConfigViewState {
    pub fn new(root: &Path, settings_path: Option<&Path>, registry: &Registry) -> Self {
        let mut v = ConfigViewState {
            root: root.to_path_buf(),
            settings_path: settings_path.map(Path::to_path_buf),
            catalog: Catalog::load(root, settings_path),
            rows: Vec::new(),
            selected: 0,
            focus: ConfigPane::List,
            scroll_offset: 0,
            viewport_rows: Cell::new(0),
            detail_lines: Vec::new(),
            pending: Pending::None,
            copies: Vec::new(),
        };
        v.rebuild_rows();
        v.rebuild_detail(registry);
        v
    }

    /// Rescans the library, keeping the selection on the same id.
    pub fn reload(&mut self, registry: &Registry) {
        let keep = self.selected_asset().map(|a| a.id.clone());
        self.catalog = Catalog::load(&self.root, self.settings_path.as_deref());
        self.rebuild_rows();
        if let Some(id) = keep
            && let Some(i) = self
                .rows
                .iter()
                .position(|r| matches!(r, ConfigRow::Item(i) if self.catalog.assets[*i].id == id))
        {
            self.selected = i;
        }
        self.rebuild_detail(registry);
    }

    /// Moves the selection to the row of `id`, when listed.
    pub fn select_id(&mut self, id: &str, registry: &Registry) {
        if let Some(i) = self
            .rows
            .iter()
            .position(|r| matches!(r, ConfigRow::Item(i) if self.catalog.assets[*i].id == id))
        {
            self.selected = i;
            self.rebuild_detail(registry);
        }
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        for kind in Kind::ALL {
            let items: Vec<usize> = self
                .catalog
                .assets
                .iter()
                .enumerate()
                .filter(|(_, a)| a.kind == kind)
                .map(|(i, _)| i)
                .collect();
            if items.is_empty() {
                continue;
            }
            self.rows.push(ConfigRow::Header(kind));
            self.rows.extend(items.into_iter().map(ConfigRow::Item));
        }
        self.ensure_selectable();
    }

    fn ensure_selectable(&mut self) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = self.selected.min(self.rows.len() - 1);
        if matches!(self.rows[self.selected], ConfigRow::Header(_)) {
            let next = self.rows[self.selected..]
                .iter()
                .position(|r| matches!(r, ConfigRow::Item(_)))
                .map(|p| self.selected + p);
            self.selected = next.unwrap_or(self.selected);
        }
    }

    pub fn item_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r, ConfigRow::Item(_)))
            .count()
    }

    pub fn selected_asset(&self) -> Option<&Asset> {
        match self.rows.get(self.selected)? {
            ConfigRow::Item(i) => self.catalog.assets.get(*i),
            ConfigRow::Header(_) => None,
        }
    }

    /// The kind under the cursor (an item's kind, or a header's).
    pub fn selected_kind(&self) -> Option<Kind> {
        match self.rows.get(self.selected)? {
            ConfigRow::Item(i) => self.catalog.assets.get(*i).map(|a| a.kind),
            ConfigRow::Header(k) => Some(*k),
        }
    }

    /// Moves over items, skipping headers, without wrapping.
    pub fn step(&mut self, delta: isize, registry: &Registry) {
        if self.rows.is_empty() {
            return;
        }
        let mut i = self.selected as isize;
        let len = self.rows.len() as isize;
        let dir = delta.signum();
        let mut remaining = delta.abs();
        while remaining > 0 {
            let mut j = i + dir;
            while j >= 0 && j < len && matches!(self.rows[j as usize], ConfigRow::Header(_)) {
                j += dir;
            }
            if j < 0 || j >= len {
                break;
            }
            i = j;
            remaining -= 1;
        }
        if i as usize != self.selected {
            self.selected = i as usize;
            self.scroll_offset = 0;
            self.rebuild_detail(registry);
        }
    }

    pub fn max_scroll(&self) -> usize {
        self.detail_lines
            .len()
            .saturating_sub(self.viewport_rows.get().max(1))
    }

    pub fn scroll(&mut self, delta: isize) {
        self.scroll_offset = if delta < 0 {
            self.scroll_offset.saturating_sub(delta.unsigned_abs())
        } else {
            self.scroll_offset
                .saturating_add(delta as usize)
                .min(self.max_scroll())
        };
    }

    /// Workspaces whose copy of the selected loop skill or agent differs
    /// or is missing.
    pub fn stale_copies(&self) -> usize {
        self.copies.iter().filter(|c| !c.same).count()
    }

    pub fn rebuild_detail(&mut self, registry: &Registry) {
        let dim = Style::default().fg(Color::DarkGray);
        let key = Style::default().fg(Color::Yellow);
        let red = Style::default().fg(Color::Red);
        let green = Style::default().fg(Color::Green);
        let Some(asset) = self.selected_asset().cloned() else {
            self.detail_lines = vec![Line::styled("  nothing selected", dim)];
            self.copies.clear();
            return;
        };
        self.copies = match asset.kind {
            Kind::LoopSkill | Kind::LoopAgent => self.catalog.workspace_copies(&asset, registry),
            _ => Vec::new(),
        };
        let row = |k: &str, v: String, style: Style| {
            Line::from(vec![
                Span::styled(format!("  {k:<11}"), key),
                Span::styled(v, style),
            ])
        };
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(row(
            "Kind",
            asset.kind.label().to_string(),
            Style::default(),
        ));
        let source = match asset.source {
            Source::Builtin => "built-in (compiled into agent-mux)".to_string(),
            Source::Override => format!("override  {}", asset.path.display()),
            Source::User => format!("user file  {}", asset.path.display()),
        };
        lines.push(row("Source", source, Style::default()));
        if asset.source == Source::Builtin {
            lines.push(row("Edit path", asset.path.display().to_string(), dim));
        }
        if let Some(repo) = asset.repo_path {
            lines.push(row("Built-in", repo.to_string(), dim));
        }
        if asset.problems.is_empty() {
            lines.push(row("Status", "valid".into(), green));
        } else {
            lines.push(row(
                "Status",
                format!("{} problem(s)", asset.problems.len()),
                red,
            ));
            for p in &asset.problems {
                lines.push(Line::styled(format!("             {p}"), red));
            }
        }
        match asset.kind {
            Kind::LoopSkill => {
                let pats = self.catalog.patterns_using(&asset.name);
                let used = if pats.is_empty() {
                    "no pattern lists it".to_string()
                } else {
                    format!("patterns {}", pats.join(", "))
                };
                lines.push(row("Used by", used, Style::default()));
            }
            Kind::LoopAgent => {
                lines.push(row(
                    "Used by",
                    if asset.name == "loop-verifier" {
                        "every pattern with verifier = true".to_string()
                    } else {
                        "installed into every registered loop workspace".to_string()
                    },
                    Style::default(),
                ));
            }
            Kind::Prompts => {
                lines.push(row(
                    "Used by",
                    "every loop run ([loop] run) and hydrated skill launch ([skill] hydration_hint)".into(),
                    Style::default(),
                ));
            }
            Kind::LoopPattern => {
                let (pats, err) = crate::loops::patterns::load(&self.root);
                lines.push(row(
                    "Patterns",
                    format!(
                        "{} ({})",
                        pats.len(),
                        pats.iter()
                            .map(|p| p.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Style::default(),
                ));
                if let Some(e) = err {
                    lines.push(row("Library", e, red));
                }
            }
            Kind::Skill => {
                lines.push(row(
                    "Package",
                    format!(
                        "{}  (installed per harness from the Agents sidebar)",
                        asset.name
                    ),
                    Style::default(),
                ));
            }
            Kind::LoopTemplate => {
                lines.push(row(
                    "Used by",
                    "the scaffolder, when the file is missing from a workspace".into(),
                    Style::default(),
                ));
            }
            Kind::Settings => {
                lines.push(row(
                    "Reload",
                    "profiles, agents, loops and editor apply on save; tracing needs a restart"
                        .into(),
                    dim,
                ));
            }
        }
        if !self.copies.is_empty() {
            let same = self.copies.iter().filter(|c| c.same).count();
            let stale = self.copies.len() - same;
            let summary = if stale == 0 {
                format!("{} workspace copy(ies), all current", self.copies.len())
            } else {
                format!(
                    "{} workspace copy(ies), {stale} differ or missing (u pushes)",
                    self.copies.len()
                )
            };
            lines.push(row(
                "Workspaces",
                summary,
                if stale == 0 {
                    green
                } else {
                    Style::default().fg(Color::Yellow)
                },
            ));
            for c in &self.copies {
                let state = if !c.present {
                    "missing"
                } else if c.same {
                    "current"
                } else {
                    "differs"
                };
                lines.push(Line::styled(
                    format!("             {:<8} {}", state, c.path.display()),
                    if c.same {
                        dim
                    } else {
                        Style::default().fg(Color::Yellow)
                    },
                ));
            }
        }
        let placeholders = asset.placeholders();
        if !placeholders.is_empty() {
            lines.push(row("Fills", placeholders.join(" "), dim));
        }
        lines.push(Line::styled(
            "  ────────────────────────────────────────────────────────────",
            dim,
        ));
        let text = asset.effective();
        if text.trim().is_empty() {
            lines.push(Line::styled(
                if asset.kind == Kind::Settings {
                    "  no settings file yet; Enter creates one from the example"
                } else {
                    "  (empty)"
                },
                dim,
            ));
        } else {
            for l in text.lines() {
                lines.push(Line::raw(format!("  {l}")));
            }
        }
        self.detail_lines = lines;
        self.scroll_offset = self.scroll_offset.min(self.max_scroll());
    }

    /// The label of a row's source column and its colour.
    pub fn source_label(asset: &Asset) -> (String, Style) {
        let text = asset.source.label().to_string();
        let style = if !asset.valid() {
            Style::default().fg(Color::Red)
        } else {
            match asset.source {
                Source::Builtin => Style::default().fg(Color::DarkGray),
                Source::Override => Style::default().fg(Color::Cyan),
                Source::User => Style::default().fg(Color::Green),
            }
        };
        (text, style)
    }

    /// The footer line: the pending question, or the key hints.
    pub fn footer(&self) -> String {
        match &self.pending {
            Pending::Reset => match self.selected_asset() {
                Some(a) if a.source == Source::User => {
                    format!(" Delete {}? [y/n]", a.id)
                }
                Some(a) => format!(" Reset {} to the built-in text? [y/n]", a.id),
                None => " [y/n]".into(),
            },
            Pending::Push => {
                " Rewrite the loop skills and agents in every registered loop workspace? [y/n]"
                    .into()
            }
            Pending::NewName { kind, input } => {
                format!(" New {} name: {input}_   [Enter] create  [Esc] cancel", kind.label())
            }
            Pending::None => {
                " [Enter/e] edit  [n] new  [R] reset  [u] push to workspaces  [r] rescan  [←/→] pane  [Esc] close"
                    .into()
            }
        }
    }
}

impl std::fmt::Debug for ConfigViewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigViewState")
            .field("root", &self.root)
            .field("rows", &self.rows.len())
            .field("selected", &self.selected)
            .field("focus", &self.focus)
            .field("pending", &self.pending)
            .finish()
    }
}
