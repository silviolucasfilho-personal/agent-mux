//! The loop builder (`o` / `f` in the Loops section): every loop pattern,
//! built-in or the user's, edited field by field in the same look as the
//! flow builder. The left pane lists the patterns with where each comes
//! from; the right pane draws the selected one's cycle (schedule → triage
//! → fix → checkers → state file) and its fields in words. Saving writes
//! the pattern into the library registry by id (`loops::builder::save`),
//! so an edited built-in is the user's copy and `R` restores it.

use super::text_area::TextArea;
use super::{App, Mode, Notice};
use crate::loops::builder::{self, Item, Origin};
use crate::loops::{Level, Pattern};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Fields,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LField {
    Id,
    Name,
    Goal,
    Every,
    Level,
    Skills,
    Checkers,
    Verifier,
    Breaker,
    Gates,
    Prompt,
    Model,
    CheckerModel,
    RunsPerDay,
    TokensPerDay,
    Priority,
    Risk,
    TokenCost,
    StateFile,
    EarlyExit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edits {
    Text,
    Cycle,
    Pick,
}

impl LField {
    pub const ALL: [LField; 20] = [
        LField::Id,
        LField::Name,
        LField::Goal,
        LField::Every,
        LField::Level,
        LField::Skills,
        LField::Checkers,
        LField::Prompt,
        LField::Verifier,
        LField::Breaker,
        LField::Gates,
        LField::Model,
        LField::CheckerModel,
        LField::RunsPerDay,
        LField::TokensPerDay,
        LField::Priority,
        LField::Risk,
        LField::TokenCost,
        LField::StateFile,
        LField::EarlyExit,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LField::Id => "Id",
            LField::Name => "Name",
            LField::Goal => "Goal",
            LField::Every => "Every",
            LField::Level => "Starts at",
            LField::Skills => "Skills",
            LField::Checkers => "Checkers",
            LField::Verifier => "Verifier",
            LField::Breaker => "Breaker",
            LField::Gates => "Human gates",
            LField::Prompt => "Prompt",
            LField::Model => "Model",
            LField::CheckerModel => "Checker model",
            LField::RunsPerDay => "Runs a day",
            LField::TokensPerDay => "Tokens a day",
            LField::Priority => "Priority",
            LField::Risk => "Risk",
            LField::TokenCost => "Token cost",
            LField::StateFile => "State file",
            LField::EarlyExit => "Early exit",
        }
    }

    pub fn edits(self) -> Edits {
        match self {
            LField::Level
            | LField::Verifier
            | LField::Breaker
            | LField::Risk
            | LField::TokenCost
            | LField::StateFile
            | LField::EarlyExit => Edits::Cycle,
            LField::Skills | LField::Checkers | LField::Prompt => Edits::Pick,
            _ => Edits::Text,
        }
    }

    /// The group a field is drawn under.
    pub fn group(self) -> &'static str {
        match self {
            LField::Id | LField::Name | LField::Goal => "What it is",
            LField::Every | LField::Level | LField::Skills | LField::Checkers | LField::Prompt => {
                "How it runs"
            }
            LField::Verifier | LField::Breaker | LField::Gates => "Safety",
            LField::Model | LField::CheckerModel => "Models",
            _ => "Budget",
        }
    }
}

const RISKS: [&str; 3] = ["low", "medium", "high"];
const COSTS: [&str; 4] = ["low", "medium", "high", "very-high"];

#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    /// Loop skills or loop agents, ticked in order.
    Multi {
        skills: bool,
        options: Vec<(String, String)>,
        chosen: Vec<String>,
        selected: usize,
    },
    Prompt {
        text: TextArea,
    },
    NewName {
        text: TextArea,
        /// Copy this pattern instead of starting blank.
        from: Option<usize>,
    },
    Confirm {
        question: String,
        action: Confirm,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Restore,
    Delete,
    Discard,
}

#[derive(Debug)]
pub struct LoopBuilderState {
    pub items: Vec<Item>,
    /// The id each item had when loaded (`None` for a new one), so a
    /// rename replaces rather than duplicates.
    pub loaded_ids: Vec<Option<String>>,
    pub selected: usize,
    pub focus: Focus,
    pub field: usize,
    pub edit: Option<(LField, TextArea)>,
    pub overlay: Option<Overlay>,
    /// Loop skills and loop agents: name and one-line description.
    pub skills: Vec<(String, String)>,
    pub agents: Vec<(String, String)>,
    /// Registered loops per pattern id.
    pub in_use: Vec<(String, usize)>,
    pub error: Option<String>,
}

impl LoopBuilderState {
    pub fn current(&self) -> Option<&Item> {
        self.items.get(self.selected)
    }

    fn current_mut(&mut self) -> Option<&mut Pattern> {
        self.items.get_mut(self.selected).map(|i| &mut i.pattern)
    }

    pub fn current_field(&self) -> Option<LField> {
        LField::ALL.get(self.field).copied()
    }

    pub fn uses(&self, id: &str) -> usize {
        self.in_use
            .iter()
            .find(|(p, _)| p == id)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    }

    pub fn dirty(&self) -> bool {
        self.items.iter().any(Item::dirty)
    }

    /// What the selected pattern's checks say.
    pub fn problems(&self) -> Vec<String> {
        let Some(it) = self.current() else {
            return Vec::new();
        };
        let others: Vec<&str> = self
            .items
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.selected)
            .map(|(_, x)| x.pattern.id.as_str())
            .collect();
        let skills: Vec<String> = self.skills.iter().map(|s| s.0.clone()).collect();
        let agents: Vec<String> = self.agents.iter().map(|s| s.0.clone()).collect();
        builder::check(&it.pattern, &others, &skills, &agents)
    }

    pub fn value(&self, f: LField) -> String {
        let Some(it) = self.current() else {
            return String::new();
        };
        let p = &it.pattern;
        let or =
            |o: &Option<String>, d: &str| o.clone().filter(|x| !x.is_empty()).unwrap_or(d.into());
        match f {
            LField::Id => p.id.clone(),
            LField::Name => p.name.clone(),
            LField::Goal => p.goal.clone(),
            LField::Every => builder::format_interval(p.default_interval_s),
            LField::Level => match p.week_one_level {
                Level::L1 => "L1 · reports only".into(),
                Level::L2 => "L2 · proposes one fix".into(),
                Level::L3 => "L3 · fixes and verifies".into(),
            },
            LField::Skills => p.skills.join(" → "),
            LField::Checkers => {
                let a = p.effective_agents();
                if a.is_empty() {
                    "none".into()
                } else {
                    a.join(", ")
                }
            }
            LField::Verifier => yes(p.verifier),
            LField::Breaker => yes(p.breaker),
            LField::Gates => {
                if p.human_gates.is_empty() {
                    "none".into()
                } else {
                    p.human_gates.join(", ")
                }
            }
            LField::Prompt => match &p.prompt {
                Some(t) => t.split_whitespace().take(14).collect::<Vec<_>>().join(" ") + " …",
                None => "the default loop prompt (prompts.toml [loop] run)".into(),
            },
            LField::Model => or(&p.model, "the profile's"),
            LField::CheckerModel => or(&p.verifier_model, "the run's (inherit)"),
            LField::RunsPerDay => p.max_runs_per_day.to_string(),
            LField::TokensPerDay => format!("{}k", p.max_tokens_per_day / 1000),
            LField::Priority => p.priority.to_string(),
            LField::Risk => p.risk.clone(),
            LField::TokenCost => p.token_cost.clone(),
            LField::StateFile => p.state_file.clone(),
            LField::EarlyExit => yes(p.cost.early_exit_required),
        }
    }

    fn edit_text(&self, f: LField) -> String {
        let Some(it) = self.current() else {
            return String::new();
        };
        let p = &it.pattern;
        match f {
            LField::Gates => p.human_gates.join(", "),
            LField::Model => p.model.clone().unwrap_or_default(),
            LField::CheckerModel => p.verifier_model.clone().unwrap_or_default(),
            LField::TokensPerDay => p.max_tokens_per_day.to_string(),
            f => self.value(f),
        }
    }
}

fn yes(b: bool) -> String {
    if b { "yes" } else { "no" }.into()
}

fn cycle_str(all: &[&str], cur: &str, delta: isize) -> String {
    let i = all.iter().position(|x| *x == cur).unwrap_or(0) as isize;
    all[(i + delta).rem_euclid(all.len() as isize) as usize].to_string()
}

/// The `description:` of a skill or agent file.
fn description_of(text: &str) -> String {
    crate::tracing::inventory::parse_frontmatter(text)
        .and_then(|f| {
            f.into_iter()
                .find(|(k, _)| k == "description")
                .map(|(_, v)| v)
        })
        .unwrap_or_default()
}

enum After {
    Stay,
    Close,
}

impl App {
    fn loop_builder_state(&mut self, select: Option<&str>) -> Option<LoopBuilderState> {
        let root = self.library_root();
        let (items, error) = builder::load(&root);
        let skills: Vec<(String, String)> = crate::assets::loop_skill_names(&root)
            .into_iter()
            .map(|n| {
                let text = crate::assets::loop_skill(&root, &n).unwrap_or_default();
                let d = description_of(&text);
                (n, d)
            })
            .collect();
        let agents: Vec<(String, String)> = crate::assets::loop_agents(&root)
            .into_iter()
            .map(|(n, t)| {
                let d = description_of(&t);
                (n, d)
            })
            .collect();
        let mut in_use: Vec<(String, usize)> = Vec::new();
        for l in &self.loop_registry.loops {
            match in_use.iter_mut().find(|(p, _)| *p == l.pattern) {
                Some(x) => x.1 += 1,
                None => in_use.push((l.pattern.clone(), 1)),
            }
        }
        let selected = select
            .and_then(|id| items.iter().position(|i| i.pattern.id == id))
            .unwrap_or(0);
        Some(LoopBuilderState {
            loaded_ids: items.iter().map(|i| Some(i.pattern.id.clone())).collect(),
            items,
            selected,
            focus: Focus::List,
            field: 0,
            edit: None,
            overlay: None,
            skills,
            agents,
            in_use,
            error,
        })
    }

    /// `o` in the Loops section: the selected loop's pattern in the builder.
    pub fn open_loop_builder_selected(&mut self) {
        let pattern = self.selected_loop().map(|e| e.pattern.clone());
        if let Some(st) = self.loop_builder_state(pattern.as_deref()) {
            self.mode = Mode::LoopBuilder(Box::new(st));
        }
    }

    /// `f` in the Loops section: the builder with a new pattern started.
    pub fn open_loop_builder_new(&mut self) {
        if let Some(mut st) = self.loop_builder_state(None) {
            st.selected = st.items.len();
            st.overlay = Some(Overlay::NewName {
                text: TextArea::new("my-loop"),
                from: None,
            });
            self.mode = Mode::LoopBuilder(Box::new(st));
        }
    }

    pub fn handle_loop_builder_key(&mut self, key: &KeyEvent) {
        let Mode::LoopBuilder(mut st) = std::mem::replace(&mut self.mode, Mode::Control) else {
            return;
        };
        match self.loop_key(&mut st, key) {
            After::Stay => self.mode = Mode::LoopBuilder(st),
            After::Close => {
                crate::loops::patterns::reload();
                self.loop_audits.clear();
                self.mode = Mode::Control;
            }
        }
    }

    /// After `Ctrl+O` on the prompt: the text returns to it.
    pub fn loop_builder_editor_finished(&mut self, path: &std::path::Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        if let Mode::LoopBuilder(st) = &mut self.mode
            && let Some(Overlay::Prompt { text: t }) = &mut st.overlay
        {
            t.set(text.trim_end_matches('\n'));
        }
    }

    fn loop_key(&mut self, st: &mut LoopBuilderState, key: &KeyEvent) -> After {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if st.overlay.is_some() {
            return self.loop_overlay_key(st, key);
        }
        if let Some((f, t)) = st.edit.as_mut() {
            match key.code {
                KeyCode::Esc => st.edit = None,
                KeyCode::Enter => {
                    let (f, text) = (*f, t.text.trim().to_string());
                    match self.loop_commit(st, f, &text) {
                        Ok(()) => st.edit = None,
                        Err(e) => self.notice = Some(Notice::warn(e)),
                    }
                }
                _ => super::flow_builder::text_key(t, key, ctrl),
            }
            return After::Stay;
        }
        // wherever the focus is
        match key.code {
            KeyCode::Esc if st.focus == Focus::Fields => {
                st.focus = Focus::List;
                return After::Stay;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                if st.dirty() {
                    st.overlay = Some(Overlay::Confirm {
                        question: "Leave the loop builder? Unsaved edits are lost. [y/n]".into(),
                        action: Confirm::Discard,
                    });
                    return After::Stay;
                }
                return After::Close;
            }
            KeyCode::Char('s') => {
                self.loop_save(st);
                return After::Stay;
            }
            KeyCode::Char('n') => {
                st.overlay = Some(Overlay::NewName {
                    text: TextArea::new("my-loop"),
                    from: None,
                });
                return After::Stay;
            }
            KeyCode::Char('c') if st.current().is_some() => {
                let base = st
                    .current()
                    .map(|i| format!("{}-copy", i.pattern.id))
                    .unwrap_or_default();
                st.overlay = Some(Overlay::NewName {
                    text: TextArea::new(base),
                    from: Some(st.selected),
                });
                return After::Stay;
            }
            KeyCode::Char('R') => {
                match st.current().map(|i| i.origin) {
                    Some(Origin::Edited) => {
                        st.overlay = Some(Overlay::Confirm {
                            question: format!(
                                "Restore the built-in {}? Your edits to it are removed. [y/n]",
                                st.current()
                                    .map(|i| i.pattern.id.clone())
                                    .unwrap_or_default()
                            ),
                            action: Confirm::Restore,
                        })
                    }
                    Some(Origin::Builtin) if st.current().is_some_and(Item::dirty) => {
                        // unsaved edits to an untouched built-in: just drop them
                        if let Some(it) = st.items.get_mut(st.selected)
                            && let Some(s) = it.saved.clone()
                        {
                            it.pattern = s;
                        }
                    }
                    _ => self.notice = Some(Notice::info("R restores an edited built-in pattern")),
                }
                return After::Stay;
            }
            KeyCode::Char('d') if st.focus == Focus::List => {
                match st.current().map(|i| i.origin) {
                    Some(Origin::Yours) => {
                        let id = st
                            .current()
                            .map(|i| i.pattern.id.clone())
                            .unwrap_or_default();
                        let uses = st.uses(&id);
                        st.overlay = Some(Overlay::Confirm {
                            question: if uses > 0 {
                                format!("Delete {id}? {uses} registered loop(s) use it. [y/n]")
                            } else {
                                format!("Delete {id}? [y/n]")
                            },
                            action: Confirm::Delete,
                        });
                    }
                    Some(_) => {
                        self.notice = Some(Notice::info(
                            "a built-in pattern cannot be deleted; R restores an edited one",
                        ))
                    }
                    None => {}
                }
                return After::Stay;
            }
            _ => {}
        }
        match st.focus {
            Focus::List => match key.code {
                KeyCode::Up | KeyCode::Char('k') => st.selected = st.selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    st.selected = (st.selected + 1).min(st.items.len())
                }
                KeyCode::Home | KeyCode::Char('g') => st.selected = 0,
                KeyCode::End | KeyCode::Char('G') => st.selected = st.items.len(),
                KeyCode::Enter
                | KeyCode::Right
                | KeyCode::Char('l')
                | KeyCode::Tab
                | KeyCode::Char('e') => {
                    if st.selected == st.items.len() {
                        st.overlay = Some(Overlay::NewName {
                            text: TextArea::new("my-loop"),
                            from: None,
                        });
                    } else {
                        st.focus = Focus::Fields;
                    }
                }
                _ => {}
            },
            Focus::Fields => {
                let n = LField::ALL.len();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => st.field = st.field.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => st.field = (st.field + 1).min(n - 1),
                    KeyCode::Tab => st.field = (st.field + 1) % n,
                    KeyCode::BackTab => st.field = (st.field + n - 1) % n,
                    KeyCode::Home | KeyCode::Char('g') => st.field = 0,
                    KeyCode::End | KeyCode::Char('G') => st.field = n - 1,
                    KeyCode::Left | KeyCode::Char('h')
                        if st.current_field().map(LField::edits) != Some(Edits::Cycle) =>
                    {
                        st.focus = Focus::List
                    }
                    KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                        if let Some(f) = st.current_field() {
                            let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                            loop_cycle(st, f, delta);
                        }
                    }
                    KeyCode::Enter | KeyCode::Char('e') => {
                        if let Some(f) = st.current_field() {
                            self.loop_activate(st, f);
                        }
                    }
                    _ => {}
                }
            }
        }
        After::Stay
    }

    fn loop_activate(&mut self, st: &mut LoopBuilderState, f: LField) {
        let Some(p) = st.current().map(|i| i.pattern.clone()) else {
            return;
        };
        match f.edits() {
            Edits::Cycle => loop_cycle(st, f, 1),
            Edits::Text => st.edit = Some((f, TextArea::new(st.edit_text(f)))),
            Edits::Pick => match f {
                LField::Prompt => {
                    let cur = p
                        .prompt
                        .clone()
                        .unwrap_or_else(|| crate::prompts::Prompts::current().loop_run.clone());
                    st.overlay = Some(Overlay::Prompt {
                        text: TextArea::new(cur),
                    });
                }
                LField::Skills | LField::Checkers => {
                    let skills = f == LField::Skills;
                    let all = if skills { &st.skills } else { &st.agents };
                    let chosen: Vec<String> = if skills {
                        p.skills.clone()
                    } else {
                        p.effective_agents()
                    };
                    // the chosen ones first, in their order, then the rest
                    let mut options: Vec<(String, String)> = chosen
                        .iter()
                        .filter_map(|c| all.iter().find(|a| a.0 == *c).cloned())
                        .collect();
                    options.extend(all.iter().filter(|a| !chosen.contains(&a.0)).cloned());
                    st.overlay = Some(Overlay::Multi {
                        skills,
                        options,
                        chosen,
                        selected: 0,
                    });
                }
                _ => {}
            },
        }
    }

    fn loop_commit(
        &mut self,
        st: &mut LoopBuilderState,
        f: LField,
        text: &str,
    ) -> Result<(), String> {
        let num = |t: &str| -> Result<u64, String> {
            let t = t.trim().replace(['_', ','], "");
            t.strip_suffix('k')
                .map(|k| k.parse::<u64>().map(|v| v * 1000))
                .unwrap_or_else(|| t.parse::<u64>())
                .map_err(|_| format!("{t:?} is not a number"))
        };
        let Some(p) = st.current_mut() else {
            return Ok(());
        };
        match f {
            LField::Id => {
                if !crate::workflows::builder::is_id(text) {
                    return Err(format!("{text:?}: an id must match ^[a-z][a-z0-9_-]*$"));
                }
                p.id = text.to_string();
            }
            LField::Name => p.name = text.to_string(),
            LField::Goal => p.goal = text.to_string(),
            LField::Every => p.default_interval_s = builder::parse_interval(text)?,
            LField::Gates => {
                p.human_gates = text
                    .split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(str::to_string)
                    .collect()
            }
            LField::Model => p.model = Some(text.to_string()).filter(|x| !x.is_empty()),
            LField::CheckerModel => {
                p.verifier_model = Some(text.to_string()).filter(|x| !x.is_empty())
            }
            LField::RunsPerDay => p.max_runs_per_day = num(text)? as u32,
            LField::TokensPerDay => p.max_tokens_per_day = num(text)?,
            LField::Priority => p.priority = num(text)?.min(255) as u8,
            _ => {}
        }
        Ok(())
    }

    fn loop_overlay_key(&mut self, st: &mut LoopBuilderState, key: &KeyEvent) -> After {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
        let Some(ov) = st.overlay.as_mut() else {
            return After::Stay;
        };
        match ov {
            Overlay::Confirm { action, .. } => {
                let action = *action;
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        st.overlay = None;
                        match action {
                            Confirm::Discard => return After::Close,
                            Confirm::Restore | Confirm::Delete => {
                                let id = st
                                    .current()
                                    .map(|i| i.pattern.id.clone())
                                    .unwrap_or_default();
                                match builder::remove(&self.library_root(), &id) {
                                    Ok(()) => {
                                        self.loop_reload(st, Some(&id));
                                        self.notice =
                                            Some(Notice::info(if action == Confirm::Restore {
                                                format!("{id} is the built-in again")
                                            } else {
                                                format!("{id} deleted")
                                            }));
                                    }
                                    Err(e) => self.notice = Some(Notice::error(e)),
                                }
                            }
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => st.overlay = None,
                    _ => {}
                }
            }
            Overlay::NewName { text, from } => match key.code {
                KeyCode::Esc => {
                    st.overlay = None;
                    st.selected = st.selected.min(st.items.len().saturating_sub(1));
                }
                KeyCode::Enter => {
                    let id = text.text.trim().to_string();
                    if !crate::workflows::builder::is_id(&id) {
                        self.notice = Some(Notice::warn(format!(
                            "{id:?}: an id must match ^[a-z][a-z0-9_-]*$"
                        )));
                        return After::Stay;
                    }
                    if st.items.iter().any(|i| i.pattern.id == id) {
                        self.notice =
                            Some(Notice::warn(format!("a pattern is already called {id}")));
                        return After::Stay;
                    }
                    let mut p = match from.and_then(|i| st.items.get(i)) {
                        Some(src) => src.pattern.clone(),
                        None => builder::blank(&id),
                    };
                    p.id = id.clone();
                    if from.is_some() {
                        p.name = format!("{} (copy)", p.name);
                    }
                    st.items.push(Item {
                        pattern: p,
                        origin: Origin::Yours,
                        saved: None,
                    });
                    st.loaded_ids.push(None);
                    st.selected = st.items.len() - 1;
                    st.overlay = None;
                    st.focus = Focus::Fields;
                    st.field = LField::ALL
                        .iter()
                        .position(|f| *f == LField::Goal)
                        .unwrap_or(0);
                }
                _ => super::flow_builder::text_key(text, key, ctrl),
            },
            Overlay::Prompt { text } => match key.code {
                KeyCode::Esc => st.overlay = None,
                KeyCode::Char('o') if ctrl => {
                    let dir = self.workflows_runtime_dir().join("loops");
                    let path = dir.join("builder-prompt.md");
                    if std::fs::create_dir_all(&dir)
                        .and_then(|_| std::fs::write(&path, &text.text))
                        .is_ok()
                    {
                        self.editor_request = Some(super::EditorRequest {
                            path,
                            asset_id: "loop:prompt".into(),
                            command: crate::assets::editor_command(self.editor.as_deref()),
                        });
                    }
                }
                KeyCode::Char('r') if ctrl => {
                    // back to the default prompt
                    st.overlay = None;
                    if let Some(p) = st.current_mut() {
                        p.prompt = None;
                    }
                }
                KeyCode::Enter if !alt => {
                    let t = text.text.trim_end().to_string();
                    st.overlay = None;
                    let default = crate::prompts::Prompts::current().loop_run.clone();
                    if let Some(p) = st.current_mut() {
                        p.prompt = (!t.is_empty() && t != default.trim_end()).then_some(t);
                    }
                }
                _ => super::flow_builder::text_key(text, key, ctrl),
            },
            Overlay::Multi {
                skills,
                options,
                chosen,
                selected,
            } => match key.code {
                KeyCode::Esc => st.overlay = None,
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1).min(options.len().saturating_sub(1))
                }
                KeyCode::Char(' ') => {
                    if let Some((name, _)) = options.get(*selected) {
                        match chosen.iter().position(|c| c == name) {
                            Some(i) => {
                                chosen.remove(i);
                            }
                            None => chosen.push(name.clone()),
                        }
                    }
                }
                KeyCode::Char('K') => {
                    // move the ticked one earlier: the first skill is the triage
                    if let Some((name, _)) = options.get(*selected)
                        && let Some(i) = chosen.iter().position(|c| c == name)
                        && i > 0
                    {
                        chosen.swap(i, i - 1);
                    }
                }
                KeyCode::Char('J') => {
                    if let Some((name, _)) = options.get(*selected)
                        && let Some(i) = chosen.iter().position(|c| c == name)
                        && i + 1 < chosen.len()
                    {
                        chosen.swap(i, i + 1);
                    }
                }
                KeyCode::Enter => {
                    let (skills, chosen) = (*skills, chosen.clone());
                    st.overlay = None;
                    if let Some(p) = st.current_mut() {
                        if skills {
                            p.skills = chosen;
                        } else {
                            p.agents = chosen;
                        }
                    }
                }
                _ => {}
            },
        }
        After::Stay
    }

    fn loop_save(&mut self, st: &mut LoopBuilderState) {
        let Some(it) = st.current().cloned() else {
            return;
        };
        let problems = st.problems();
        if !problems.is_empty() {
            self.notice = Some(Notice::warn(format!(
                "{} problem(s) to fix first: {}",
                problems.len(),
                problems[0]
            )));
            return;
        }
        let old = st.loaded_ids.get(st.selected).cloned().flatten();
        match builder::save(&self.library_root(), &it.pattern, old.as_deref()) {
            Ok(path) => {
                let id = it.pattern.id.clone();
                self.loop_reload(st, Some(&id));
                crate::loops::patterns::reload();
                self.loop_audits.clear();
                let copy = if st.current().map(|i| i.origin) == Some(Origin::Edited) {
                    "; your copy replaces the built-in, R restores it"
                } else {
                    ""
                };
                self.notice = Some(Notice::info(format!(
                    "saved {id} in {}{copy}",
                    path.display()
                )));
            }
            Err(e) => self.notice = Some(Notice::error(e)),
        }
    }

    /// Re-reads the patterns, keeping other unsaved edits and the selection.
    fn loop_reload(&mut self, st: &mut LoopBuilderState, select: Option<&str>) {
        let (fresh, error) = builder::load(&self.library_root());
        let mut items = fresh;
        let mut loaded: Vec<Option<String>> =
            items.iter().map(|i| Some(i.pattern.id.clone())).collect();
        // unsaved edits elsewhere survive a save of one pattern
        for (old, old_id) in st.items.iter().zip(&st.loaded_ids) {
            if !old.dirty()
                || old_id.as_deref() == select
                || Some(old.pattern.id.as_str()) == select
            {
                continue;
            }
            match items
                .iter_mut()
                .find(|i| Some(i.pattern.id.as_str()) == old_id.as_deref())
            {
                Some(slot) => slot.pattern = old.pattern.clone(),
                None => {
                    items.push(old.clone());
                    loaded.push(old_id.clone());
                }
            }
        }
        st.items = items;
        st.loaded_ids = loaded;
        st.error = error;
        st.selected = select
            .and_then(|id| st.items.iter().position(|i| i.pattern.id == id))
            .unwrap_or(0)
            .min(st.items.len().saturating_sub(1));
    }
}

fn loop_cycle(st: &mut LoopBuilderState, f: LField, delta: isize) {
    let Some(p) = st.current_mut() else { return };
    match f {
        LField::Level => {
            let all = [Level::L1, Level::L2, Level::L3];
            let i = all.iter().position(|l| *l == p.week_one_level).unwrap_or(0) as isize;
            p.week_one_level = all[(i + delta).rem_euclid(3) as usize];
        }
        LField::Verifier => p.verifier = !p.verifier,
        LField::Breaker => p.breaker = !p.breaker,
        LField::EarlyExit => p.cost.early_exit_required = !p.cost.early_exit_required,
        LField::Risk => p.risk = cycle_str(&RISKS, &p.risk, delta),
        LField::TokenCost => p.token_cost = cycle_str(&COSTS, &p.token_cost, delta),
        LField::StateFile => {
            p.state_file = cycle_str(&crate::loops::STATE_FILES, &p.state_file, delta)
        }
        _ => {}
    }
}
