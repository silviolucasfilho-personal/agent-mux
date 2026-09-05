//! Which CLI a profile launches, and how each one spells the launch
//! options the new-session dialog offers.
//!
//! The three harnesses agree on the *concepts* — a model, skipping
//! approval prompts, resuming, a one-shot prompt — and disagree on almost
//! every spelling. Codex is the odd one structurally: it says one-shot
//! and resume as subcommands that have to lead the command line, while
//! Claude and Antigravity say them as ordinary flags.
//!
//! Every mapping below was checked against `--help` on the installed
//! CLIs (codex-cli 0.153.2) rather than taken from documentation.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
    Antigravity,
}

impl Harness {
    /// The harness a profile command runs, by program name — deliberately
    /// the same rule the trace planner uses (`file_stem`), so a profile
    /// is understood the same way by both and cannot be traced as one
    /// CLI while being given another's flags.
    pub fn detect(command: &str) -> Option<Harness> {
        match Path::new(command)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(command)
        {
            "claude" => Some(Harness::Claude),
            "codex" => Some(Harness::Codex),
            "agy" => Some(Harness::Antigravity),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Antigravity => "agy",
        }
    }
}

/// Which conversation a launch should pick up.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Resume {
    #[default]
    Off,
    /// The most recent conversation, whatever it was.
    Last,
    /// One named conversation, from the history viewer or trace browser.
    Id(String),
}

/// The per-launch choices the dialog collects. Anything left unset
/// renders nothing at all, so the CLI keeps its own default.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchOptions {
    pub model: Option<String>,
    pub bypass_approvals: bool,
    pub resume: Resume,
    /// Run this prompt non-interactively and exit.
    pub one_shot: Option<String>,
}

/// Argv split around the profile's own arguments: `leading` carries the
/// subcommand a harness requires up front, `trailing` the flags and the
/// positional prompt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Rendered {
    pub leading: Vec<String>,
    pub trailing: Vec<String>,
}

impl LaunchOptions {
    /// True when nothing was chosen, so the command line is untouched.
    pub fn is_empty(&self) -> bool {
        self.model.as_ref().is_none_or(|m| m.trim().is_empty())
            && !self.bypass_approvals
            && self.resume == Resume::Off
            && self.one_shot.as_ref().is_none_or(|p| p.trim().is_empty())
    }

    fn model(&self) -> Option<&str> {
        self.model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
    }

    fn prompt(&self) -> Option<&str> {
        self.one_shot
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
    }

    /// The argv these options add for `harness`.
    pub fn render(&self, harness: Harness) -> Rendered {
        let mut out = Rendered::default();
        let prompt = self.prompt();
        match harness {
            Harness::Claude | Harness::Antigravity => {
                match &self.resume {
                    Resume::Off => {}
                    // bare `--resume` opens a picker, so "last" is
                    // `--continue` on both of these
                    Resume::Last => out.trailing.push("--continue".into()),
                    Resume::Id(id) => {
                        out.trailing.push(if harness == Harness::Claude {
                            "--resume".into()
                        } else {
                            "--conversation".into()
                        });
                        out.trailing.push(id.clone());
                    }
                }
                if let Some(model) = self.model() {
                    out.trailing.push("--model".into());
                    out.trailing.push(model.to_string());
                }
                if self.bypass_approvals {
                    out.trailing.push("--dangerously-skip-permissions".into());
                }
                if let Some(prompt) = prompt {
                    out.trailing.push("-p".into());
                    out.trailing.push(prompt.to_string());
                }
            }
            Harness::Codex => {
                // `exec` and `resume` are subcommands, and `codex exec
                // resume …` is how the two compose
                if prompt.is_some() {
                    out.leading.push("exec".into());
                }
                match &self.resume {
                    Resume::Off => {}
                    Resume::Last => {
                        out.leading.push("resume".into());
                        out.leading.push("--last".into());
                    }
                    Resume::Id(id) => {
                        out.leading.push("resume".into());
                        out.leading.push(id.clone());
                    }
                }
                if let Some(model) = self.model() {
                    out.trailing.push("--model".into());
                    out.trailing.push(model.to_string());
                }
                if self.bypass_approvals {
                    // an accepted alias of
                    // --dangerously-bypass-approvals-and-sandbox
                    out.trailing.push("--yolo".into());
                }
                if let Some(prompt) = prompt {
                    // positional, and last
                    out.trailing.push(prompt.to_string());
                }
            }
        }
        out
    }
}

/// The arguments that resume one recorded conversation — the whole
/// command line, since a resume replaces a profile's own arguments
/// rather than adding to them (a profile carrying `--continue` must not
/// fight the id being resumed).
pub fn resume_args(harness: Harness, session_id: &str) -> Vec<String> {
    let options = LaunchOptions {
        resume: Resume::Id(session_id.to_string()),
        ..Default::default()
    };
    compose(&[], &options.render(harness))
}

/// The full argument list for a launch: the harness's subcommand, then
/// the profile's own arguments, then the options' flags.
pub fn compose(profile_args: &[String], rendered: &Rendered) -> Vec<String> {
    let mut args = rendered.leading.clone();
    args.extend(profile_args.iter().cloned());
    args.extend(rendered.trailing.iter().cloned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> LaunchOptions {
        LaunchOptions::default()
    }

    fn rendered(o: &LaunchOptions, h: Harness) -> Vec<String> {
        compose(&[], &o.render(h))
    }

    #[test]
    fn a_command_is_matched_by_program_name() {
        assert_eq!(Harness::detect("claude"), Some(Harness::Claude));
        assert_eq!(Harness::detect("codex"), Some(Harness::Codex));
        assert_eq!(Harness::detect("agy"), Some(Harness::Antigravity));
        assert_eq!(
            Harness::detect("/home/me/.local/bin/claude"),
            Some(Harness::Claude)
        );
        // `.exe` is stripped like any extension; a backslash path only
        // splits on Windows, which is where it would appear
        assert_eq!(Harness::detect("/opt/bin/codex.exe"), Some(Harness::Codex));
        #[cfg(windows)]
        assert_eq!(Harness::detect(r"C:\tools\codex.exe"), Some(Harness::Codex));
        assert_eq!(Harness::detect("bash"), None);
        assert_eq!(Harness::detect("my-claude-wrapper"), None);
    }

    #[test]
    fn nothing_chosen_adds_nothing() {
        let o = opts();
        assert!(o.is_empty());
        for h in [Harness::Claude, Harness::Codex, Harness::Antigravity] {
            assert!(rendered(&o, h).is_empty(), "{h:?}");
        }
        // a blank model or prompt is the same as unset: never pass an
        // empty parameter
        let blank = LaunchOptions {
            model: Some("   ".into()),
            one_shot: Some("".into()),
            ..Default::default()
        };
        assert!(blank.is_empty());
        for h in [Harness::Claude, Harness::Codex, Harness::Antigravity] {
            assert!(rendered(&blank, h).is_empty(), "{h:?}");
        }
    }

    #[test]
    fn model_and_approvals_use_each_cli_s_spelling() {
        let o = LaunchOptions {
            model: Some("claude-opus-5".into()),
            bypass_approvals: true,
            ..Default::default()
        };
        assert_eq!(
            rendered(&o, Harness::Claude),
            vec!["--model", "claude-opus-5", "--dangerously-skip-permissions"]
        );
        assert_eq!(
            rendered(&o, Harness::Antigravity),
            vec!["--model", "claude-opus-5", "--dangerously-skip-permissions"]
        );
        assert_eq!(
            rendered(&o, Harness::Codex),
            vec!["--model", "claude-opus-5", "--yolo"]
        );
        // the model is trimmed, not quoted or defaulted
        let padded = LaunchOptions {
            model: Some("  gpt-5.6  ".into()),
            ..Default::default()
        };
        assert_eq!(
            rendered(&padded, Harness::Codex),
            vec!["--model", "gpt-5.6"]
        );
    }

    #[test]
    fn resume_distinguishes_the_last_session_from_a_named_one() {
        let last = LaunchOptions {
            resume: Resume::Last,
            ..Default::default()
        };
        assert_eq!(rendered(&last, Harness::Claude), vec!["--continue"]);
        assert_eq!(rendered(&last, Harness::Antigravity), vec!["--continue"]);
        assert_eq!(rendered(&last, Harness::Codex), vec!["resume", "--last"]);

        let by_id = LaunchOptions {
            resume: Resume::Id("abc-123".into()),
            ..Default::default()
        };
        assert_eq!(
            rendered(&by_id, Harness::Claude),
            vec!["--resume", "abc-123"]
        );
        assert_eq!(
            rendered(&by_id, Harness::Antigravity),
            vec!["--conversation", "abc-123"],
            "agy's -c means --continue; by id is --conversation"
        );
        assert_eq!(rendered(&by_id, Harness::Codex), vec!["resume", "abc-123"]);
    }

    #[test]
    fn a_one_shot_prompt_is_positional_and_last() {
        let o = LaunchOptions {
            one_shot: Some("fix the failing test".into()),
            model: Some("gpt-5.6".into()),
            ..Default::default()
        };
        assert_eq!(
            rendered(&o, Harness::Claude),
            vec!["--model", "gpt-5.6", "-p", "fix the failing test"]
        );
        assert_eq!(
            rendered(&o, Harness::Antigravity),
            vec!["--model", "gpt-5.6", "-p", "fix the failing test"]
        );
        // codex spells it as a subcommand, which has to lead
        let r = o.render(Harness::Codex);
        assert_eq!(r.leading, vec!["exec"]);
        assert_eq!(
            r.trailing,
            vec!["--model", "gpt-5.6", "fix the failing test"]
        );
    }

    #[test]
    fn one_shot_composes_with_resume() {
        let o = LaunchOptions {
            one_shot: Some("summarize".into()),
            resume: Resume::Last,
            ..Default::default()
        };
        assert_eq!(
            rendered(&o, Harness::Claude),
            vec!["--continue", "-p", "summarize"]
        );
        // `codex exec resume --last <prompt>`
        assert_eq!(
            rendered(&o, Harness::Codex),
            vec!["exec", "resume", "--last", "summarize"]
        );
    }

    #[test]
    fn resume_args_covers_every_harness_including_codex() {
        assert_eq!(
            resume_args(Harness::Claude, "abc-123"),
            vec!["--resume", "abc-123"]
        );
        assert_eq!(
            resume_args(Harness::Antigravity, "abc-123"),
            vec!["--conversation", "abc-123"]
        );
        // codex has no --resume flag at all: it is a subcommand
        assert_eq!(
            resume_args(Harness::Codex, "abc-123"),
            vec!["resume", "abc-123"]
        );
    }

    #[test]
    fn the_profiles_own_arguments_sit_between_subcommand_and_flags() {
        let o = LaunchOptions {
            one_shot: Some("go".into()),
            bypass_approvals: true,
            ..Default::default()
        };
        let profile_args = vec!["--search".to_string()];
        assert_eq!(
            compose(&profile_args, &o.render(Harness::Codex)),
            vec!["exec", "--search", "--yolo", "go"]
        );
        assert_eq!(
            compose(&profile_args, &o.render(Harness::Claude)),
            vec!["--search", "--dangerously-skip-permissions", "-p", "go"]
        );
    }
}
