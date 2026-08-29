use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Find a native .exe for `command`: either `command` is itself a path to an
/// existing .exe, or `<command>.exe` exists in one of `path_var`'s entries.
pub fn find_exe(command: &str, path_var: &OsStr) -> Option<PathBuf> {
    let p = Path::new(command);
    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")) && p.is_file() {
        return Some(p.to_path_buf());
    }
    if p.is_absolute() {
        return None;
    }
    for dir in std::env::split_paths(path_var) {
        let candidate = dir.join(format!("{command}.exe"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// (program, prefix_args) to hand to CommandBuilder. On Windows,
/// npm-installed CLIs are .cmd shims that ConPTY can't spawn directly, so
/// anything that isn't a native .exe goes through `cmd /c`.
#[cfg(windows)]
pub fn resolve_command(command: &str) -> (String, Vec<String>) {
    match find_exe(command, &std::env::var_os("PATH").unwrap_or_default()) {
        Some(exe) => (exe.to_string_lossy().into_owned(), vec![]),
        None => ("cmd.exe".into(), vec!["/c".into(), command.into()]),
    }
}

#[cfg(not(windows))]
pub fn resolve_command(command: &str) -> (String, Vec<String>) {
    (command.to_string(), vec![])
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    #[test]
    fn finds_exe_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("myagent.exe");
        std::fs::write(&exe, b"fake").unwrap();
        let found = find_exe("myagent", dir.path().as_os_str());
        assert_eq!(found, Some(exe));
    }

    #[test]
    fn explicit_exe_path_is_used_directly() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("tool.exe");
        std::fs::write(&exe, b"fake").unwrap();
        let cmd = exe.to_string_lossy().into_owned();
        assert_eq!(find_exe(&cmd, std::ffi::OsStr::new("")), Some(exe));
    }

    #[test]
    fn missing_command_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(find_exe("nope-nothing-here", dir.path().as_os_str()), None);
    }

    #[cfg(windows)]
    #[test]
    fn unresolved_command_falls_back_to_cmd_shim() {
        // "definitely-not-on-path-xyz" won't resolve to an .exe
        let (program, args) = resolve_command("definitely-not-on-path-xyz");
        assert_eq!(program, "cmd.exe");
        assert_eq!(args, vec!["/c".to_string(), "definitely-not-on-path-xyz".to_string()]);
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_always_direct() {
        let (program, args) = resolve_command("claude");
        assert_eq!(program, "claude");
        assert!(args.is_empty());
    }
}
