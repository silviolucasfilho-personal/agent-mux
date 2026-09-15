//! Filesystem watch over the agent roots, so the Agents sidebar refreshes on
//! its own when a package is added, edited, or removed while agent-mux runs.
//!
//! One `notify` watcher covers every root that exists. A root that does not
//! exist yet (a workspace without `.agent-mux/agents`, say) is covered by a
//! non-recursive watch on its nearest existing ancestor, and promoted to a
//! recursive watch as soon as it appears. Bursts of events are debounced into
//! a single [`AppEvent::AgentsChanged`].

use crate::events::AppEvent;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use tokio::sync::mpsc::Sender;

/// Quiet period after the last filesystem event before a reload is emitted.
pub const DEBOUNCE: Duration = Duration::from_millis(400);

/// Nearest ancestor of `path` that exists on disk, if any.
fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    path.ancestors().find(|p| p.is_dir()).map(Path::to_path_buf)
}

/// Canonicalizes the existing prefix of `path` and re-appends the missing
/// tail, so a root that does not exist yet still compares equal to the
/// canonical paths the OS watcher reports (`/var` vs `/private/var`).
fn canonical_prefix(path: &Path) -> PathBuf {
    if let Ok(c) = path.canonicalize() {
        return c;
    }
    let Some(anc) = nearest_existing_ancestor(path) else {
        return path.to_path_buf();
    };
    let base = anc.canonicalize().unwrap_or(anc.clone());
    match path.strip_prefix(&anc) {
        Ok(rest) => base.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// True when `path` lies inside a root, or is an ancestor of one (so that
/// creating the root itself is noticed).
fn touches_roots(path: &Path, roots: &[PathBuf]) -> bool {
    roots
        .iter()
        .any(|r| path.starts_with(r) || r.starts_with(path))
}

fn is_relevant(event: &notify::Event, roots: &[PathBuf]) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    event.paths.iter().any(|p| touches_roots(p, roots))
}

/// Arms a watch for `root`: recursive when it exists, otherwise on its
/// nearest existing ancestor. Returns whether the root itself is watched.
fn arm(watcher: &mut RecommendedWatcher, root: &Path) -> bool {
    if root.is_dir() {
        return watcher.watch(root, RecursiveMode::Recursive).is_ok();
    }
    if let Some(anc) = nearest_existing_ancestor(root) {
        let _ = watcher.watch(&anc, RecursiveMode::NonRecursive);
    }
    false
}

/// Starts watching `roots` and forwards debounced change notifications to
/// `tx`. The watcher lives on its own thread for the rest of the process.
pub fn spawn(roots: Vec<PathBuf>, tx: Sender<AppEvent>) -> Result<(), notify::Error> {
    let roots: Vec<PathBuf> = roots.iter().map(|r| canonical_prefix(r)).collect();
    let (ev_tx, ev_rx) = mpsc::channel::<()>();
    let filter_roots = roots.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && is_relevant(&ev, &filter_roots)
        {
            let _ = ev_tx.send(());
        }
    })?;

    let mut armed: Vec<bool> = roots.iter().map(|r| arm(&mut watcher, r)).collect();

    std::thread::Builder::new()
        .name("agent-roots-watch".into())
        .spawn(move || {
            // The watcher must outlive the loop; dropping it stops events.
            let mut watcher = watcher;
            while ev_rx.recv().is_ok() {
                // Debounce: swallow the rest of the burst.
                while ev_rx.recv_timeout(DEBOUNCE).is_ok() {}
                // Promote roots that came into existence.
                for (i, root) in roots.iter().enumerate() {
                    if !armed[i] && root.is_dir() {
                        armed[i] = arm(&mut watcher, root);
                    }
                }
                if tx.blocking_send(AppEvent::AgentsChanged).is_err() {
                    break;
                }
            }
        })
        .map_err(notify::Error::io)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestor_of_root_counts_as_touching() {
        let roots = [PathBuf::from("/tmp/x/.agent-mux/agents")];
        assert!(touches_roots(Path::new("/tmp/x/.agent-mux"), &roots));
        assert!(touches_roots(
            Path::new("/tmp/x/.agent-mux/agents/foo/AGENTS.md"),
            &roots
        ));
        assert!(!touches_roots(Path::new("/tmp/x/README.md"), &roots));
    }
}
