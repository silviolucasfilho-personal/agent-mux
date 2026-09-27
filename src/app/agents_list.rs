//! The Agents list: everything runnable in one sidebar section, each
//! running session nested under the agent that started it (spec
//! `docs/superpowers/specs/2026-09-26-agent-first-design.md`, section 4).
//!
//! The list is a projection over the existing runtimes. Moving the cursor
//! onto a row selects the same thing the old sections selected (a
//! session, a loop, a workflow row, a skill), so every action behind a key
//! keeps working unchanged; only the rows without an older home (a
//! harness, a persona agent) are remembered here.

use super::workflows::WorkflowRow;
use super::{App, SidebarSection};
use crate::harness::Harness;

/// What a row of the Agents list is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentKind {
    /// A group heading; never selected.
    Header(&'static str),
    /// A harness profile: the plain agent (index into `profiles`).
    Harness(usize),
    /// A running or finished session (index into `sessions`).
    Session(usize),
    /// A scheduled agent (index into `loop_registry.loops`).
    Loop(usize),
    /// A flow: a workflow run, plan or document (index into `workflow_rows()`).
    Flow(usize),
    /// A skill package launched as an agent (index into `skills`).
    Skill(usize),
    /// A persona agent from the catalog, by name.
    Persona(String),
    /// A loop or workflow run no other row stands for (an older run whose
    /// sessions are still listed), by its group key (`wf:<run>`).
    Run(String),
}

impl AgentKind {
    pub fn selectable(&self) -> bool {
        !matches!(self, AgentKind::Header(_))
    }

    /// The sidebar section whose actions this row takes.
    pub fn section(&self) -> SidebarSection {
        match self {
            AgentKind::Loop(_) => SidebarSection::Loops,
            AgentKind::Flow(_) => SidebarSection::Workflows,
            AgentKind::Skill(_) | AgentKind::Persona(_) => SidebarSection::Agents,
            _ => SidebarSection::Active,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLine {
    pub kind: AgentKind,
    /// 1 for a session under its agent.
    pub depth: u8,
}

/// A harness or persona row under the cursor, remembered with the
/// session list it was chosen against: a new or removed session moves the
/// cursor to the session rows, where the older sections put it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFocus {
    pub kind: AgentKind,
    pub selected: usize,
    pub sessions: usize,
}

impl App {
    /// Which agent row a session belongs under.
    fn owner_of(&self, idx: usize, workflow_rows: &[WorkflowRow]) -> AgentKind {
        let s = &self.sessions[idx];
        if let Some(g) = &s.group {
            let known = match g.kind {
                crate::tree::GroupKind::Loop => self
                    .loop_registry
                    .loops
                    .iter()
                    .position(|l| l.id == g.id)
                    .map(AgentKind::Loop),
                crate::tree::GroupKind::Workflow => workflow_rows
                    .iter()
                    .position(|r| {
                        matches!(r, WorkflowRow::Live(id) | WorkflowRow::Recent(id) if *id == g.id)
                    })
                    .map(AgentKind::Flow),
            };
            // a run no row stands for still keeps its sessions together
            return known.unwrap_or_else(|| AgentKind::Run(g.key()));
        }
        if let Some(skill) = &s.skill_id
            && let Some(i) = self.skills.iter().position(|k| k.id == *skill)
        {
            return AgentKind::Skill(i);
        }
        let by_name = self.profiles.iter().position(|p| p.name == s.profile.name);
        let by_harness = Harness::detect(&s.profile.command).and_then(|h| {
            self.profiles
                .iter()
                .position(|p| Harness::detect(&p.command) == Some(h))
        });
        AgentKind::Harness(by_name.or(by_harness).unwrap_or(0))
    }

    /// Every row of the Agents list, in order: harnesses, scheduled
    /// agents, flows, skills, personas; sessions under their owner.
    pub fn agent_lines(&self) -> Vec<AgentLine> {
        let rows = self.workflow_rows();
        let owners: Vec<AgentKind> = (0..self.sessions.len())
            .map(|i| self.owner_of(i, &rows))
            .collect();
        let mut out = Vec::new();
        let push = |out: &mut Vec<AgentLine>, kind: AgentKind| {
            let children: Vec<usize> = owners
                .iter()
                .enumerate()
                .filter(|(_, o)| **o == kind)
                .map(|(i, _)| i)
                .collect();
            out.push(AgentLine { kind, depth: 0 });
            for i in children {
                out.push(AgentLine {
                    kind: AgentKind::Session(i),
                    depth: 1,
                });
            }
        };
        for p in 0..self.profiles.len() {
            push(&mut out, AgentKind::Harness(p));
        }
        if !self.loop_registry.loops.is_empty() {
            out.push(AgentLine {
                kind: AgentKind::Header("scheduled"),
                depth: 0,
            });
            for i in 0..self.loop_registry.loops.len() {
                push(&mut out, AgentKind::Loop(i));
            }
        }
        let mut runs: Vec<AgentKind> = Vec::new();
        for o in &owners {
            if matches!(o, AgentKind::Run(_)) && !runs.contains(o) {
                runs.push(o.clone());
            }
        }
        if !rows.is_empty() || !runs.is_empty() {
            out.push(AgentLine {
                kind: AgentKind::Header("flows"),
                depth: 0,
            });
            for r in runs {
                push(&mut out, r);
            }
            for i in 0..rows.len() {
                push(&mut out, AgentKind::Flow(i));
            }
        }
        if !self.skills.is_empty() {
            out.push(AgentLine {
                kind: AgentKind::Header("skills"),
                depth: 0,
            });
            for i in 0..self.skills.len() {
                push(&mut out, AgentKind::Skill(i));
            }
        }
        if !self.personas.is_empty() {
            out.push(AgentLine {
                kind: AgentKind::Header("personas"),
                depth: 0,
            });
            for name in &self.personas {
                out.push(AgentLine {
                    kind: AgentKind::Persona(name.clone()),
                    depth: 0,
                });
            }
        }
        out
    }

    /// The harness or persona row under the cursor, while it still holds.
    pub fn agent_focus(&self) -> Option<&AgentKind> {
        self.agent_focus
            .as_ref()
            .filter(|f| f.selected == self.selected && f.sessions == self.sessions.len())
            .map(|f| &f.kind)
    }

    /// The index of the cursor in `lines`, read from the selection the
    /// sections keep.
    pub fn agent_cursor(&self, lines: &[AgentLine]) -> Option<usize> {
        let find = |k: &AgentKind| lines.iter().position(|l| l.kind == *k);
        if let Some(k) = self.agent_focus()
            && matches!(k.section(), s if s == self.sidebar_section)
            && let Some(i) = find(k)
        {
            return Some(i);
        }
        match self.sidebar_section {
            SidebarSection::History => None,
            SidebarSection::Active => {
                if self.sessions.is_empty() {
                    lines.iter().position(|l| l.kind.selectable())
                } else {
                    find(&AgentKind::Session(self.selected))
                }
            }
            SidebarSection::Loops => find(&AgentKind::Loop(self.selected_loop)),
            SidebarSection::Workflows => find(&AgentKind::Flow(self.selected_workflow)),
            SidebarSection::Agents => find(&AgentKind::Skill(self.selected_agent)),
        }
        .or_else(|| lines.iter().position(|l| l.kind.selectable()))
    }

    /// The row under the cursor.
    pub fn agent_row(&self) -> Option<AgentKind> {
        let lines = self.agent_lines();
        self.agent_cursor(&lines).map(|i| lines[i].kind.clone())
    }

    /// Puts the cursor on row `i` and selects what it stands for.
    pub fn select_agent_line(&mut self, lines: &[AgentLine], i: usize) {
        let Some(line) = lines.get(i) else { return };
        self.agent_focus = None;
        match &line.kind {
            AgentKind::Header(_) => {}
            AgentKind::Session(s) => {
                self.sidebar_section = SidebarSection::Active;
                if *s != self.selected {
                    self.selection = None;
                    self.selected = *s;
                }
            }
            AgentKind::Loop(l) => {
                self.sidebar_section = SidebarSection::Loops;
                self.selected_loop = *l;
            }
            AgentKind::Flow(w) => {
                self.sidebar_section = SidebarSection::Workflows;
                self.select_workflow_row(*w);
            }
            AgentKind::Skill(k) => {
                self.sidebar_section = SidebarSection::Agents;
                self.selected_agent = *k;
            }
            kind @ (AgentKind::Harness(_) | AgentKind::Persona(_) | AgentKind::Run(_)) => {
                self.sidebar_section = kind.section();
                self.agent_focus = Some(AgentFocus {
                    kind: kind.clone(),
                    selected: self.selected,
                    sessions: self.sessions.len(),
                });
            }
        }
    }

    /// Moves the cursor by one selectable row; `false` at either end.
    pub fn move_agent_cursor(&mut self, delta: isize) -> bool {
        let lines = self.agent_lines();
        let Some(cur) = self.agent_cursor(&lines) else {
            if let Some(first) = lines.iter().position(|l| l.kind.selectable()) {
                self.select_agent_line(&lines, first);
                return true;
            }
            return false;
        };
        let mut i = cur as isize;
        loop {
            i += delta;
            if i < 0 || i >= lines.len() as isize {
                return false;
            }
            if lines[i as usize].kind.selectable() {
                self.select_agent_line(&lines, i as usize);
                return true;
            }
        }
    }

    /// The first row of `section`'s kind, selected (a helper for code and
    /// tests that used to Tab to a section).
    pub fn select_first_row_of(&mut self, section: SidebarSection) {
        if section == SidebarSection::History {
            self.sidebar_section = SidebarSection::History;
            return;
        }
        let lines = self.agent_lines();
        let at = lines.iter().position(|l| {
            l.kind.selectable()
                && l.kind.section() == section
                && !matches!(l.kind, AgentKind::Persona(_))
                && (section != SidebarSection::Active
                    || matches!(l.kind, AgentKind::Session(_))
                    || self.sessions.is_empty())
        });
        match at {
            Some(i) => self.select_agent_line(&lines, i),
            None => self.sidebar_section = section,
        }
    }

    /// Re-reads the persona agents the list shows.
    pub fn reload_personas(&mut self) {
        let catalog = crate::agents::Catalog::load(&self.library_root(), None);
        self.personas = catalog
            .entries
            .iter()
            .filter(|e| e.spec.is_some())
            .map(|e| e.name.clone())
            .collect();
    }
}
