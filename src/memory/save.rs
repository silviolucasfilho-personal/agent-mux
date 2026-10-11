//! Saved on close: when an interactive session closes, or is still open
//! when agent-mux quits, a copy of it is asked once, in the background,
//! to save what the next session in its workspace will need.
//!
//! The App writes a `Job` under `<memory>/.saves/` and starts
//! `agent-mux memory save-session <job>` in its own process group, so the
//! save outlives agent-mux. That process runs the harness headless with
//! the `[memory] save` prompt, reads the `mux-note` blocks of its answer
//! and writes the notes itself: the harness needs no write access and
//! the notes are validated like any other. A conversation whose
//! transcript has not grown since its last save is skipped.
//!
//! How each harness answers without changing the conversation, probed
//! 2026-10-11:
//!
//! - Claude Code 2.1.296: `claude --resume <id> --fork-session
//!   --no-session-persistence --tools "" -p <prompt>` answered from the
//!   conversation's context; no file was written and the original
//!   transcript kept its size.
//! - codex-cli 0.162.1: `codex exec --ephemeral -o <file> -c
//!   sandbox_mode="read-only" fork <id> <prompt>` answered from the
//!   context and wrote no rollout; `exec resume --ephemeral` still
//!   appended to the original, so fork it is.
//! - Antigravity 1.3.3 has no fork: `agy -p <prompt> --mode plan` in a
//!   fresh conversation read the transcript agent-mux rendered to a file
//!   (`handoff::render`) and answered from it.

use super::{fence, index, log, note, paths};
use crate::handoff::{self, Source};
use crate::harness::Harness;
use crate::prompts::Prompts;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where jobs, their output and the per-conversation marks live.
pub const SAVES: &str = ".saves";

/// The most notes one save writes.
pub const MAX_NOTES: usize = 5;

/// A save that runs longer is stopped.
pub const TIMEOUT: Duration = Duration::from_secs(600);

/// One save, as the App hands it to the background process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    /// The profile's program (`claude`, a path to `codex`, …).
    pub command: String,
    /// The profile's `--model`, when it sets one.
    pub model: Option<String>,
    pub workspace: PathBuf,
    /// The harness's conversation id.
    pub conversation: String,
    /// The harness's transcript file: what tells a conversation that grew
    /// from one that did not, and what Antigravity reads.
    pub transcript: Option<PathBuf>,
    pub memory: PathBuf,
    pub user_dir: Option<PathBuf>,
    /// The agent-mux launch id, a note's `source.launch`.
    pub launch: Option<String>,
    pub index_lines: usize,
}

impl Job {
    pub fn harness(&self) -> Option<Harness> {
        Harness::detect(&self.command)
    }

    /// The mark that records the transcript's size at the last save.
    fn mark(&self) -> PathBuf {
        let h = self.harness().map(|h| h.as_str()).unwrap_or("unknown");
        self.memory
            .join(SAVES)
            .join(format!("{h}-{}.saved", note::slug(&self.conversation)))
    }

    fn transcript_len(&self) -> Option<u64> {
        let t = self.transcript.as_ref()?;
        std::fs::metadata(t).ok().map(|m| m.len())
    }

    /// Whether the conversation has not grown since its last save.
    pub fn unchanged(&self) -> bool {
        let Some(now) = self.transcript_len() else {
            return false;
        };
        std::fs::read_to_string(self.mark())
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            == Some(now)
    }

    fn record(&self) {
        if let Some(len) = self.transcript_len() {
            let _ = paths::write_atomic(&self.mark(), &format!("{len}\n"));
        }
    }

    fn file_stem(&self) -> String {
        let h = self.harness().map(|h| h.as_str()).unwrap_or("unknown");
        format!(
            "{h}-{}",
            note::slug(&self.conversation)
                .chars()
                .take(36)
                .collect::<String>()
        )
    }

    /// Writes the job file; the background process reads it back.
    pub fn write(&self) -> std::io::Result<PathBuf> {
        let dir = self.memory.join(SAVES);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", self.file_stem()));
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        paths::write_atomic(&path, &text)?;
        Ok(path)
    }
}

/// The value of `--model` (or `-m`) in a profile's arguments.
pub fn model_arg(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--model" || a == "-m" {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix("--model=") {
            return Some(v.to_string());
        }
    }
    None
}

/// What the save reads: the conversation itself, or a transcript file.
fn source_words(transcript: Option<&Path>) -> String {
    match transcript {
        None => "this conversation".into(),
        Some(t) => format!(
            "the conversation in the transcript at {} (read it first; it is the session that closed, not this one)",
            t.display()
        ),
    }
}

/// The memory as it stands, fenced: the workspace's index and the user's.
fn memory_text(job: &Job, prompts: &Prompts) -> String {
    let read = |d: &Path| std::fs::read_to_string(paths::index_path(d)).ok();
    let mut body = format!("Workspace memory, {}:\n\n", job.memory.display());
    body.push_str(
        &read(&job.memory).unwrap_or_else(|| format!("{}\n\nNo notes yet.\n", index::TITLE)),
    );
    if let Some(u) = job.user_dir.as_deref()
        && !note::files(u).is_empty()
        && let Some(text) = read(u)
    {
        body.push_str(&format!("\nWhat holds in every workspace:\n\n{text}"));
    }
    fence::fence(&prompts.memory_notice, &body)
}

/// The `[memory] save` prompt for this job.
pub fn prompt(job: &Job, transcript_md: Option<&Path>, prompts: &Prompts) -> String {
    prompts
        .memory_save
        .replace("{source}", &source_words(transcript_md))
        .replace("{max}", &MAX_NOTES.to_string())
        .replace("{memory}", &memory_text(job, prompts))
}

/// The program and arguments of the save run. `out` is where Codex
/// writes its last message; the other harnesses answer on stdout.
pub fn command(job: &Job, prompt: &str, out: &Path) -> Option<(String, Vec<String>)> {
    let mut args: Vec<String> = Vec::new();
    match job.harness()? {
        Harness::Claude => {
            args.extend(
                [
                    "--resume",
                    &job.conversation,
                    "--fork-session",
                    "--no-session-persistence",
                    "--tools",
                    "",
                ]
                .map(String::from),
            );
            if let Some(m) = &job.model {
                args.extend(["--model".into(), m.clone()]);
            }
            args.extend(["-p".into(), prompt.to_string()]);
        }
        Harness::Codex => {
            args.extend(
                [
                    "exec",
                    "--skip-git-repo-check",
                    "--ephemeral",
                    "-c",
                    "sandbox_mode=\"read-only\"",
                    "-o",
                ]
                .map(String::from),
            );
            args.push(out.to_string_lossy().into_owned());
            if let Some(m) = &job.model {
                args.extend(["--model".into(), m.clone()]);
            }
            args.extend(["fork".into(), job.conversation.clone(), prompt.to_string()]);
        }
        Harness::Antigravity => {
            args.extend(["-p".into(), prompt.to_string()]);
            args.extend(["--mode".into(), "plan".into()]);
            if let Some(m) = &job.model {
                args.extend(["--model".into(), m.clone()]);
            }
        }
    }
    Some((job.command.clone(), args))
}

/// The bodies of the ```` ```mux-note ```` blocks in `text`. A block
/// closes on a fence of at least its opening's length.
pub fn note_blocks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut open: Option<(usize, String)> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let ticks = trimmed.chars().take_while(|c| *c == '`').count();
        match &mut open {
            None => {
                if ticks >= 3 && trimmed[ticks..].trim() == "mux-note" {
                    open = Some((ticks, String::new()));
                }
            }
            Some((n, body)) => {
                if ticks >= *n && trimmed[ticks..].trim().is_empty() {
                    out.push(std::mem::take(body));
                    open = None;
                } else {
                    body.push_str(line);
                    body.push('\n');
                }
            }
        }
    }
    out
}

/// What one save did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub saved: Vec<String>,
    pub rejected: Vec<String>,
}

/// Writes the notes of `answer` into the job's memory, at most
/// `MAX_NOTES`, with the job's source; rebuilds the index.
pub fn apply(job: &Job, answer: &str, prompts: &Prompts) -> Outcome {
    let harness = job
        .harness()
        .map(|h| h.as_str())
        .unwrap_or("unknown")
        .to_string();
    let mut outcome = Outcome::default();
    for block in note_blocks(answer).into_iter() {
        if outcome.saved.len() >= MAX_NOTES {
            outcome
                .rejected
                .push(format!("more than {MAX_NOTES} notes"));
            break;
        }
        let mut n = match note::parse(&block, "") {
            Ok(n) => n,
            Err(e) => {
                outcome.rejected.push(e);
                continue;
            }
        };
        let existing = note::read(&job.memory, &n.name).ok();
        n.source = note::Source {
            harness: harness.clone(),
            launch: job.launch.clone(),
            session: Some(job.conversation.clone()),
            when: Some(super::now_rfc3339()),
        };
        if n.confidence == note::Confidence::Imported {
            n.confidence = note::Confidence::Observed;
        }
        // a pin is the user's: an update keeps it
        if let Some(e) = &existing {
            n.pinned |= e.pinned;
        }
        match note::write(&job.memory, &n) {
            Ok(_) => {
                log::append(
                    &job.memory,
                    &harness,
                    job.launch.as_deref(),
                    if existing.is_some() {
                        "update-on-close"
                    } else {
                        "save-on-close"
                    },
                    &format!("notes/{}.md", n.name),
                );
                outcome.saved.push(n.name);
            }
            Err(e) => outcome.rejected.push(format!("{}: {e}", n.name)),
        }
    }
    if !outcome.saved.is_empty() {
        let _ = index::rebuild(&job.memory, job.index_lines, &prompts.memory_index_footer);
    }
    outcome
}

/// Runs `program` with a deadline, its stdout into `stdout`.
fn run_with_timeout(
    job: &Job,
    program: &str,
    args: &[String],
    stdout: &Path,
) -> Result<std::process::ExitStatus, String> {
    let out = std::fs::File::create(stdout).map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .current_dir(&job.workspace)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(std::process::Stdio::null());
    // the save is not a session with memory: no start text, no second save
    for k in [
        super::ENV_DIR,
        super::ENV_INDEX,
        super::ENV_USER,
        super::ENV_BRIEF,
        "AGENT_MUX_SESSION_ID",
    ] {
        cmd.env_remove(k);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{program}: {e}"))?;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("stopped after {} s", TIMEOUT.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// `agent-mux memory save-session <job>`: the whole save, logged to the
/// memory's `log.md`. The job file is removed when it is done.
pub fn run(job_path: &Path) -> anyhow::Result<()> {
    let job: Job = serde_json::from_str(&std::fs::read_to_string(job_path)?)?;
    let result = run_job(&job, job_path);
    let _ = std::fs::remove_file(job_path);
    let harness = job.harness().map(|h| h.as_str()).unwrap_or("unknown");
    let launch = job.launch.as_deref();
    match result {
        Ok(Some(o)) => {
            let what = if o.saved.is_empty() {
                "nothing to save".to_string()
            } else {
                format!("{} notes", o.saved.len())
            };
            log::append(&job.memory, harness, launch, "closed", &what);
            for r in o.rejected {
                log::append(&job.memory, harness, launch, "save-rejected", &r);
            }
            Ok(())
        }
        Ok(None) => {
            log::append(&job.memory, harness, launch, "closed", "no new turns");
            Ok(())
        }
        Err(e) => {
            log::append(&job.memory, harness, launch, "save-failed", &e);
            anyhow::bail!(e)
        }
    }
}

fn run_job(job: &Job, job_path: &Path) -> Result<Option<Outcome>, String> {
    if job.unchanged() {
        return Ok(None);
    }
    let harness = job.harness().ok_or("not a harness agent-mux knows")?;
    let prompts = Prompts::load(&crate::assets::root());
    let stem = job_path.with_extension("");
    // Antigravity cannot fork: it reads the transcript from a file
    let transcript_md = if harness == Harness::Antigravity {
        let source = Source {
            provider: crate::transcript::Provider::Antigravity,
            conversation: job.conversation.clone(),
            transcript: job.transcript.clone(),
        };
        let text = handoff::source_text(&source)?;
        let path = stem.with_extension("md");
        paths::write_atomic(&path, &handoff::render(source.provider, &text))
            .map_err(|e| e.to_string())?;
        Some(path)
    } else {
        None
    };
    let prompt = prompt(job, transcript_md.as_deref(), &prompts);
    let last = stem.with_extension("last.txt");
    let stdout = stem.with_extension("out.txt");
    let (program, args) = command(job, &prompt, &last).ok_or("not a harness agent-mux knows")?;
    let status = run_with_timeout(job, &program, &args, &stdout);
    let answer = std::fs::read_to_string(if harness == Harness::Codex {
        &last
    } else {
        &stdout
    })
    .unwrap_or_default();
    for f in [Some(&last), Some(&stdout), transcript_md.as_ref()]
        .into_iter()
        .flatten()
    {
        let _ = std::fs::remove_file(f);
    }
    let status = status?;
    if !status.success() && note_blocks(&answer).is_empty() {
        return Err(format!("{program} exited with {status}"));
    }
    let outcome = apply(job, &answer, &prompts);
    job.record();
    Ok(Some(outcome))
}

/// Starts the save in its own process group, so neither agent-mux
/// quitting nor the terminal's signals stop it.
pub fn spawn(exe: &Path, job_path: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    std::process::Command::new(exe)
        .args(["memory", "save-session"])
        .arg(job_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(dir: &Path, command: &str) -> Job {
        Job {
            command: command.into(),
            model: None,
            workspace: dir.to_path_buf(),
            conversation: "abc-123".into(),
            transcript: None,
            memory: dir.join("mem"),
            user_dir: None,
            launch: Some("l1".into()),
            index_lines: 60,
        }
    }

    #[test]
    fn blocks_are_read_with_their_own_fence_length() {
        let text = "intro\n```mux-note\n---\nname: a\n---\nbody\n```\nmid\n````mux-note\nx\n```\nstill\n````\n```rust\nno\n```\n";
        let b = note_blocks(text);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0], "---\nname: a\n---\nbody\n");
        assert_eq!(b[1], "x\n```\nstill\n");
        assert!(note_blocks("nothing to save").is_empty());
    }

    #[test]
    fn each_harness_answers_without_changing_the_conversation() {
        let d = tempfile::tempdir().unwrap();
        let out = Path::new("/o.txt");
        let (p, a) = command(&job(d.path(), "/bin/claude"), "P", out).unwrap();
        assert_eq!(p, "/bin/claude");
        assert_eq!(
            a,
            [
                "--resume",
                "abc-123",
                "--fork-session",
                "--no-session-persistence",
                "--tools",
                "",
                "-p",
                "P"
            ]
        );
        let mut j = job(d.path(), "codex");
        j.model = Some("gpt-5.6".into());
        let (_, a) = command(&j, "P", out).unwrap();
        assert_eq!(
            a,
            [
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "-c",
                "sandbox_mode=\"read-only\"",
                "-o",
                "/o.txt",
                "--model",
                "gpt-5.6",
                "fork",
                "abc-123",
                "P"
            ]
        );
        let (_, a) = command(&job(d.path(), "agy"), "P", out).unwrap();
        assert_eq!(a, ["-p", "P", "--mode", "plan"]);
        assert!(command(&job(d.path(), "bash"), "P", out).is_none());
    }

    #[test]
    fn model_comes_from_the_profile_args() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(model_arg(&a(&["--model", "opus"])), Some("opus".into()));
        assert_eq!(model_arg(&a(&["-x", "--model=o3"])), Some("o3".into()));
        assert_eq!(model_arg(&a(&["--yolo"])), None);
    }

    #[test]
    fn notes_are_written_with_the_job_as_source_and_pins_kept() {
        let d = tempfile::tempdir().unwrap();
        let j = job(d.path(), "claude");
        paths::ensure(&j.memory).unwrap();
        let prompts = Prompts::builtin();
        // a pinned note the save updates keeps its pin
        let pinned = "---\nname: rule\ndescription: old\nkind: feedback\nconfidence: stated\npinned: true\n---\n\nold\n";
        std::fs::write(paths::note_path(&j.memory, "rule"), pinned).unwrap();
        let answer = "Saved:\n```mux-note\n---\nname: rule\ndescription: new\nkind: feedback\nconfidence: stated\n---\n\nnew body\n```\n```mux-note\n---\nname: Bad Name\ndescription: x\n---\nx\n```\n```mux-note\n---\nname: state\ndescription: where the work is\nkind: project\nconfidence: imported\n---\n\nhalf done\n```\n";
        let o = apply(&j, answer, &prompts);
        assert_eq!(o.saved, ["rule", "state"]);
        assert_eq!(o.rejected.len(), 1, "{:?}", o.rejected);
        let rule = note::read(&j.memory, "rule").unwrap();
        assert!(rule.pinned && rule.description == "new");
        assert_eq!(rule.source.harness, "claude");
        assert_eq!(rule.source.launch.as_deref(), Some("l1"));
        assert_eq!(rule.source.session.as_deref(), Some("abc-123"));
        let state = note::read(&j.memory, "state").unwrap();
        assert_eq!(state.confidence, note::Confidence::Observed);
        let index = std::fs::read_to_string(paths::index_path(&j.memory)).unwrap();
        assert!(
            index.contains("[state]") && index.contains("[rule]"),
            "{index}"
        );
        let log = std::fs::read_to_string(j.memory.join(paths::LOG)).unwrap();
        assert!(log.contains("update-on-close notes/rule.md"), "{log}");
        assert!(log.contains("save-on-close notes/state.md"), "{log}");
    }

    #[test]
    fn at_most_max_notes_are_saved() {
        let d = tempfile::tempdir().unwrap();
        let j = job(d.path(), "claude");
        paths::ensure(&j.memory).unwrap();
        let answer: String = (0..MAX_NOTES + 2)
            .map(|i| format!("```mux-note\n---\nname: n{i}\ndescription: d\n---\nb\n```\n"))
            .collect();
        let o = apply(&j, &answer, &Prompts::builtin());
        assert_eq!(o.saved.len(), MAX_NOTES);
    }

    #[test]
    fn an_unchanged_transcript_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        let mut j = job(d.path(), "claude");
        paths::ensure(&j.memory).unwrap();
        let t = d.path().join("t.jsonl");
        std::fs::write(&t, "one\n").unwrap();
        j.transcript = Some(t.clone());
        assert!(!j.unchanged(), "never saved");
        j.record();
        assert!(j.unchanged());
        std::fs::write(&t, "one\ntwo\n").unwrap();
        assert!(!j.unchanged(), "a new turn");
    }

    #[test]
    fn the_prompt_carries_the_memory_and_the_source() {
        let d = tempfile::tempdir().unwrap();
        let j = job(d.path(), "claude");
        paths::ensure(&j.memory).unwrap();
        let p = prompt(&j, None, &Prompts::builtin());
        assert!(p.contains("this conversation"));
        assert!(p.contains(fence::START) && p.contains("No notes yet."));
        assert!(p.contains("at most 5 notes"));
        assert!(!p.contains("{memory}") && !p.contains("{source}"));
        let p = prompt(&j, Some(Path::new("/t.md")), &Prompts::builtin());
        assert!(p.contains("the transcript at /t.md"));
    }

    #[test]
    fn a_job_round_trips_through_its_file() {
        let d = tempfile::tempdir().unwrap();
        let j = job(d.path(), "claude");
        let path = j.write().unwrap();
        assert!(path.starts_with(j.memory.join(SAVES)));
        let back: Job = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(back, j);
    }
}
