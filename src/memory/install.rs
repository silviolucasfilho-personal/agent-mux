//! The install flow: what is missing, as steps, and running one.
//!
//! In order, each step only when needed:
//!
//! 1. **Download**: the latest macOS release
//!    (`ai-memory-macos-<arch>.tar.gz` from GitHub's `releases/latest`),
//!    its `.sha256` checked, unpacked into `~/Applications/ai-memory`
//!    (the binary, its `hooks/` and `packaging/`). Skipped when an
//!    `ai-memory` is already installed (`find_binary`).
//! 2. **Init**: `ai-memory init`, when the data folder has no
//!    `config.toml`.
//! 3. **Server**: the launchd agent ai-memory ships
//!    (`packaging/launchd`, rendered here with the binary's path) written
//!    to `~/Library/LaunchAgents` and bootstrapped, so `ai-memory serve
//!    --transport http --enable-web` runs at login and restarts; an agent
//!    already there is kicked instead. Then a wait for the port: a fresh
//!    binary's first start is slow on macOS.
//! 4. **Wire** each harness whose CLI is installed (or the ones asked
//!    for): `ai-memory install-mcp --client <agent> --apply` and
//!    `ai-memory install-hooks --agent <agent> --apply` for what its files
//!    lack. Both are ai-memory's own, idempotent, and back up the file
//!    they change.
//!
//! Uninstall is `ai-memory uninstall --apply --yes` (hooks, MCP entries,
//! instructions and skills from every harness; `--purge-data` also wipes
//! the memory), then the launchd agent's `bootout` and file. The binary
//! stays.

use super::{State, agent_name, data_dir, install_dir, plist_path};
use crate::harness::Harness;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const RELEASES: &str = "https://github.com/akitaonrails/ai-memory/releases/latest/download";

/// How long a started server has to answer.
const SERVER_WAIT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Download,
    Init,
    /// Write the launchd agent and bootstrap it.
    Launchd,
    /// The agent exists: start it again.
    Kickstart,
    WaitServer,
    Mcp(Harness),
    Hooks(Harness),
    Uninstall {
        purge: bool,
    },
    RemoveLaunchd,
}

/// One step: what it does, and the command it runs, for the user to see
/// before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub kind: Kind,
    pub title: String,
    pub command: String,
    /// The ai-memory binary the step runs.
    pub bin: PathBuf,
}

/// What to wire.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// These harnesses only; `None`: every one whose CLI is installed.
    pub only: Option<Vec<Harness>>,
}

/// A path as a step shows it: under `~`, quoted only when it must be.
pub fn shown(p: &Path) -> String {
    quote(p)
}

fn quote(p: &Path) -> String {
    let text = p.to_string_lossy();
    let home = super::home();
    let text = match p.strip_prefix(&home) {
        Ok(rest) if home != Path::new("/") => format!("~/{}", rest.display()),
        _ => text.into_owned(),
    };
    if text.contains(|c: char| c.is_whitespace() || "'\"$`\\".contains(c)) {
        crate::tracing::hooks::install::sh_quote(&text)
    } else {
        text
    }
}

/// The release asset for this machine; `None` off macOS.
pub fn asset() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        _ => return None,
    };
    Some(format!("ai-memory-macos-{arch}.tar.gz"))
}

/// The steps that take `state` to a wired machine.
pub fn plan(state: &State, home: &Path, opts: &Options) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    let bin = match &state.binary {
        Some(b) => b.path.clone(),
        None => {
            let asset = asset().ok_or(
                "agent-mux downloads ai-memory on macOS only; install it from https://github.com/akitaonrails/ai-memory#install and run this again",
            )?;
            let dir = install_dir(home);
            steps.push(Step {
                kind: Kind::Download,
                title: format!("Download ai-memory into {}", dir.display()),
                command: format!(
                    "download {RELEASES}/{asset}, check its .sha256, unpack into {}",
                    quote(&dir)
                ),
                bin: dir.join("ai-memory"),
            });
            dir.join("ai-memory")
        }
    };
    let step = |kind: Kind, title: String, command: String| Step {
        kind,
        title,
        command,
        bin: bin.clone(),
    };
    if !state.initialized {
        steps.push(step(
            Kind::Init,
            format!("Create the data folder {}", data_dir(home).display()),
            format!("{} init", quote(&bin)),
        ));
    }
    if !state.server {
        if state.launchd {
            steps.push(step(
                Kind::Kickstart,
                "Start the ai-memory server".into(),
                format!(
                    "launchctl kickstart -k gui/$(id -u)/{}",
                    super::LAUNCHD_LABEL
                ),
            ));
        } else {
            let plist = plist_path(home);
            steps.push(step(
                Kind::Launchd,
                "Run the ai-memory server at login (launchd)".into(),
                format!(
                    "write {} && launchctl bootstrap gui/$(id -u) {}",
                    quote(&plist),
                    quote(&plist)
                ),
            ));
        }
        steps.push(step(
            Kind::WaitServer,
            format!("Wait for the server on {}", super::server_addr()),
            String::new(),
        ));
    }
    let wanted: Vec<Harness> = match &opts.only {
        Some(list) => list.clone(),
        None => state
            .harnesses
            .iter()
            .filter(|w| w.cli)
            .map(|w| w.harness)
            .collect(),
    };
    for h in wanted {
        let Some(w) = state.wiring(h) else { continue };
        let name = agent_name(h);
        if !w.mcp {
            steps.push(step(
                Kind::Mcp(h),
                format!("Register ai-memory's MCP server with {}", super::label(h)),
                format!("{} install-mcp --client {name} --apply", quote(&bin)),
            ));
        }
        if !w.hooks {
            steps.push(step(
                Kind::Hooks(h),
                format!("Install ai-memory's hooks into {}", super::label(h)),
                format!("{} install-hooks --agent {name} --apply", quote(&bin)),
            ));
        }
    }
    Ok(steps)
}

/// The steps that take ai-memory's wiring and server away.
pub fn plan_uninstall(state: &State, home: &Path, purge: bool) -> Vec<Step> {
    let mut steps = Vec::new();
    if let Some(b) = &state.binary {
        steps.push(Step {
            kind: Kind::Uninstall { purge },
            title: if purge {
                "Remove ai-memory from every harness and delete its memory".into()
            } else {
                "Remove ai-memory's hooks, MCP entries and skills from every harness".into()
            },
            command: format!(
                "{} uninstall --apply --yes --mcp-url {}{}",
                quote(&b.path),
                mcp_url(),
                if purge { " --purge-data" } else { "" }
            ),
            bin: b.path.clone(),
        });
    }
    if state.launchd {
        steps.push(Step {
            kind: Kind::RemoveLaunchd,
            title: "Stop the server and remove its launchd agent".into(),
            command: format!(
                "launchctl bootout gui/$(id -u)/{} && rm {}",
                super::LAUNCHD_LABEL,
                quote(&plist_path(home))
            ),
            bin: state
                .binary
                .as_ref()
                .map(|b| b.path.clone())
                .unwrap_or_default(),
        });
    }
    steps
}

/// The launchd agent for `bin`: ai-memory's `packaging/launchd` file with
/// its two placeholders filled.
pub fn plist(bin: &Path, home: &Path) -> String {
    let esc = |s: String| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let bin = esc(bin.to_string_lossy().into_owned());
    let home = esc(home.to_string_lossy().into_owned());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by agent-mux from ai-memory's packaging/launchd agent. -->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{bin}</string>
    <string>serve</string>
    <string>--transport</string>
    <string>http</string>
    <string>--enable-web</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>StandardOutPath</key>
  <string>{home}/Library/Logs/ai-memory/stdout.log</string>
  <key>StandardErrorPath</key>
  <string>{home}/Library/Logs/ai-memory/stderr.log</string>
</dict>
</plist>
"#,
        label = super::LAUNCHD_LABEL
    )
}

/// Runs one step, telling `log` what happens.
pub fn run(step: &Step, home: &Path, log: &mut dyn FnMut(String)) -> Result<(), String> {
    match &step.kind {
        Kind::Download => download(&install_dir(home), log),
        Kind::Init => command(&step.bin, &["init"], log),
        Kind::Launchd => {
            std::fs::create_dir_all(home.join("Library/Logs/ai-memory"))
                .map_err(|e| e.to_string())?;
            let path = plist_path(home);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(&path, plist(&step.bin, home))
                .map_err(|e| format!("{}: {e}", path.display()))?;
            log(format!("wrote {}", path.display()));
            let target = format!("gui/{}", uid()?);
            launchctl(&["bootstrap", &target, &path.to_string_lossy()], log)
        }
        Kind::Kickstart => {
            let service = format!("gui/{}/{}", uid()?, super::LAUNCHD_LABEL);
            if launchctl(&["kickstart", "-k", &service], log).is_ok() {
                return Ok(());
            }
            // written but never loaded (or unloaded since): load it
            let target = format!("gui/{}", uid()?);
            launchctl(
                &["bootstrap", &target, &plist_path(home).to_string_lossy()],
                log,
            )
        }
        Kind::WaitServer => {
            let addr = super::server_addr();
            let start = Instant::now();
            while start.elapsed() < SERVER_WAIT {
                if super::server_up(&addr, Duration::from_millis(500)) {
                    log(format!("the server answers on {addr}"));
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(format!(
                "nothing answers on {addr} after {} s; see ~/Library/Logs/ai-memory/stderr.log",
                SERVER_WAIT.as_secs()
            ))
        }
        Kind::Mcp(h) => command(
            &step.bin,
            &["install-mcp", "--client", agent_name(*h), "--apply"],
            log,
        ),
        Kind::Hooks(h) => command(
            &step.bin,
            &["install-hooks", "--agent", agent_name(*h), "--apply"],
            log,
        ),
        Kind::Uninstall { purge } => {
            // ai-memory knows its MCP entries by their URL: the server's
            let url = mcp_url();
            let mut args = vec!["uninstall", "--apply", "--yes", "--mcp-url", &url];
            if *purge {
                args.push("--purge-data");
            }
            command(&step.bin, &args, log)
        }
        Kind::RemoveLaunchd => {
            let service = format!("gui/{}/{}", uid()?, super::LAUNCHD_LABEL);
            // not loaded is fine: the file goes either way
            let _ = launchctl(&["bootout", &service], log);
            let path = plist_path(home);
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            log(format!("removed {}", path.display()));
            Ok(())
        }
    }
}

/// The MCP endpoint the harnesses were given.
fn mcp_url() -> String {
    format!("http://{}/mcp", super::server_addr())
}

fn uid() -> Result<String, String> {
    let out = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| format!("id -u: {e}"))?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() {
        return Err("id -u printed nothing".into());
    }
    Ok(uid)
}

fn launchctl(args: &[&str], log: &mut dyn FnMut(String)) -> Result<(), String> {
    command(Path::new("launchctl"), args, log)
}

/// A line of a command's output as the log shows it: colours gone, the
/// home folder as `~`; `None` for a blank line or ai-memory's own
/// `INFO`/`DEBUG` logging.
pub fn tidy(line: &str) -> Option<String> {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        plain.push(c);
    }
    let t = plain.trim();
    if t.is_empty() {
        return None;
    }
    let mut words = t.split_whitespace();
    if words
        .next()
        .is_some_and(|w| w.len() > 18 && w.as_bytes()[4] == b'-' && w.contains('T'))
        && matches!(words.next(), Some("INFO" | "DEBUG" | "TRACE"))
    {
        return None;
    }
    let home = super::home();
    let home = home.to_string_lossy();
    Some(if home.len() > 1 {
        plain.trim_end().replace(home.as_ref(), "~")
    } else {
        plain.trim_end().to_string()
    })
}

/// Runs a program, its output line by line into `log`.
fn command(program: &Path, args: &[&str], log: &mut dyn FnMut(String)) -> Result<(), String> {
    log(format!("$ {} {}", shown(program), args.join(" ")));
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    for line in String::from_utf8_lossy(&out.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&out.stderr).lines())
    {
        if let Some(line) = tidy(line) {
            log(format!("  {line}"));
        }
    }
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} {} failed ({})",
            program
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            args.first().copied().unwrap_or(""),
            out.status
        ))
    }
}

/// Downloads the release into `dir`, checks it against its `.sha256`, and
/// unpacks it there.
fn download(dir: &Path, log: &mut dyn FnMut(String)) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let asset = asset().ok_or("no ai-memory release for this machine")?;
    let agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(300)))
        .build()
        .new_agent();
    let fetch = |url: &str| -> Result<Vec<u8>, String> {
        let mut resp = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
        resp.body_mut()
            .with_config()
            .limit(512 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| format!("{url}: {e}"))
    };
    let url = format!("{RELEASES}/{asset}");
    log(format!("downloading {url}"));
    let bytes = fetch(&url)?;
    let sum = String::from_utf8_lossy(&fetch(&format!("{url}.sha256"))?).into_owned();
    let expected = sum.split_whitespace().next().unwrap_or("").to_lowercase();
    let actual = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if expected.len() != 64 || expected != actual {
        return Err(format!(
            "{asset}: checksum mismatch (expected {expected}, got {actual})"
        ));
    }
    log(format!("{asset}: {} bytes, sha256 ok", bytes.len()));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let archive = dir.join(&asset);
    std::fs::write(&archive, &bytes).map_err(|e| format!("{}: {e}", archive.display()))?;
    let unpacked = command(
        Path::new("tar"),
        &[
            "-xzf",
            &archive.to_string_lossy(),
            "-C",
            &dir.to_string_lossy(),
        ],
        log,
    );
    let _ = std::fs::remove_file(&archive);
    unpacked?;
    if !dir.join("ai-memory").is_file() {
        return Err(format!("{asset} had no ai-memory binary"));
    }
    log(format!("installed {}", dir.join("ai-memory").display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Binary, Wiring};

    fn state(binary: bool, server: bool, launchd: bool) -> State {
        State {
            binary: binary.then(|| Binary {
                path: PathBuf::from("/opt/ai-memory"),
                version: Some("ai-memory 2.6.7".into()),
            }),
            initialized: binary,
            server,
            launchd,
            feed: true,
            harnesses: vec![
                Wiring {
                    harness: Harness::Claude,
                    cli: true,
                    mcp: true,
                    hooks: false,
                },
                Wiring {
                    harness: Harness::Codex,
                    cli: true,
                    mcp: false,
                    hooks: false,
                },
                Wiring {
                    harness: Harness::Antigravity,
                    cli: false,
                    mcp: false,
                    hooks: false,
                },
            ],
        }
    }

    fn kinds(steps: &[Step]) -> Vec<Kind> {
        steps.iter().map(|s| s.kind.clone()).collect()
    }

    #[test]
    fn a_running_install_only_wires_what_the_installed_harnesses_lack() {
        let home = Path::new("/Users/u");
        let steps = plan(&state(true, true, true), home, &Options::default()).unwrap();
        assert_eq!(
            kinds(&steps),
            vec![
                Kind::Hooks(Harness::Claude),
                Kind::Mcp(Harness::Codex),
                Kind::Hooks(Harness::Codex)
            ]
        );
        assert_eq!(
            steps[0].command,
            "/opt/ai-memory install-hooks --agent claude-code --apply"
        );
        assert_eq!(
            steps[1].command,
            "/opt/ai-memory install-mcp --client codex --apply"
        );
        // asked for by name, a harness without its CLI is wired too
        let only = Options {
            only: Some(vec![Harness::Antigravity]),
        };
        let steps = plan(&state(true, true, true), home, &only).unwrap();
        assert_eq!(
            kinds(&steps),
            vec![
                Kind::Mcp(Harness::Antigravity),
                Kind::Hooks(Harness::Antigravity)
            ]
        );
        assert!(steps[0].command.contains("--client antigravity-cli"));
    }

    #[test]
    fn a_stopped_server_is_started_and_waited_for() {
        let home = Path::new("/Users/u");
        let none = Options { only: Some(vec![]) };
        let fresh = plan(&state(true, false, false), home, &none).unwrap();
        assert_eq!(kinds(&fresh), vec![Kind::Launchd, Kind::WaitServer]);
        let known = plan(&state(true, false, true), home, &none).unwrap();
        assert_eq!(kinds(&known), vec![Kind::Kickstart, Kind::WaitServer]);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn nothing_installed_starts_with_the_download() {
        let home = Path::new("/Users/u");
        let none = Options { only: Some(vec![]) };
        let steps = plan(&state(false, false, false), home, &none).unwrap();
        assert_eq!(
            kinds(&steps),
            vec![Kind::Download, Kind::Init, Kind::Launchd, Kind::WaitServer]
        );
        assert_eq!(
            steps[1].bin,
            PathBuf::from("/Users/u/Applications/ai-memory/ai-memory"),
            "later steps run the downloaded binary"
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn elsewhere_nothing_installed_asks_for_a_manual_install() {
        let none = Options { only: Some(vec![]) };
        let e = plan(&state(false, false, false), Path::new("/home/u"), &none).unwrap_err();
        assert!(e.contains("macOS only"));
    }

    #[test]
    fn uninstall_removes_the_wiring_then_the_server() {
        let home = Path::new("/Users/u");
        let steps = plan_uninstall(&state(true, true, true), home, true);
        assert_eq!(
            kinds(&steps),
            vec![Kind::Uninstall { purge: true }, Kind::RemoveLaunchd]
        );
        assert_eq!(
            steps[0].command,
            format!(
                "/opt/ai-memory uninstall --apply --yes --mcp-url http://{}/mcp --purge-data",
                crate::memory::server_addr()
            )
        );
        assert!(plan_uninstall(&state(false, false, false), home, false).is_empty());
    }

    /// Downloads the real release: `cargo test -- --ignored downloads`.
    #[test]
    #[ignore]
    fn downloads_and_unpacks_the_latest_release() {
        let d = tempfile::tempdir().unwrap();
        let mut log = Vec::new();
        download(d.path(), &mut |l| log.push(l)).unwrap();
        assert!(d.path().join("ai-memory").is_file(), "{log:?}");
        assert!(d.path().join("hooks/claude-code").is_dir());
        assert!(log.iter().any(|l| l.contains("sha256 ok")));
    }

    #[test]
    fn output_lines_lose_colours_and_logging() {
        assert_eq!(
            tidy(
                "\u{1b}[2m2026-10-13T21:33:21.105946Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m ai_memory_cli: starting"
            ),
            None
        );
        assert_eq!(
            tidy("2026-10-13T21:33:21Z  WARN slow").as_deref(),
            Some("2026-10-13T21:33:21Z  WARN slow")
        );
        assert_eq!(tidy("   ").as_deref(), None);
        assert_eq!(
            tidy("  \u{1b}[32m✓\u{1b}[0m created x").as_deref(),
            Some("  ✓ created x")
        );
    }

    #[test]
    fn the_launchd_agent_runs_the_binary_with_logs_under_home() {
        let p = plist(
            Path::new("/Users/u/Applications/ai-memory/ai-memory"),
            Path::new("/Users/u"),
        );
        assert!(p.contains("<string>/Users/u/Applications/ai-memory/ai-memory</string>"));
        assert!(p.contains("<string>com.github.akitaonrails.ai-memory</string>"));
        assert!(p.contains("<string>/Users/u/Library/Logs/ai-memory/stderr.log</string>"));
        assert!(p.contains("<string>--enable-web</string>"));
    }
}
