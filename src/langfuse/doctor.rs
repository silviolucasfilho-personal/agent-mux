//! `agent-mux langfuse doctor` — one-shot diagnosis of the silent-no-trace
//! failure modes: unresolved config, bad keys/host, missing CLIs or data
//! dirs, a `claude` too old for `--session-id`.

use crate::config;
use std::path::{Path, PathBuf};

fn mask_key(key: &str) -> String {
    // char-based: byte slicing would panic on multi-byte input
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 10 {
        return "***".to_string();
    }
    let head: String = chars[..6].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}...{tail}")
}

/// Mirrors execvp: an entry must be a file AND executable (unix), and a
/// non-executable early PATH hit must not shadow a later real one.
fn on_path(command: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    #[cfg(unix)]
    fn runnable(p: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    fn runnable(_p: &Path) -> bool {
        true
    }
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(command);
        if candidate.is_file() && runnable(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{command}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

fn check(label: &str, ok: bool, detail: &str) {
    let mark = if ok { "ok " } else { "!! " };
    println!("  [{mark}] {label}: {detail}");
}

fn dir_status(path: Option<&Path>) -> (bool, String) {
    match path {
        Some(p) if p.is_dir() => (true, p.display().to_string()),
        Some(p) => (false, format!("{} (missing)", p.display())),
        None => (false, "unresolvable (no HOME?)".to_string()),
    }
}

pub fn run() -> anyhow::Result<()> {
    println!("agent-mux langfuse doctor\n");
    let cfg = config::load()?;
    match &cfg.loaded_from {
        Some(path) => println!("config file: {}", path.display()),
        None => println!("config file: none found (built-in defaults; [langfuse] can only come from a file)"),
    }
    let env = |k: &str| std::env::var(k).ok();
    let Some(resolved) = config::resolve_langfuse(cfg.langfuse.as_ref(), &env) else {
        println!("\n[langfuse] is absent, disabled, or its keys don't resolve — tracing is OFF.");
        println!("Enable it in profiles.toml (see profiles.example.toml); keys fall back to");
        println!("$LANGFUSE_PUBLIC_KEY / $LANGFUSE_SECRET_KEY / $LANGFUSE_HOST.");
        return Ok(());
    };

    println!("\nresolved configuration:");
    println!("  host:         {}", resolved.host);
    println!("  public_key:   {}", mask_key(&resolved.public_key));
    println!("  secret_key:   {}", mask_key(&resolved.secret_key));
    println!("  content_mode: {:?}", resolved.content_mode);
    check(
        "public key prefix",
        resolved.public_key.starts_with("pk-lf-"),
        if resolved.public_key.starts_with("pk-lf-") {
            "pk-lf-*"
        } else {
            "does not start with pk-lf- — swapped keys?"
        },
    );
    check(
        "secret key prefix",
        resolved.secret_key.starts_with("sk-lf-"),
        if resolved.secret_key.starts_with("sk-lf-") {
            "sk-lf-*"
        } else {
            "does not start with sk-lf- — swapped keys?"
        },
    );
    if resolved.secret_from_file
        && cfg
            .loaded_from
            .as_ref()
            .is_some_and(|p| p == Path::new("profiles.toml"))
    {
        println!(
            "  [!!] secret_key sits in the cwd-relative ./profiles.toml — easy to commit.\n\
             \x20      Prefer $LANGFUSE_SECRET_KEY."
        );
    }

    println!("\nendpoint probe (empty resourceSpans batch):");
    match crate::langfuse::export::probe(&resolved.host, &resolved.public_key, &resolved.secret_key)
    {
        Ok(()) => check("POST /api/public/otel/v1/traces", true, "200 — host + auth work"),
        Err(e) => check("POST /api/public/otel/v1/traces", false, &e),
    }

    println!("\nper-provider correlation readiness:");
    // Claude
    let claude_bin = on_path("claude");
    check(
        "claude on PATH",
        claude_bin.is_some(),
        &claude_bin
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not found".into()),
    );
    if claude_bin.is_some() {
        let supports = std::process::Command::new("claude")
            .arg("--help")
            .output()
            .ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).contains("--session-id"))
            .unwrap_or(false);
        check(
            "claude supports --session-id",
            supports,
            if supports {
                "yes — deterministic correlation"
            } else {
                "NO — set [profiles.langfuse] inject_session_id = false, or update claude"
            },
        );
    }
    let claude_dir = resolved
        .claude_dir
        .clone()
        .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".claude"))
        });
    let (ok, detail) = dir_status(claude_dir.as_deref());
    check("claude data dir", ok, &detail);

    // Codex
    check(
        "codex on PATH",
        on_path("codex").is_some(),
        &on_path("codex")
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not found".into()),
    );
    let codex_sessions = resolved
        .codex_dir
        .clone()
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".codex"))
        })
        .map(|d| d.join("sessions"));
    let (ok, detail) = dir_status(codex_sessions.as_deref());
    check("codex sessions dir", ok, &detail);

    // Antigravity
    check(
        "agy on PATH",
        on_path("agy").is_some(),
        &on_path("agy")
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not found".into()),
    );
    let brain = resolved
        .antigravity_dir
        .clone()
        .or_else(crate::history::default_antigravity_root)
        .map(|d| d.join("brain"));
    let (ok, detail) = dir_status(brain.as_deref());
    check("antigravity brain dir", ok, &detail);

    println!("\nA provider marked [!!] still gets a lifecycle trace per session; content-\nbearing turn traces need its transcript machinery above to be in place.");
    Ok(())
}
