//! The flow builder (`f` / `o` in the Workflows section): a workflow built
//! step by step. Three screens share one draft (`workflows::builder`):
//! **Steps** (the flow on the left, the selected step's fields on the
//! right), **What passes** (each step's answer shape and what the next one
//! reads), and **Review** (the flow in words, the checks, the document).
//! Overlays add a step, pick a skill or a source, and pick or write the
//! agent that runs a step. What it saves is an ordinary library document,
//! so running, resuming and reports are the Workflows section's own.

use super::text_area::TextArea;
use super::{App, Mode, Notice};
use crate::agents::{self, Catalog};
use crate::workflows::builder::{Check, Does, Draft, FieldKind, Items, Role, Runs};
use crate::workflows::library;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Steps,
    Exchange,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Fields,
}

/// One row of the right pane: a field of the flow or of the selected step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    FlowName,
    FlowDescription,
    FlowWhen,
    FlowArgs,
    FlowOutput,
    FlowBudget,
    FlowWorkspace,
    Name,
    Runs,
    Items,
    Does,
    Who,
    Gets,
    Answers,
    Checked,
    Votes,
    KeepBelow,
    VotersWho,
    Attempts,
    JudgeWho,
    Paths,
    RepeatBy,
    Filter,
    Harness,
    Model,
    Effort,
    Isolation,
}

/// How a field is changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edits {
    /// `Enter` types it.
    Text,
    /// `←`/`→` (or `Enter`) steps through values.
    Cycle,
    /// `Enter` opens a list.
    Pick,
    /// `Enter` opens another screen.
    Jump,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Field::FlowName => "Name",
            Field::FlowDescription => "What it does",
            Field::FlowWhen => "When to use",
            Field::FlowArgs => "Arguments",
            Field::FlowOutput => "Answer from",
            Field::FlowBudget => "Budget",
            Field::FlowWorkspace => "Workspace",
            Field::Name => "Name",
            Field::Runs => "Runs",
            Field::Items => "For each of",
            Field::Does => "Does",
            Field::Who => "Who",
            Field::Gets => "Gets",
            Field::Answers => "Answers with",
            Field::Checked => "Checked",
            Field::Votes => "Votes",
            Field::KeepBelow => "Keep when",
            Field::VotersWho => "Voters",
            Field::Attempts => "Attempts",
            Field::JudgeWho => "Judge",
            Field::Paths => "Paths",
            Field::RepeatBy => "Same when",
            Field::Filter => "Keep only",
            Field::Harness => "Harness",
            Field::Model => "Model",
            Field::Effort => "Effort",
            Field::Isolation => "Isolation",
        }
    }

    pub fn edits(self) -> Edits {
        match self {
            Field::Runs
            | Field::Checked
            | Field::Votes
            | Field::KeepBelow
            | Field::Attempts
            | Field::Harness
            | Field::Effort
            | Field::Isolation
            | Field::FlowOutput => Edits::Cycle,
            Field::Items
            | Field::Does
            | Field::Who
            | Field::Gets
            | Field::VotersWho
            | Field::JudgeWho => Edits::Pick,
            Field::Answers => Edits::Jump,
            _ => Edits::Text,
        }
    }
}

const HARNESSES: [&str; 4] = ["", "claude", "codex", "agy"];
const EFFORTS: [&str; 5] = ["", "low", "medium", "high", "max"];

/// What an inline text edit writes back to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditTarget {
    Field(Field),
    /// A new field of an answer shape.
    NewField(String),
    RenameField(String, String),
    /// The allowed values of a `one of` field.
    Values(String, String),
    /// `keep` on the Exchange screen.
    Filter,
    /// `dedupe_by` on the Exchange screen.
    Dedupe,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InlineEdit {
    pub target: EditTarget,
    pub text: TextArea,
}

/// A choice in a list overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickValue {
    Nothing,
    Skill(String),
    WritePrompt,
    Path(String),
    TypeList,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickOption {
    pub label: String,
    pub detail: String,
    pub value: PickValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickFor {
    Does,
    Gets,
    Items,
}

/// The fields of the new-agent form, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentField {
    Name,
    Purpose,
    Instructions,
    Tools,
    Mcp,
    ModelClaude,
    ModelCodex,
    ModelAgy,
    EffortCodex,
    EffortAgy,
    SaveTo,
}

impl AgentField {
    pub const ALL: [AgentField; 11] = [
        AgentField::Name,
        AgentField::Purpose,
        AgentField::Instructions,
        AgentField::Tools,
        AgentField::Mcp,
        AgentField::ModelClaude,
        AgentField::ModelCodex,
        AgentField::ModelAgy,
        AgentField::EffortCodex,
        AgentField::EffortAgy,
        AgentField::SaveTo,
    ];
}

pub const TOOL_NAMES: [&str; 4] = ["read files", "edit files", "run commands", "web"];

#[derive(Debug, Clone, PartialEq)]
pub struct AgentForm {
    pub field: AgentField,
    pub name: TextArea,
    pub purpose: TextArea,
    pub instructions: TextArea,
    /// read, edit, shell, web.
    pub tools: [bool; 4],
    pub tool_cursor: usize,
    pub mcp: TextArea,
    pub models: [TextArea; 3],
    /// Codex's and Antigravity's effort, indexes into `EFFORTS`.
    pub efforts: [usize; 2],
    pub to_workspace: bool,
    pub error: Option<String>,
}

impl AgentForm {
    fn new(role_hint: &str) -> AgentForm {
        AgentForm {
            field: AgentField::Name,
            name: TextArea::new(role_hint),
            purpose: TextArea::default(),
            instructions: TextArea::default(),
            tools: [true, false, true, false],
            tool_cursor: 0,
            mcp: TextArea::default(),
            models: Default::default(),
            efforts: [0, 0],
            to_workspace: true,
            error: None,
        }
    }

    pub fn text_mut(&mut self) -> Option<&mut TextArea> {
        match self.field {
            AgentField::Name => Some(&mut self.name),
            AgentField::Purpose => Some(&mut self.purpose),
            AgentField::Instructions => Some(&mut self.instructions),
            AgentField::Mcp => Some(&mut self.mcp),
            AgentField::ModelClaude => Some(&mut self.models[0]),
            AgentField::ModelCodex => Some(&mut self.models[1]),
            AgentField::ModelAgy => Some(&mut self.models[2]),
            _ => None,
        }
    }

    pub fn effort(&self, i: usize) -> &'static str {
        EFFORTS[self.efforts[i]]
    }

    /// The tools as the agent file names them.
    pub fn tool_list(&self) -> Vec<String> {
        let mut v: Vec<String> = ["read", "edit", "shell", "web"]
            .iter()
            .zip(self.tools)
            .filter(|(_, on)| *on)
            .map(|(t, _)| t.to_string())
            .collect();
        for m in self.mcp.text.split([',', ' ']).map(str::trim) {
            if !m.is_empty() {
                v.push(format!("mcp:{m}"));
            }
        }
        v
    }

    /// The agent file this form writes.
    pub fn render(&self) -> String {
        let q = |s: &str| toml::Value::String(s.to_string()).to_string();
        let tools = self.tool_list();
        let mut text = agents::cli::render(
            self.name.text.trim(),
            &self.purpose.text,
            &self.instructions.text,
            Some(&tools),
            None,
            None,
        );
        for (i, h) in agents::HARNESSES.iter().enumerate() {
            let model = self.models[i].text.trim();
            let effort = match i {
                1 => self.effort(0),
                2 => self.effort(1),
                _ => "",
            };
            if model.is_empty() && effort.is_empty() {
                continue;
            }
            text.push_str(&format!("\n[backends.{h}]\n"));
            if !model.is_empty() {
                text.push_str(&format!("model = {}\n", q(model)));
            }
            if !effort.is_empty() {
                text.push_str(&format!("effort = {}\n", q(effort)));
            }
        }
        text
    }
}

/// The agent overlay: pick one, or write a new one.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentPicker {
    pub role: Role,
    /// `None` is "no agent"; the rest are names from the catalog.
    pub options: Vec<Option<String>>,
    pub selected: usize,
    pub form: Option<AgentForm>,
}

impl AgentPicker {
    /// The index of "+ new agent", after the options.
    pub fn new_index(&self) -> usize {
        self.options.len()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    AddStep {
        name: TextArea,
        choice: usize,
        on_name: bool,
    },
    Pick {
        title: String,
        options: Vec<PickOption>,
        selected: usize,
        purpose: PickFor,
    },
    Agent(Box<AgentPicker>),
    Prompt {
        text: TextArea,
    },
    Confirm {
        question: String,
        action: ConfirmAction,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    DeleteStep,
    Discard,
    /// Remove the library copy of a built-in workflow.
    Restore,
    Overwrite {
        run: bool,
    },
}

/// Where the draft came from, for saving over it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    New,
    /// A library or built-in document of this name.
    Document(String),
    /// A planner's document (plan id).
    Plan(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExPane {
    Strip,
    Gives,
    Gets,
}

#[derive(Debug)]
pub struct FlowBuilderState {
    pub draft: Draft,
    pub origin: Origin,
    pub workspace: String,
    pub screen: Screen,
    /// 0 is the flow's own settings; step `i` is row `i + 1`.
    pub selected: usize,
    pub focus: Focus,
    pub field: usize,
    pub edit: Option<InlineEdit>,
    pub overlay: Option<Overlay>,
    pub ex_pane: ExPane,
    /// Groups opened inside the selected step's answer (`finding` inside
    /// `findings`), innermost last.
    pub ex_groups: Vec<String>,
    pub ex_field: usize,
    pub ex_gets: usize,
    pub review_scroll: usize,
    pub dirty: bool,
    pub catalog: Catalog,
    /// Step skills: id, description, whether it edits files.
    pub skills: Vec<(String, String, bool)>,
    pub checks: Vec<Check>,
    pub estimate: String,
    /// Agents a planner defined for the document, saved with it.
    pub plan_agents: Vec<String>,
    /// A built-in workflow: `Some(true)` when the library holds the
    /// user's copy of it.
    pub builtin: Option<bool>,
}

impl FlowBuilderState {
    pub fn step_index(&self) -> Option<usize> {
        self.selected.checked_sub(1)
    }

    /// The fields the right pane shows for the selected row.
    pub fn fields(&self) -> Vec<Field> {
        let Some(i) = self.step_index() else {
            return vec![
                Field::FlowName,
                Field::FlowDescription,
                Field::FlowWhen,
                Field::FlowArgs,
                Field::FlowOutput,
                Field::FlowBudget,
                Field::FlowWorkspace,
            ];
        };
        let runs = self.draft.runs(i);
        let mut v = vec![Field::Name, Field::Runs];
        if runs.has_items() {
            v.push(Field::Items);
        }
        v.extend([Field::Does, Field::Who, Field::Gets, Field::Answers]);
        match runs {
            Runs::Parallel => {
                v.push(Field::Checked);
                if self.draft.check_votes(i).is_some() {
                    v.extend([Field::Votes, Field::KeepBelow, Field::VotersWho]);
                }
                v.push(Field::Filter);
            }
            Runs::EachThenCheck => {
                v.extend([
                    Field::Votes,
                    Field::KeepBelow,
                    Field::VotersWho,
                    Field::Filter,
                ]);
            }
            Runs::BestOf => v.extend([Field::Attempts, Field::JudgeWho]),
            Runs::PickPath => v.push(Field::Paths),
            Runs::UntilQuiet => v.extend([Field::RepeatBy, Field::Filter]),
            Runs::Once => {}
        }
        v.extend([
            Field::Harness,
            Field::Model,
            Field::Effort,
            Field::Isolation,
        ]);
        v
    }

    pub fn current_field(&self) -> Option<Field> {
        self.fields().get(self.field).copied()
    }

    /// The text a field shows.
    pub fn value(&self, f: Field) -> String {
        let d = &self.draft;
        let dash = || "—".to_string();
        let Some(i) = self.step_index() else {
            return match f {
                Field::FlowName => d.name(),
                Field::FlowDescription => d.flow_str("description"),
                Field::FlowWhen => d.flow_str("when_to_use"),
                Field::FlowArgs => d.args_text(),
                Field::FlowOutput => d.output().unwrap_or_else(dash),
                Field::FlowBudget => d
                    .budget()
                    .map(|b| format!("{}k tokens", b / 1000))
                    .unwrap_or_else(|| "none".into()),
                Field::FlowWorkspace => self.workspace.clone(),
                _ => String::new(),
            };
        };
        let or_dash = |s: String| if s.is_empty() { dash() } else { s };
        match f {
            Field::Name => d.id(i),
            Field::Runs => d.runs(i).title().into(),
            Field::Items => match d.items(i) {
                Items::List(v) if v.is_empty() => "nothing yet".into(),
                Items::List(v) => v.join(", "),
                Items::From(p) => d.source_words(i, &p),
                Items::None if d.runs(i) == Runs::BestOf => {
                    format!("{} fresh attempts", d.attempts(i))
                }
                Items::None => "nothing yet".into(),
            },
            Field::Does => match d.does(i) {
                Does::Skill(k) => format!("skill {k}"),
                Does::Prompt(p) => format!("prompt: {}", one_line(&p)),
                Does::Nothing => "nothing yet".into(),
            },
            Field::Who => d.agent(i, Role::Step).unwrap_or_else(|| "no agent".into()),
            Field::Gets => match d.step(i).get("input").and_then(|v| v.as_str()) {
                Some(p) => d.source_words(i, p),
                None => "nothing from earlier steps".into(),
            },
            Field::Answers => match d.step(i).get("result").and_then(|v| v.as_str()) {
                Some(s) => {
                    let names: Vec<String> = d.fields(s).into_iter().map(|f| f.name).collect();
                    format!("{s}: {}", names.join(", "))
                }
                None => "free text".into(),
            },
            Field::Checked => if d.check_votes(i).is_some() {
                "yes"
            } else {
                "no"
            }
            .into(),
            Field::Votes => d
                .check_votes(i)
                .map(|(v, _)| format!("{v} independent"))
                .unwrap_or_else(dash),
            Field::KeepBelow => d
                .check_votes(i)
                .map(|(_, b)| Draft::keep_words(b))
                .unwrap_or_else(dash),
            Field::VotersWho => d
                .agent(i, Role::Voters)
                .unwrap_or_else(|| "no agent".into()),
            Field::Attempts => d.attempts(i).to_string(),
            Field::JudgeWho => d.agent(i, Role::Judge).unwrap_or_else(|| "no agent".into()),
            Field::Paths => or_dash(d.branches_text(i)),
            Field::RepeatBy => or_dash(d.list_text(i, "dedupe_by")),
            Field::Filter => or_dash(d.step_str(i, "keep")),
            Field::Harness => {
                let h = d.step_str(i, "harness");
                if h.is_empty() { "the run's".into() } else { h }
            }
            Field::Model => {
                let m = d.step_str(i, "model");
                if m.is_empty() {
                    "the agent's, else the profile's".into()
                } else {
                    m
                }
            }
            Field::Effort => or_dash(d.step_str(i, "effort")),
            Field::Isolation => {
                if d.step_str(i, "isolation") == "worktree" {
                    "worktree".into()
                } else {
                    "in place".into()
                }
            }
            _ => String::new(),
        }
    }

    /// The text an inline edit of `f` starts from.
    fn edit_text(&self, f: Field) -> String {
        let d = &self.draft;
        match (f, self.step_index()) {
            (Field::FlowBudget, _) => d.budget().map(|b| b.to_string()).unwrap_or_default(),
            (Field::Items, Some(i)) => match d.items(i) {
                Items::List(v) => v.join(", "),
                _ => String::new(),
            },
            (Field::Model, Some(i)) => d.step_str(i, "model"),
            (Field::Paths, Some(i)) => d.branches_text(i),
            (Field::RepeatBy, Some(i)) => d.list_text(i, "dedupe_by"),
            (Field::Filter, Some(i)) => d.step_str(i, "keep"),
            (f, _) => self.value(f),
        }
    }

    /// The answer shape the Exchange screen edits: the innermost group
    /// opened, else the step's own.
    pub fn ex_schema(&self) -> Option<String> {
        if let Some(g) = self.ex_groups.last() {
            return Some(g.clone());
        }
        let i = self.step_index()?;
        self.draft
            .step(i)
            .get("result")
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }

    fn touch(&mut self) {
        self.dirty = true;
    }

    /// Re-runs the checks and the estimate.
    pub fn refresh(&mut self, skill_infos: &[crate::workflows::document::SkillInfo]) {
        let mut checks = Vec::new();
        let mut ok = |text: &str| {
            checks.push(Check {
                ok: true,
                warning: false,
                text: text.into(),
            })
        };
        match self.draft.parsed() {
            Err(problems) => {
                self.estimate.clear();
                for p in problems {
                    checks.push(Check {
                        ok: false,
                        warning: false,
                        text: p,
                    });
                }
            }
            Ok(doc) => {
                self.estimate = crate::app::workflows_view::estimate_text(&doc);
                let problems = crate::workflows::validate(&doc, Some(skill_infos));
                let diags = agents::fit::check(&doc, &self.catalog, skill_infos);
                if problems.is_empty() {
                    ok("every step reads from an earlier step or an argument");
                    ok("every step has something to do and a valid shape");
                }
                if !doc.agents().is_empty()
                    && !diags
                        .iter()
                        .any(|d| d.level == agents::launch::Level::Error)
                {
                    ok("every agent exists and fits the step it runs");
                }
                for p in problems {
                    checks.push(Check {
                        ok: false,
                        warning: false,
                        text: p,
                    });
                }
                for d in diags {
                    checks.push(Check {
                        ok: false,
                        warning: d.level == agents::launch::Level::Warning,
                        text: d.message,
                    });
                }
            }
        }
        self.checks = checks;
    }

    pub fn problems(&self) -> usize {
        self.checks.iter().filter(|c| !c.ok && !c.warning).count()
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.draft.len());
        let n = self.fields().len();
        self.field = self.field.min(n.saturating_sub(1));
    }
}

fn one_line(text: &str) -> String {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > 60 {
        format!("{}…", t.chars().take(60).collect::<String>())
    } else {
        t
    }
}

fn cycle<T: PartialEq + Copy>(all: &[T], cur: T, delta: isize) -> T {
    let i = all.iter().position(|x| *x == cur).unwrap_or(0) as isize;
    all[(i + delta).rem_euclid(all.len() as isize) as usize]
}

/// What a key did to the builder.
enum After {
    Stay,
    Close,
    Into(Box<Mode>),
}

impl App {
    fn step_skills(&self) -> Vec<(String, String, bool)> {
        let mut v: Vec<(String, String, bool)> = self
            .skills
            .iter()
            .chain(self.hidden_skills.iter())
            .filter(|s| s.id.starts_with("wf-"))
            .map(|s| (s.id.clone(), s.description.clone(), s.writes))
            .collect();
        v.sort();
        v.dedup_by(|a, b| a.0 == b.0);
        v
    }

    fn builder_state(&self, draft: Draft, origin: Origin, workspace: String) -> FlowBuilderState {
        let catalog = Catalog::load(
            &self.library_root(),
            Some(std::path::Path::new(&workspace)).filter(|p| p.is_dir()),
        );
        let mut st = FlowBuilderState {
            draft,
            origin,
            workspace,
            screen: Screen::Steps,
            selected: 0,
            focus: Focus::Fields,
            field: 0,
            edit: None,
            overlay: None,
            ex_pane: ExPane::Gives,
            ex_groups: Vec::new(),
            ex_field: 0,
            ex_gets: 0,
            review_scroll: 0,
            dirty: false,
            catalog,
            skills: self.step_skills(),
            checks: Vec::new(),
            estimate: String::new(),
            plan_agents: Vec::new(),
            builtin: None,
        };
        st.refresh(&self.all_skill_infos());
        st.builtin = self.flow_builtin_state(&st);
        st
    }

    /// `f` in the Workflows section: a new flow.
    pub fn open_flow_builder_new(&mut self) {
        let names: Vec<String> = self
            .workflow_entries()
            .into_iter()
            .map(|e| e.name)
            .collect();
        let name = (1..)
            .map(|n| {
                if n == 1 {
                    "my-flow".to_string()
                } else {
                    format!("my-flow-{n}")
                }
            })
            .find(|n| !names.contains(n))
            .expect("a free name");
        let workspace = self
            .dialog_workspaces()
            .into_iter()
            .next()
            .unwrap_or_default();
        let mut st = self.builder_state(Draft::blank(&name), Origin::New, workspace);
        st.focus = Focus::List;
        self.notice = Some(Notice::info("a new flow: a adds the first step"));
        self.mode = Mode::FlowBuilder(Box::new(st));
    }

    /// `o` in the Workflows section: the selected document or plan in the
    /// builder.
    pub fn open_flow_builder_selected(&mut self) {
        use super::workflows::WorkflowRow;
        let workspace = self
            .dialog_workspaces()
            .into_iter()
            .next()
            .unwrap_or_default();
        let (text, origin, plan_agents, workspace) = match self.selected_workflow_row() {
            Some(WorkflowRow::Planned(id)) => {
                let Some(p) = self.planned_workflows.iter().find(|p| p.id == id).cloned() else {
                    return;
                };
                if p.document.is_empty() {
                    self.notice = Some(Notice::warn("the planner answered without a document"));
                    return;
                }
                (
                    p.document,
                    Origin::Plan(id),
                    p.agents,
                    p.workspace.to_string_lossy().into_owned(),
                )
            }
            Some(WorkflowRow::Doc(_)) => {
                let Some(e) = self.selected_workflow_entry().cloned() else {
                    return;
                };
                if let library::Source::Skill(id) = &e.source {
                    self.notice = Some(Notice::info(format!(
                        "{} is distributed by the {id} skill; f starts a new flow",
                        e.name
                    )));
                    return;
                }
                (e.text, Origin::Document(e.name), Vec::new(), workspace)
            }
            _ => {
                self.notice = Some(Notice::info(
                    "o opens a workflow or a plan in the builder; f starts a new flow",
                ));
                return;
            }
        };
        match Draft::from_text(&text) {
            Ok(d) => {
                let mut st = self.builder_state(d, origin, workspace);
                st.plan_agents = plan_agents;
                st.focus = Focus::List;
                st.selected = 1.min(st.draft.len());
                self.mode = Mode::FlowBuilder(Box::new(st));
            }
            Err(e) => self.notice = Some(Notice::error(e)),
        }
    }

    pub fn handle_flow_builder_key(&mut self, key: &KeyEvent) {
        let Mode::FlowBuilder(mut st) = std::mem::replace(&mut self.mode, Mode::Control) else {
            return;
        };
        let after = self.flow_key(&mut st, key);
        match after {
            After::Stay => self.mode = Mode::FlowBuilder(st),
            After::Close => self.mode = Mode::Control,
            After::Into(m) => self.mode = *m,
        }
    }

    fn flow_refresh(&self, st: &mut FlowBuilderState) {
        st.refresh(&self.all_skill_infos());
        st.builtin = self.flow_builtin_state(st);
        st.clamp();
    }

    fn flow_key(&mut self, st: &mut FlowBuilderState, key: &KeyEvent) -> After {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Ctrl+E: the text being typed goes to $EDITOR and comes back
        if ctrl && key.code == KeyCode::Char('e') && self.flow_text_mut(st).is_some() {
            self.flow_open_text_editor(st);
            return After::Stay;
        }
        if st.overlay.is_some() {
            return self.flow_overlay_key(st, key);
        }
        if st.edit.is_some() {
            self.flow_edit_key(st, key);
            return After::Stay;
        }
        match st.screen {
            Screen::Steps => self.flow_steps_key(st, key),
            Screen::Exchange => self.flow_exchange_key(st, key),
            Screen::Review => self.flow_review_key(st, key),
        }
    }

    /// The text field that has the cursor, if any.
    fn flow_text_mut<'a>(&self, st: &'a mut FlowBuilderState) -> Option<&'a mut TextArea> {
        if let Some(e) = &mut st.edit {
            return Some(&mut e.text);
        }
        match &mut st.overlay {
            Some(Overlay::Prompt { text }) => Some(text),
            Some(Overlay::AddStep {
                name,
                on_name: true,
                ..
            }) => Some(name),
            Some(Overlay::Agent(p)) => p.form.as_mut().and_then(|f| f.text_mut()),
            _ => None,
        }
    }

    fn flow_open_text_editor(&mut self, st: &mut FlowBuilderState) {
        let Some(t) = self.flow_text_mut(st) else {
            return;
        };
        let text = t.text.clone();
        let dir = self.workflows_runtime_dir().join("workflows");
        let path = dir.join("builder-text.md");
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, text)) {
            self.notice = Some(Notice::error(format!("{}: {e}", path.display())));
            return;
        }
        self.editor_request = Some(super::EditorRequest {
            path,
            asset_id: "flow:text".into(),
            command: crate::assets::editor_command(self.editor.as_deref()),
        });
    }

    /// After the editor: the text returns to the field it came from, or
    /// the whole document replaces the draft.
    pub fn flow_editor_finished(&mut self, request: &super::EditorRequest) {
        let text = match std::fs::read_to_string(&request.path) {
            Ok(t) => t,
            Err(e) => {
                self.notice = Some(Notice::error(format!("{}: {e}", request.path.display())));
                return;
            }
        };
        let Mode::FlowBuilder(mut st) = std::mem::replace(&mut self.mode, Mode::Control) else {
            return;
        };
        if request.asset_id.starts_with("flow:agent:") {
            // the agent changed on disk: reload it, and check it still loads
            st.catalog = Catalog::load(
                &self.library_root(),
                Some(std::path::Path::new(&st.workspace)).filter(|p| p.is_dir()),
            );
            let name = request.asset_id.trim_start_matches("flow:agent:");
            if let Some(e) = st.catalog.entry(name) {
                self.notice = Some(if e.problems.is_empty() {
                    Notice::info(format!("agent {name} saved"))
                } else {
                    Notice::warn(format!("agent {name}: {}", e.problems.join("; ")))
                });
            }
        } else if request.asset_id == "flow:doc" {
            match Draft::from_text(&text) {
                Ok(d) => {
                    st.draft = d;
                    st.dirty = true;
                    self.notice = Some(Notice::info("the document is back in the builder"));
                }
                Err(e) => {
                    self.notice = Some(Notice::error(format!(
                        "the edited document does not parse, nothing changed: {e}"
                    )))
                }
            }
        } else if let Some(t) = self.flow_text_mut(&mut st) {
            t.set(text.trim_end_matches('\n'));
        }
        self.flow_refresh(&mut st);
        self.mode = Mode::FlowBuilder(st);
    }

    fn flow_close(&mut self, st: &mut FlowBuilderState) -> After {
        if st.dirty {
            st.overlay = Some(Overlay::Confirm {
                question: "Leave the builder? Changes since the last save are lost. [y/n]".into(),
                action: ConfirmAction::Discard,
            });
            After::Stay
        } else {
            After::Close
        }
    }

    // ---- the Steps screen ---------------------------------------------------

    fn flow_steps_key(&mut self, st: &mut FlowBuilderState, key: &KeyEvent) -> After {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        // keys that work wherever the focus is
        match key.code {
            KeyCode::Esc => {
                if st.focus == Focus::Fields && st.step_index().is_some() {
                    st.focus = Focus::List;
                    return After::Stay;
                }
                return self.flow_close(st);
            }
            KeyCode::Char('a') => {
                let base = st
                    .step_index()
                    .map(|_| "step".to_string())
                    .unwrap_or_else(|| "step".into());
                let name = st.draft.fresh_id(&base);
                st.overlay = Some(Overlay::AddStep {
                    name: TextArea::new(name),
                    choice: 0,
                    on_name: false,
                });
                return After::Stay;
            }
            KeyCode::Char('w') => {
                if st.step_index().is_none() && !st.draft.is_empty() {
                    st.selected = 1;
                }
                st.screen = Screen::Exchange;
                st.ex_groups.clear();
                st.ex_field = 0;
                return After::Stay;
            }
            KeyCode::Char('v') => {
                st.screen = Screen::Review;
                st.review_scroll = 0;
                return After::Stay;
            }
            KeyCode::Char('g') if st.step_index().is_some() => {
                self.flow_open_agents(st, Role::Step);
                return After::Stay;
            }
            _ => {}
        }
        match st.focus {
            Focus::List => match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    st.selected = st.selected.saturating_sub(1);
                    st.field = 0;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    st.selected = (st.selected + 1).min(st.draft.len());
                    st.field = 0;
                }
                KeyCode::Char('K') => {
                    if let Some(i) = st.step_index() {
                        st.selected = st.draft.move_step(i, -1) + 1;
                        st.touch();
                    }
                }
                KeyCode::Char('J') => {
                    if let Some(i) = st.step_index() {
                        st.selected = st.draft.move_step(i, 1) + 1;
                        st.touch();
                    }
                }
                KeyCode::Char('d') | KeyCode::Delete if st.step_index().is_some() => {
                    st.overlay = Some(Overlay::Confirm {
                        question: format!("Delete step {}? [y/n]", st.draft.id(st.selected - 1)),
                        action: ConfirmAction::DeleteStep,
                    });
                }
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab => {
                    st.focus = Focus::Fields;
                    st.field = 0;
                }
                _ => {}
            },
            Focus::Fields => {
                let n = st.fields().len();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => st.field = st.field.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        st.field = (st.field + 1).min(n.saturating_sub(1))
                    }
                    KeyCode::Tab => st.field = (st.field + 1) % n.max(1),
                    KeyCode::BackTab => st.field = (st.field + n - 1) % n.max(1),
                    KeyCode::Left | KeyCode::Char('h')
                        if st.current_field().map(Field::edits) != Some(Edits::Cycle) =>
                    {
                        st.focus = Focus::List;
                    }
                    KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                        let delta = if key.code == KeyCode::Left || shift {
                            -1
                        } else {
                            1
                        };
                        if let Some(f) = st.current_field()
                            && f.edits() == Edits::Cycle
                        {
                            self.flow_cycle(st, f, delta);
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(f) = st.current_field() {
                            self.flow_activate(st, f);
                        }
                    }
                    _ => {}
                }
            }
        }
        self.flow_refresh(st);
        After::Stay
    }

    fn flow_cycle(&mut self, st: &mut FlowBuilderState, f: Field, delta: isize) {
        let d = &mut st.draft;
        if f == Field::FlowOutput {
            let ids = d.step_ids();
            if !ids.is_empty() {
                let cur = d.output().unwrap_or_default();
                let i = ids.iter().position(|x| *x == cur).unwrap_or(0) as isize;
                let next = &ids[(i + delta).rem_euclid(ids.len() as isize) as usize];
                d.set_flow_str("output", next);
            }
            st.touch();
            return;
        }
        let Some(i) = st.selected.checked_sub(1) else {
            return;
        };
        match f {
            Field::Runs => {
                let next = cycle(&Runs::ALL, d.runs(i), delta);
                d.set_runs(i, next);
            }
            Field::Checked => {
                let on = d.check_votes(i).is_none();
                d.set_checked(i, on);
            }
            Field::Votes => {
                if let Some((v, b)) = d.check_votes(i) {
                    d.set_check_votes(i, v + delta as i64, b);
                }
            }
            Field::KeepBelow => {
                if let Some((v, b)) = d.check_votes(i) {
                    d.set_check_votes(i, v, b + delta as i64);
                }
            }
            Field::Attempts => {
                let n = d.attempts(i);
                d.set_attempts(i, n + delta as i64);
            }
            Field::Harness => {
                let cur = d.step_str(i, "harness");
                let next = cycle(
                    &HARNESSES,
                    HARNESSES.iter().find(|h| **h == cur).copied().unwrap_or(""),
                    delta,
                );
                d.set_step_str(i, "harness", next);
            }
            Field::Effort => {
                let cur = d.step_str(i, "effort");
                let next = cycle(
                    &EFFORTS,
                    EFFORTS.iter().find(|h| **h == cur).copied().unwrap_or(""),
                    delta,
                );
                d.set_step_str(i, "effort", next);
            }
            Field::Isolation => {
                let wt = d.step_str(i, "isolation") == "worktree";
                d.set_step_str(i, "isolation", if wt { "" } else { "worktree" });
            }
            _ => return,
        }
        st.touch();
    }

    fn flow_activate(&mut self, st: &mut FlowBuilderState, f: Field) {
        match f.edits() {
            Edits::Cycle => self.flow_cycle(st, f, 1),
            Edits::Jump => {
                st.screen = Screen::Exchange;
                st.ex_pane = ExPane::Gives;
                st.ex_groups.clear();
                st.ex_field = 0;
            }
            Edits::Text => {
                st.edit = Some(InlineEdit {
                    target: EditTarget::Field(f),
                    text: TextArea::new(st.edit_text(f)),
                });
            }
            Edits::Pick => match f {
                Field::Who => self.flow_open_agents(st, Role::Step),
                Field::VotersWho => self.flow_open_agents(st, Role::Voters),
                Field::JudgeWho => self.flow_open_agents(st, Role::Judge),
                Field::Does => {
                    let mut options = vec![PickOption {
                        label: "write a prompt".into(),
                        detail: "say in words what the session does; {item}, {args.name} and {step} fill in".into(),
                        value: PickValue::WritePrompt,
                    }];
                    for (id, desc, writes) in &st.skills {
                        options.push(PickOption {
                            label: format!("{id}{}", if *writes { "  (edits files)" } else { "" }),
                            detail: desc.clone(),
                            value: PickValue::Skill(id.clone()),
                        });
                    }
                    let selected = match st.step_index().map(|i| st.draft.does(i)) {
                        Some(Does::Skill(k)) => options
                            .iter()
                            .position(|o| o.value == PickValue::Skill(k.clone()))
                            .unwrap_or(0),
                        _ => 0,
                    };
                    st.overlay = Some(Overlay::Pick {
                        title: "What does this step do?".into(),
                        options,
                        selected,
                        purpose: PickFor::Does,
                    });
                }
                Field::Gets | Field::Items => {
                    let Some(i) = st.step_index() else { return };
                    let items = f == Field::Items;
                    let mut options = vec![if items {
                        PickOption {
                            label: "a list I type".into(),
                            detail: "lenses, folders, questions: one session each".into(),
                            value: PickValue::TypeList,
                        }
                    } else {
                        PickOption {
                            label: "nothing from earlier steps".into(),
                            detail: "the session reads only its own context".into(),
                            value: PickValue::Nothing,
                        }
                    }];
                    for src in st.draft.sources(i) {
                        if items && !src.many && !src.path.starts_with("args.") {
                            continue;
                        }
                        options.push(PickOption {
                            label: src.words,
                            detail: src.path.clone(),
                            value: PickValue::Path(src.path),
                        });
                    }
                    let cur = if items {
                        match st.draft.items(i) {
                            Items::From(p) => Some(p),
                            _ => None,
                        }
                    } else {
                        st.draft
                            .step(i)
                            .get("input")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                    };
                    let selected = cur
                        .and_then(|c| {
                            options
                                .iter()
                                .position(|o| o.value == PickValue::Path(c.clone()))
                        })
                        .unwrap_or(0);
                    st.overlay = Some(Overlay::Pick {
                        title: if items {
                            "Run once for each of".into()
                        } else {
                            "What does this step read?".into()
                        },
                        options,
                        selected,
                        purpose: if items { PickFor::Items } else { PickFor::Gets },
                    });
                }
                _ => {}
            },
        }
    }

    // ---- inline text edits --------------------------------------------------

    fn flow_edit_key(&mut self, st: &mut FlowBuilderState, key: &KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(edit) = st.edit.as_mut() else { return };
        match key.code {
            KeyCode::Esc => st.edit = None,
            KeyCode::Enter => {
                let edit = st.edit.take().expect("an edit");
                if let Err(e) = self.flow_commit(st, &edit) {
                    self.notice = Some(Notice::warn(e));
                    st.edit = Some(edit);
                    return;
                }
                st.touch();
                self.flow_refresh(st);
            }
            _ => text_key(&mut edit.text, key, ctrl),
        }
    }

    fn flow_commit(&mut self, st: &mut FlowBuilderState, edit: &InlineEdit) -> Result<(), String> {
        let text = edit.text.text.trim().to_string();
        let d = &mut st.draft;
        let step = st.selected.checked_sub(1);
        match &edit.target {
            EditTarget::Field(f) => match (f, step) {
                (Field::FlowName, _) => {
                    if !crate::workflows::builder::is_id(&text) {
                        return Err(format!(
                            "{text:?}: a flow name must match ^[a-z][a-z0-9_-]*$"
                        ));
                    }
                    d.set_flow_str("name", &text);
                }
                (Field::FlowDescription, _) => d.set_flow_str("description", &text),
                (Field::FlowWhen, _) => d.set_flow_str("when_to_use", &text),
                (Field::FlowArgs, _) => d.set_args_text(&text)?,
                (Field::FlowBudget, _) => d.set_budget(&text)?,
                (Field::FlowWorkspace, _) => {
                    let p = match text.strip_prefix("~/") {
                        Some(rest) => std::env::var("HOME")
                            .map(|h| format!("{h}/{rest}"))
                            .unwrap_or(text.clone()),
                        None => text.clone(),
                    };
                    if !std::path::Path::new(&p).is_dir() {
                        return Err(format!("{text} is not a directory"));
                    }
                    st.workspace = p;
                    st.catalog = Catalog::load(
                        &self.library_root(),
                        Some(std::path::Path::new(&st.workspace)),
                    );
                }
                (Field::Name, Some(i)) => d.rename_step(i, &text)?,
                (Field::Items, Some(i)) => d.set_items_list(i, &text),
                (Field::Model, Some(i)) => d.set_step_str(i, "model", &text),
                (Field::Paths, Some(i)) => d.set_branches_text(i, &text)?,
                (Field::RepeatBy, Some(i)) => d.set_list(i, "dedupe_by", &text),
                (Field::Filter, Some(i)) => {
                    if !text.is_empty() {
                        crate::workflows::document::Predicate::parse(&text)?;
                    }
                    d.set_step_str(i, "keep", &text)
                }
                _ => {}
            },
            EditTarget::NewField(schema) => {
                d.add_field(schema, &text)?;
                st.ex_field = d
                    .fields(schema)
                    .iter()
                    .position(|f| f.name == text)
                    .unwrap_or(0);
            }
            EditTarget::RenameField(schema, old) => d.rename_field(schema, old, &text)?,
            EditTarget::Values(schema, name) => {
                let values: Vec<String> = text
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .collect();
                if values.len() < 2 {
                    return Err("one of needs two values or more, separated by commas".into());
                }
                d.set_kind(schema, name, FieldKind::OneOf(values));
            }
            EditTarget::Filter => {
                let Some(i) = step else { return Ok(()) };
                if !text.is_empty() {
                    crate::workflows::document::Predicate::parse(&text)?;
                }
                d.set_step_str(i, "keep", &text);
            }
            EditTarget::Dedupe => {
                let Some(i) = step else { return Ok(()) };
                d.set_list(i, "dedupe_by", &text);
            }
        }
        Ok(())
    }

    // ---- overlays -----------------------------------------------------------

    fn flow_overlay_key(&mut self, st: &mut FlowBuilderState, key: &KeyEvent) -> After {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(overlay) = st.overlay.as_mut() else {
            return After::Stay;
        };
        match overlay {
            Overlay::Confirm { action, .. } => {
                let action = *action;
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        st.overlay = None;
                        match action {
                            ConfirmAction::Discard => return After::Close,
                            ConfirmAction::DeleteStep => {
                                if let Some(i) = st.step_index() {
                                    st.draft.remove_step(i);
                                    st.selected = st.selected.min(st.draft.len());
                                    st.touch();
                                }
                            }
                            ConfirmAction::Overwrite { run } => {
                                return self.flow_save(st, run, true);
                            }
                            ConfirmAction::Restore => self.flow_restore(st),
                        }
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => st.overlay = None,
                    _ => {}
                }
            }
            Overlay::AddStep {
                name,
                choice,
                on_name,
            } => match key.code {
                KeyCode::Esc => st.overlay = None,
                KeyCode::Tab | KeyCode::BackTab => *on_name = !*on_name,
                KeyCode::Enter => {
                    let (id, runs) = (name.text.trim().to_string(), Runs::ALL[*choice]);
                    if !crate::workflows::builder::is_id(&id) {
                        self.notice = Some(Notice::warn(format!(
                            "{id:?}: a step name must match ^[a-z][a-z0-9_-]*$"
                        )));
                        return After::Stay;
                    }
                    let at = st.selected; // after the selected step (row 0: first)
                    let i = st.draft.add_step(at, &id, runs);
                    st.overlay = None;
                    st.selected = i + 1;
                    st.focus = Focus::Fields;
                    // straight to what the step does
                    st.field = st
                        .fields()
                        .iter()
                        .position(|f| *f == Field::Does)
                        .unwrap_or(0);
                    st.touch();
                }
                _ if *on_name => text_key(name, key, ctrl),
                KeyCode::Left | KeyCode::Char('h') => *choice = choice.saturating_sub(1),
                KeyCode::Right | KeyCode::Char('l') => *choice = (*choice + 1).min(5),
                KeyCode::Up | KeyCode::Char('k') => *choice = choice.saturating_sub(2),
                KeyCode::Down | KeyCode::Char('j') => *choice = (*choice + 2).min(5),
                KeyCode::Char(c) if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' => {
                    *on_name = true;
                    text_key(name, key, ctrl);
                }
                _ => {}
            },
            Overlay::Pick {
                options,
                selected,
                purpose,
                ..
            } => match key.code {
                KeyCode::Esc => st.overlay = None,
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1).min(options.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    let (value, purpose) = (options[*selected].value.clone(), *purpose);
                    st.overlay = None;
                    let Some(i) = st.step_index() else {
                        return After::Stay;
                    };
                    match value {
                        PickValue::WritePrompt => {
                            let cur = match st.draft.does(i) {
                                Does::Prompt(p) => p,
                                _ => String::new(),
                            };
                            st.overlay = Some(Overlay::Prompt {
                                text: TextArea::new(cur),
                            });
                        }
                        PickValue::Skill(k) => st.draft.set_skill(i, &k),
                        PickValue::Nothing => st.draft.set_step_str(i, "input", ""),
                        PickValue::Path(p) => match purpose {
                            PickFor::Items => st.draft.set_items_from(i, &p),
                            _ => st.draft.set_step_str(i, "input", &p),
                        },
                        PickValue::TypeList => {
                            st.edit = Some(InlineEdit {
                                target: EditTarget::Field(Field::Items),
                                text: TextArea::new(st.edit_text(Field::Items)),
                            });
                        }
                    }
                    st.touch();
                }
                _ => {}
            },
            Overlay::Prompt { text } => match key.code {
                KeyCode::Esc => st.overlay = None,
                KeyCode::Enter
                    if !key.modifiers.contains(KeyModifiers::ALT)
                        && !key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    let t = text.text.clone();
                    st.overlay = None;
                    if let Some(i) = st.step_index() {
                        st.draft.set_prompt(i, &t);
                        st.touch();
                    }
                }
                _ => text_key(text, key, ctrl),
            },
            Overlay::Agent(_) => return self.flow_agent_key(st, key),
        }
        self.flow_refresh(st);
        After::Stay
    }

    fn flow_open_agents(&mut self, st: &mut FlowBuilderState, role: Role) {
        let Some(i) = st.step_index() else { return };
        if role == Role::Voters && st.draft.check_votes(i).is_none() {
            self.notice = Some(Notice::info(
                "this step has no voters; turn Checked on first",
            ));
            return;
        }
        st.catalog = Catalog::load(
            &self.library_root(),
            Some(std::path::Path::new(&st.workspace)).filter(|p| p.is_dir()),
        );
        let mut options: Vec<Option<String>> = vec![None];
        options.extend(
            st.catalog
                .entries
                .iter()
                .filter(|e| e.spec.is_some())
                .map(|e| Some(e.name.clone())),
        );
        let cur = st.draft.agent(i, role);
        let selected = options.iter().position(|o| *o == cur).unwrap_or(0);
        st.overlay = Some(Overlay::Agent(Box::new(AgentPicker {
            role,
            options,
            selected,
            form: None,
        })));
    }

    fn flow_agent_key(&mut self, st: &mut FlowBuilderState, key: &KeyEvent) -> After {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(Overlay::Agent(p)) = st.overlay.as_mut() else {
            return After::Stay;
        };
        let Some(i) = st.selected.checked_sub(1) else {
            return After::Stay;
        };
        if let Some(form) = p.form.as_mut() {
            let pos = AgentField::ALL
                .iter()
                .position(|f| *f == form.field)
                .unwrap_or(0);
            match key.code {
                KeyCode::Esc => p.form = None,
                KeyCode::Char('s') if ctrl => {
                    let role = p.role;
                    match self.flow_save_agent(st) {
                        Ok(name) => {
                            st.draft.set_agent(i, role, Some(&name));
                            st.overlay = None;
                            st.touch();
                            self.notice =
                                Some(Notice::info(format!("agent {name} saved and used")));
                        }
                        Err(e) => {
                            if let Some(Overlay::Agent(p)) = st.overlay.as_mut()
                                && let Some(f) = p.form.as_mut()
                            {
                                f.error = Some(e);
                            }
                        }
                    }
                }
                KeyCode::Tab | KeyCode::Down
                    if key.code == KeyCode::Tab || form.field != AgentField::Instructions =>
                {
                    form.field = AgentField::ALL[(pos + 1) % AgentField::ALL.len()];
                }
                KeyCode::BackTab | KeyCode::Up
                    if key.code == KeyCode::BackTab || form.field != AgentField::Instructions =>
                {
                    form.field =
                        AgentField::ALL[(pos + AgentField::ALL.len() - 1) % AgentField::ALL.len()];
                }
                KeyCode::Enter
                    if key
                        .modifiers
                        .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
                {
                    if let Some(t) = form.text_mut() {
                        t.newline();
                    }
                }
                KeyCode::Enter => {
                    form.field = AgentField::ALL[(pos + 1) % AgentField::ALL.len()];
                }
                _ => match form.field {
                    AgentField::Tools => match key.code {
                        KeyCode::Left => form.tool_cursor = form.tool_cursor.saturating_sub(1),
                        KeyCode::Right => form.tool_cursor = (form.tool_cursor + 1).min(3),
                        KeyCode::Char(' ') => {
                            form.tools[form.tool_cursor] = !form.tools[form.tool_cursor]
                        }
                        _ => {}
                    },
                    AgentField::EffortCodex | AgentField::EffortAgy => {
                        let k = if form.field == AgentField::EffortCodex {
                            0
                        } else {
                            1
                        };
                        let delta: isize = match key.code {
                            KeyCode::Left => -1,
                            KeyCode::Right | KeyCode::Char(' ') => 1,
                            _ => 0,
                        };
                        form.efforts[k] = (form.efforts[k] as isize + delta)
                            .rem_euclid(EFFORTS.len() as isize)
                            as usize;
                    }
                    AgentField::SaveTo => {
                        if matches!(
                            key.code,
                            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                        ) {
                            form.to_workspace = !form.to_workspace;
                        }
                    }
                    _ => {
                        if let Some(t) = form.text_mut() {
                            text_key(t, key, ctrl);
                        }
                        form.error = None;
                    }
                },
            }
            return After::Stay;
        }
        match key.code {
            KeyCode::Esc => st.overlay = None,
            KeyCode::Up | KeyCode::Char('k') => p.selected = p.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => p.selected = (p.selected + 1).min(p.new_index()),
            KeyCode::Char('n') => {
                p.selected = p.new_index();
                p.form = Some(AgentForm::new(""));
            }
            KeyCode::Char('e') => {
                if let Some(Some(name)) = p.options.get(p.selected).cloned() {
                    self.flow_edit_agent(st, &name);
                }
            }
            KeyCode::Enter => {
                if p.selected == p.new_index() {
                    let hint = match p.role {
                        Role::Voters => "skeptic",
                        Role::Judge => "judge",
                        Role::Step => "",
                    };
                    let hint = if st.catalog.get(hint).is_some() {
                        ""
                    } else {
                        hint
                    };
                    p.form = Some(AgentForm::new(hint));
                } else {
                    let (role, agent) = (p.role, p.options[p.selected].clone());
                    st.draft.set_agent(i, role, agent.as_deref());
                    st.overlay = None;
                    st.touch();
                    self.flow_refresh(st);
                }
            }
            _ => {}
        }
        After::Stay
    }

    /// `e` in the agent picker: the agent's file in `$EDITOR`. A built-in
    /// one is copied into the library first, and the copy is what changes.
    fn flow_edit_agent(&mut self, st: &mut FlowBuilderState, name: &str) {
        let Some(entry) = st.catalog.entry(name).cloned() else {
            return;
        };
        let path = entry.source.path().to_path_buf();
        if let agents::Source::Builtin(_) = entry.source
            && !path.exists()
        {
            let text = agents::text_of(&entry).unwrap_or_default();
            if let Err(e) = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|_| std::fs::write(&path, text))
            {
                self.notice = Some(Notice::error(format!("{}: {e}", path.display())));
                return;
            }
            self.notice = Some(Notice::info(format!(
                "editing your copy of the built-in {name}; R in the Configuration view restores it"
            )));
        }
        self.editor_request = Some(super::EditorRequest {
            path,
            asset_id: format!("flow:agent:{name}"),
            command: crate::assets::editor_command(self.editor.as_deref()),
        });
    }

    /// Writes the agent form to the workspace or the library; the name.
    fn flow_save_agent(&mut self, st: &mut FlowBuilderState) -> Result<String, String> {
        let Some(Overlay::Agent(p)) = &st.overlay else {
            return Err("no agent form".into());
        };
        let Some(form) = &p.form else {
            return Err("no agent form".into());
        };
        let name = form.name.text.trim().to_string();
        if !agents::is_valid_name(&name) {
            return Err(format!(
                "{name:?}: an agent name must match ^[a-z][a-z0-9_-]*$"
            ));
        }
        if st.catalog.entry(&name).is_some() {
            return Err(format!(
                "an agent called {name} already exists: pick it from the list, e edits it"
            ));
        }
        let text = form.render();
        agents::AgentSpec::parse(&text).map_err(|p| p.join("; "))?;
        let dir = if form.to_workspace {
            if !std::path::Path::new(&st.workspace).is_dir() {
                return Err("the flow's workspace does not exist; save to the library".into());
            }
            agents::workspace_dir(std::path::Path::new(&st.workspace))
        } else {
            agents::library_dir(&self.library_root())
        };
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(format!("{name}.toml"));
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        st.catalog = Catalog::load(
            &self.library_root(),
            Some(std::path::Path::new(&st.workspace)).filter(|p| p.is_dir()),
        );
        Ok(name)
    }

    // ---- the Exchange screen ------------------------------------------------

    fn flow_exchange_key(&mut self, st: &mut FlowBuilderState, key: &KeyEvent) -> After {
        let Some(i) = st.step_index() else {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('w')) {
                st.screen = Screen::Steps;
            }
            return After::Stay;
        };
        match key.code {
            KeyCode::Esc if !st.ex_groups.is_empty() => {
                st.ex_groups.pop();
                st.ex_field = 0;
                return After::Stay;
            }
            KeyCode::Esc | KeyCode::Char('w') => {
                st.screen = Screen::Steps;
                return After::Stay;
            }
            KeyCode::Char('v') => {
                st.screen = Screen::Review;
                return After::Stay;
            }
            KeyCode::Tab => {
                st.ex_pane = match st.ex_pane {
                    ExPane::Strip => ExPane::Gives,
                    ExPane::Gives => ExPane::Gets,
                    ExPane::Gets => ExPane::Strip,
                };
                return After::Stay;
            }
            KeyCode::BackTab => {
                st.ex_pane = match st.ex_pane {
                    ExPane::Strip => ExPane::Gets,
                    ExPane::Gives => ExPane::Strip,
                    ExPane::Gets => ExPane::Gives,
                };
                return After::Stay;
            }
            KeyCode::Char('[') | KeyCode::PageUp => {
                st.selected = (st.selected - 1).max(1);
                st.ex_groups.clear();
                st.ex_field = 0;
                st.ex_gets = 0;
                return After::Stay;
            }
            KeyCode::Char(']') | KeyCode::PageDown => {
                st.selected = (st.selected + 1).min(st.draft.len());
                st.ex_groups.clear();
                st.ex_field = 0;
                st.ex_gets = 0;
                return After::Stay;
            }
            _ => {}
        }
        match st.ex_pane {
            ExPane::Strip => match key.code {
                KeyCode::Left | KeyCode::Char('h') => {
                    st.selected = (st.selected - 1).max(1);
                    st.ex_groups.clear();
                    st.ex_field = 0;
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    st.selected = (st.selected + 1).min(st.draft.len());
                    st.ex_groups.clear();
                    st.ex_field = 0;
                }
                KeyCode::Enter | KeyCode::Down => st.ex_pane = ExPane::Gives,
                _ => {}
            },
            ExPane::Gives => {
                let schema = st.ex_schema();
                let fields = schema
                    .as_deref()
                    .map(|s| st.draft.fields(s))
                    .unwrap_or_default();
                let cur = fields.get(st.ex_field).cloned();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => st.ex_field = st.ex_field.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        st.ex_field = (st.ex_field + 1).min(fields.len().saturating_sub(1))
                    }
                    KeyCode::Char('+') | KeyCode::Char('n') | KeyCode::Char('a') => {
                        let schema = match schema {
                            Some(s) => s,
                            None => st.draft.ensure_result_schema(i),
                        };
                        st.edit = Some(InlineEdit {
                            target: EditTarget::NewField(schema),
                            text: TextArea::default(),
                        });
                    }
                    KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                        if let (Some(s), Some(f)) = (&schema, &cur) {
                            let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                            st.draft.cycle_kind(s, &f.name, delta);
                            st.touch();
                        }
                    }
                    KeyCode::Char('r') => {
                        if let (Some(s), Some(f)) = (&schema, &cur) {
                            st.draft.toggle_required(s, &f.name);
                            st.touch();
                        }
                    }
                    KeyCode::Char('d') | KeyCode::Delete => {
                        if let (Some(s), Some(f)) = (&schema, &cur) {
                            st.draft.remove_field(s, &f.name);
                            st.ex_field = st.ex_field.saturating_sub(1);
                            st.touch();
                        }
                    }
                    KeyCode::Char('e') => {
                        if let (Some(s), Some(f)) = (&schema, &cur) {
                            st.edit = Some(InlineEdit {
                                target: EditTarget::RenameField(s.clone(), f.name.clone()),
                                text: TextArea::new(f.name.clone()),
                            });
                        }
                    }
                    KeyCode::Enter => {
                        if let (Some(s), Some(f)) = (&schema, &cur) {
                            match &f.kind {
                                FieldKind::ListOf(g) => {
                                    st.ex_groups.push(g.clone());
                                    st.ex_field = 0;
                                }
                                FieldKind::OneOf(v) => {
                                    st.edit = Some(InlineEdit {
                                        target: EditTarget::Values(s.clone(), f.name.clone()),
                                        text: TextArea::new(v.join(", ")),
                                    });
                                }
                                _ => {
                                    st.edit = Some(InlineEdit {
                                        target: EditTarget::RenameField(s.clone(), f.name.clone()),
                                        text: TextArea::new(f.name.clone()),
                                    });
                                }
                            }
                        }
                    }
                    KeyCode::Backspace if !st.ex_groups.is_empty() => {
                        st.ex_groups.pop();
                        st.ex_field = 0;
                    }
                    _ => {}
                }
            }
            ExPane::Gets => {
                let n = st.draft.sources(i).len() + 1;
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => st.ex_gets = st.ex_gets.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        st.ex_gets = (st.ex_gets + 1).min(n.saturating_sub(1))
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if st.ex_gets == 0 {
                            st.draft.set_step_str(i, "input", "");
                        } else if let Some(src) = st.draft.sources(i).get(st.ex_gets - 1) {
                            let p = src.path.clone();
                            st.draft.set_step_str(i, "input", &p);
                        }
                        st.touch();
                    }
                    KeyCode::Char('f') => {
                        st.edit = Some(InlineEdit {
                            target: EditTarget::Filter,
                            text: TextArea::new(st.draft.step_str(i, "keep")),
                        });
                    }
                    KeyCode::Char('u') => {
                        st.edit = Some(InlineEdit {
                            target: EditTarget::Dedupe,
                            text: TextArea::new(st.draft.list_text(i, "dedupe_by")),
                        });
                    }
                    _ => {}
                }
            }
        }
        self.flow_refresh(st);
        After::Stay
    }

    // ---- the Review screen --------------------------------------------------

    fn flow_review_key(&mut self, st: &mut FlowBuilderState, key: &KeyEvent) -> After {
        match key.code {
            KeyCode::Esc | KeyCode::Char('v') | KeyCode::Left => st.screen = Screen::Steps,
            KeyCode::Char('w') => st.screen = Screen::Exchange,
            KeyCode::Up | KeyCode::Char('k') => {
                st.review_scroll = st.review_scroll.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => st.review_scroll += 1,
            KeyCode::PageUp => st.review_scroll = st.review_scroll.saturating_sub(15),
            KeyCode::PageDown => st.review_scroll += 15,
            KeyCode::Char('s') => return self.flow_save(st, false, false),
            KeyCode::Char('R') => match self.flow_builtin_state(st) {
                Some(true) => {
                    st.overlay = Some(Overlay::Confirm {
                        question: format!(
                            "Restore the built-in {}? Your saved copy is removed. [y/n]",
                            st.draft.name()
                        ),
                        action: ConfirmAction::Restore,
                    })
                }
                _ => {
                    self.notice = Some(Notice::info(
                        "R restores a built-in workflow you saved a copy of",
                    ))
                }
            },
            KeyCode::Enter => return self.flow_save(st, true, false),
            KeyCode::Char('e') => {
                let dir = self.workflows_runtime_dir().join("workflows");
                let path = dir.join(format!("{}.builder.toml", st.draft.name()));
                if let Err(e) = std::fs::create_dir_all(&dir)
                    .and_then(|_| std::fs::write(&path, st.draft.to_toml()))
                {
                    self.notice = Some(Notice::error(format!("{}: {e}", path.display())));
                } else {
                    self.editor_request = Some(super::EditorRequest {
                        path,
                        asset_id: "flow:doc".into(),
                        command: crate::assets::editor_command(self.editor.as_deref()),
                    });
                }
            }
            _ => {}
        }
        After::Stay
    }

    /// Whether the draft is a built-in workflow, and whether the library
    /// holds the user's copy of it.
    pub fn flow_builtin_state(&self, st: &FlowBuilderState) -> Option<bool> {
        let Origin::Document(name) = &st.origin else {
            return None;
        };
        library::builtin(name)?;
        Some(
            library::dir(&self.library_root())
                .join(format!("{name}.toml"))
                .exists(),
        )
    }

    /// `R`: the library copy goes, the built-in comes back into the builder.
    fn flow_restore(&mut self, st: &mut FlowBuilderState) {
        let Origin::Document(name) = st.origin.clone() else {
            return;
        };
        let Some(text) = library::builtin(&name) else {
            return;
        };
        let path = library::dir(&self.library_root()).join(format!("{name}.toml"));
        if let Err(e) = std::fs::remove_file(&path) {
            self.notice = Some(Notice::error(format!("{}: {e}", path.display())));
            return;
        }
        if let Ok(d) = Draft::from_text(text) {
            st.draft = d;
        }
        st.dirty = false;
        st.selected = st.selected.min(st.draft.len());
        self.reload_workflow_list();
        self.flow_refresh(st);
        self.notice = Some(Notice::info(format!("{name} is the built-in again")));
    }

    /// Saves the flow into the library; `run` then opens the run dialog.
    fn flow_save(&mut self, st: &mut FlowBuilderState, run: bool, overwrite: bool) -> After {
        self.flow_refresh(st);
        let name = st.draft.name();
        if !crate::workflows::builder::is_id(&name) {
            self.notice = Some(Notice::warn(format!("{name:?} is not a valid flow name")));
            return After::Stay;
        }
        if run && st.problems() > 0 {
            self.notice = Some(Notice::warn(format!(
                "{} problem(s) to fix before it runs; s saves it as it is",
                st.problems()
            )));
            return After::Stay;
        }
        let path = library::dir(&self.library_root()).join(format!("{name}.toml"));
        let ours = st.origin == Origin::Document(name.clone());
        if path.exists() && !ours && !overwrite {
            st.overlay = Some(Overlay::Confirm {
                question: format!("{name} is already in the library. Replace it? [y/n]"),
                action: ConfirmAction::Overwrite { run },
            });
            return After::Stay;
        }
        if let Err(e) =
            crate::workflows::planner::save_agents(&self.library_root(), &st.plan_agents)
        {
            self.notice = Some(Notice::error(e));
            return After::Stay;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, st.draft.to_toml()) {
            self.notice = Some(Notice::error(format!("{}: {e}", path.display())));
            return After::Stay;
        }
        st.origin = Origin::Document(name.clone());
        st.dirty = false;
        st.builtin = self.flow_builtin_state(st);
        self.reload_workflow_list();
        let saved = if library::builtin(&name).is_some() {
            format!(
                "saved {}: your copy replaces the built-in {name}; R on Review restores it",
                path.display()
            )
        } else {
            format!("saved {}", path.display())
        };
        if !run {
            self.notice = Some(if st.problems() > 0 {
                Notice::warn(format!("{saved}; {} problem(s) left", st.problems()))
            } else {
                Notice::info(saved)
            });
            return After::Stay;
        }
        let Some(entry) = self
            .workflow_entries()
            .into_iter()
            .find(|e| e.name == name && matches!(e.source, library::Source::Library(_)))
        else {
            self.notice = Some(Notice::error(format!("{saved}, but it is not in the list")));
            return After::Stay;
        };
        let mut workspaces = vec![st.workspace.clone()];
        workspaces.extend(
            self.dialog_workspaces()
                .into_iter()
                .filter(|w| *w != st.workspace),
        );
        let d =
            super::workflows_view::WorkflowDialogState::for_run(&entry, &self.profiles, workspaces);
        self.notice = Some(Notice::info(saved));
        After::Into(Box::new(Mode::WorkflowDialog(Box::new(d))))
    }
}

/// The keys every builder text field shares.
pub(crate) fn text_key(t: &mut TextArea, key: &KeyEvent, ctrl: bool) {
    match key.code {
        KeyCode::Backspace => t.backspace(),
        KeyCode::Delete => t.delete(),
        KeyCode::Left => {
            t.left();
        }
        KeyCode::Right => {
            t.right();
        }
        KeyCode::Up => {
            t.up();
        }
        KeyCode::Down => {
            t.down();
        }
        KeyCode::Home => t.home(),
        KeyCode::End => t.end(),
        KeyCode::Char('w') if ctrl => t.delete_word(),
        KeyCode::Char('u') if ctrl => t.set(""),
        KeyCode::Char('j') if ctrl => t.newline(),
        KeyCode::Enter => t.newline(),
        KeyCode::Char(c) if !ctrl => t.insert(c),
        _ => {}
    }
}
