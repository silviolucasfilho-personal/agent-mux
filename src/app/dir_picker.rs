//! The directory picker shared by the New session dialog (Directory) and
//! the Add/Edit loop dialog (Workspace): a typed path, the list of its
//! subfolders, and a search over nested subfolders.
//!
//! The picker does not own the path string; both dialogs keep theirs and
//! pass it in, so their other fields and tests stay untouched. It owns the
//! subfolder rows, which row the cursor is on and the search query.
//!
//! Keys, while the dialog's field is this picker:
//!
//! | Key | Cursor on the path | Cursor in the list |
//! | --- | --- | --- |
//! | `↓` | into the list | next row |
//! | `↑` | (not handled: the dialog decides) | previous row, past the top back to the path |
//! | `→` | into the list | open the row (`..` = parent) |
//! | `←` | parent directory | parent directory |
//! | `Enter` | submit the dialog | open the row, then submit (`..` only goes up) |
//! | typing | edits the path | searches subfolders (nested, up to [`SEARCH_DEPTH`]) |
//! | `Backspace` | edits the path | shortens the search |
//! | `Esc` | (not handled) | clears the search when one is set |

use crossterm::event::{KeyCode, KeyEvent};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// How deep a search looks below the current directory.
pub const SEARCH_DEPTH: usize = 3;
/// Rows a search returns at most.
pub const SEARCH_MAX_RESULTS: usize = 200;
/// Directories a search visits at most, so `/` or `~` stays quick.
const SEARCH_MAX_VISITED: usize = 4000;
/// Directory names a search never descends into (hidden names are also skipped).
const SEARCH_SKIP: &[&str] = &["node_modules", "target", "__pycache__", "venv", ".venv"];

/// What a key did to the picker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerEvent {
    /// Not a picker key: the dialog handles it.
    Ignored,
    /// Handled; `path_changed` is true when the path string was edited
    /// or navigated (the dialog re-validates it).
    Consumed { path_changed: bool },
    /// `Enter`: the dialog submits (the path may have changed first).
    Submit,
}

#[derive(Debug, Clone, Default)]
pub struct DirPicker {
    /// Subdirectories of the path: `..` when it has a parent, then names.
    entries: Vec<String>,
    /// The rows shown: `entries` while the query is empty, otherwise the
    /// search matches as paths relative to the current directory (`a/b`).
    rows: Vec<String>,
    /// `None` = cursor on the path text; `Some` = cursor in the list.
    /// With a query that matches nothing it is `Some(0)` over no rows.
    pub selected: Option<usize>,
    pub query: String,
}

impl DirPicker {
    pub fn for_path(path: &str) -> Self {
        let mut p = Self::default();
        p.refresh(path);
        p
    }

    /// The rows to draw.
    pub fn rows(&self) -> &[String] {
        &self.rows
    }

    /// True when the cursor is in the list rather than on the path text.
    pub fn in_list(&self) -> bool {
        self.selected.is_some()
    }

    /// Re-reads the subfolders of `path`, keeping the query and clamping the
    /// cursor.
    pub fn refresh(&mut self, path: &str) {
        let resolved = super::resolve_working_dir(path);
        self.entries = super::list_subdirectories(&resolved);
        self.apply_query(&resolved);
    }

    /// Leaves the list: cursor back on the path, search cleared.
    pub fn leave(&mut self) {
        self.selected = None;
        self.query.clear();
        self.rows = self.entries.clone();
    }

    fn apply_query(&mut self, resolved: &Path) {
        self.rows = if self.query.is_empty() {
            self.entries.clone()
        } else {
            search_subfolders(resolved, &self.query, SEARCH_DEPTH, SEARCH_MAX_RESULTS)
        };
        if let Some(i) = self.selected
            && i >= self.rows.len()
        {
            self.selected = Some(self.rows.len().saturating_sub(1));
        }
    }

    fn set_path(&mut self, path: &mut String, target: PathBuf) {
        *path = target.to_string_lossy().into_owned();
        self.query.clear();
        self.selected = None;
        self.refresh(path);
    }

    pub fn navigate_to_parent(&mut self, path: &mut String) -> bool {
        let resolved = super::resolve_working_dir(path);
        match resolved.parent() {
            Some(parent) => {
                let parent = parent.to_path_buf();
                self.set_path(path, parent);
                true
            }
            None => false,
        }
    }

    /// Enters `sub`, a name or a nested relative path from a search row.
    pub fn navigate_into(&mut self, path: &mut String, sub: &str) {
        let target = super::resolve_working_dir(path).join(sub);
        self.set_path(path, target);
    }

    /// Opens the row under the cursor; `..` goes up. Returns whether the
    /// path changed.
    fn open_selected(&mut self, path: &mut String) -> bool {
        let Some(entry) = self.selected.and_then(|i| self.rows.get(i).cloned()) else {
            return false;
        };
        if entry == ".." {
            self.navigate_to_parent(path)
        } else {
            self.navigate_into(path, &entry);
            true
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent, path: &mut String) -> PickerEvent {
        let consumed = |path_changed| PickerEvent::Consumed { path_changed };
        match key.code {
            KeyCode::Enter => {
                let is_parent = self
                    .selected
                    .and_then(|i| self.rows.get(i))
                    .is_some_and(|e| e == "..");
                if is_parent {
                    let changed = self.navigate_to_parent(path);
                    return consumed(changed);
                }
                self.open_selected(path);
                PickerEvent::Submit
            }
            KeyCode::Right => {
                if self.in_list() {
                    let changed = self.open_selected(path);
                    consumed(changed)
                } else {
                    if !self.rows.is_empty() {
                        self.selected = Some(0);
                    }
                    consumed(false)
                }
            }
            KeyCode::Left => {
                let changed = self.navigate_to_parent(path);
                consumed(changed)
            }
            KeyCode::Down => {
                if self.rows.is_empty() {
                    if !self.in_list() {
                        return PickerEvent::Ignored;
                    }
                } else {
                    self.selected = Some(match self.selected {
                        None => 0,
                        Some(i) => (i + 1).min(self.rows.len() - 1),
                    });
                }
                consumed(false)
            }
            KeyCode::Up => match self.selected {
                None => PickerEvent::Ignored,
                Some(0) => {
                    self.selected = None;
                    self.query.clear();
                    self.rows = self.entries.clone();
                    consumed(false)
                }
                Some(i) => {
                    self.selected = Some(i - 1);
                    consumed(false)
                }
            },
            KeyCode::Esc => {
                if self.in_list() && !self.query.is_empty() {
                    self.query.clear();
                    self.rows = self.entries.clone();
                    self.selected = Some(0);
                    consumed(false)
                } else {
                    PickerEvent::Ignored
                }
            }
            KeyCode::Char(c) => {
                if self.in_list() {
                    self.query.push(c);
                    let resolved = super::resolve_working_dir(path);
                    self.selected = Some(0);
                    self.apply_query(&resolved);
                    consumed(false)
                } else {
                    path.push(c);
                    self.refresh(path);
                    consumed(true)
                }
            }
            KeyCode::Backspace => {
                if self.in_list() {
                    if self.query.pop().is_some() {
                        let resolved = super::resolve_working_dir(path);
                        self.selected = Some(0);
                        self.apply_query(&resolved);
                    }
                    consumed(false)
                } else {
                    path.pop();
                    self.refresh(path);
                    consumed(true)
                }
            }
            _ => PickerEvent::Ignored,
        }
    }
}

/// Subfolders of `root` up to `max_depth` levels down whose relative path
/// contains `query` (case-insensitive), as `a/b` paths, shallowest first
/// then by name. Hidden directories, [`SEARCH_SKIP`] names and symlinks are
/// not entered; the walk stops after [`SEARCH_MAX_VISITED`] directories.
pub fn search_subfolders(
    root: &Path,
    query: &str,
    max_depth: usize,
    max_results: usize,
) -> Vec<String> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() || max_depth == 0 {
        return Vec::new();
    }
    let mut found: Vec<(usize, String)> = Vec::new();
    let mut queue: VecDeque<(PathBuf, String, usize)> = VecDeque::new();
    queue.push_back((root.to_path_buf(), String::new(), 0));
    let mut visited = 0usize;
    while let Some((dir, rel, depth)) = queue.pop_front() {
        if visited >= SEARCH_MAX_VISITED {
            break;
        }
        visited += 1;
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut children: Vec<(String, PathBuf)> = read_dir
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .filter(|(name, _)| !name.starts_with('.') && !SEARCH_SKIP.contains(&name.as_str()))
            .collect();
        children.sort_by_key(|(name, _)| name.to_lowercase());
        for (name, child) in children {
            let child_rel = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            if child_rel.to_lowercase().contains(&needle) {
                found.push((depth + 1, child_rel.clone()));
            }
            if depth + 1 < max_depth {
                queue.push_back((child, child_rel, depth + 1));
            }
        }
    }
    found.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });
    found.truncate(max_results);
    found.into_iter().map(|(_, rel)| rel).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn tree() -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        for d in [
            "alpha/src/core",
            "alpha/node_modules/x",
            "beta/docs",
            ".hidden/core",
            "core",
        ] {
            std::fs::create_dir_all(t.path().join(d)).unwrap();
        }
        t
    }

    #[test]
    fn search_finds_nested_matches_shallowest_first_and_skips_noise() {
        let t = tree();
        let rows = search_subfolders(t.path(), "CORE", SEARCH_DEPTH, SEARCH_MAX_RESULTS);
        assert_eq!(rows, vec!["core", "alpha/src/core"]);
        let rows = search_subfolders(t.path(), "x", SEARCH_DEPTH, SEARCH_MAX_RESULTS);
        assert!(rows.is_empty(), "node_modules is not entered: {rows:?}");
        assert!(search_subfolders(t.path(), "  ", SEARCH_DEPTH, 10).is_empty());
        let rows = search_subfolders(t.path(), "a", SEARCH_DEPTH, 2);
        assert_eq!(rows.len(), 2, "capped at max_results");
    }

    #[test]
    fn typing_in_the_list_searches_and_right_opens_a_nested_match() {
        let t = tree();
        let mut path = t.path().to_string_lossy().into_owned();
        let mut p = DirPicker::for_path(&path);
        assert!(p.rows().contains(&"alpha".to_string()));
        assert!(!p.in_list());

        // typing on the path edits it
        assert_eq!(
            p.handle_key(&key(KeyCode::Char('/')), &mut path),
            PickerEvent::Consumed { path_changed: true }
        );
        assert!(path.ends_with('/'));
        p.handle_key(&key(KeyCode::Backspace), &mut path);

        // down enters the list; typing there searches
        p.handle_key(&key(KeyCode::Down), &mut path);
        assert_eq!(p.selected, Some(0));
        for c in "src/co".chars() {
            p.handle_key(&key(KeyCode::Char(c)), &mut path);
        }
        assert_eq!(p.query, "src/co");
        assert_eq!(p.rows(), &["alpha/src/core".to_string()]);
        assert_eq!(p.selected, Some(0));

        // a query with no match keeps the cursor in the list
        p.handle_key(&key(KeyCode::Char('z')), &mut path);
        assert!(p.rows().is_empty());
        assert!(p.in_list());
        p.handle_key(&key(KeyCode::Backspace), &mut path);
        assert_eq!(p.rows().len(), 1);

        // right opens the nested match and clears the search
        assert_eq!(
            p.handle_key(&key(KeyCode::Right), &mut path),
            PickerEvent::Consumed { path_changed: true }
        );
        assert_eq!(PathBuf::from(&path), t.path().join("alpha/src/core"));
        assert!(p.query.is_empty());
        assert!(!p.in_list());

        // left goes to the parent; esc in the list clears a query, not the dialog
        p.handle_key(&key(KeyCode::Left), &mut path);
        assert_eq!(PathBuf::from(&path), t.path().join("alpha/src"));
        p.handle_key(&key(KeyCode::Down), &mut path);
        p.handle_key(&key(KeyCode::Char('q')), &mut path);
        assert_eq!(
            p.handle_key(&key(KeyCode::Esc), &mut path),
            PickerEvent::Consumed {
                path_changed: false
            }
        );
        assert!(p.query.is_empty());
        assert_eq!(
            p.handle_key(&key(KeyCode::Esc), &mut path),
            PickerEvent::Ignored
        );

        // up past the top returns to the path text; up there is the dialog's
        p.handle_key(&key(KeyCode::Up), &mut path);
        assert!(!p.in_list());
        assert_eq!(
            p.handle_key(&key(KeyCode::Up), &mut path),
            PickerEvent::Ignored
        );
    }

    #[test]
    fn enter_on_parent_goes_up_and_enter_on_a_row_submits_inside_it() {
        let t = tree();
        let mut path = t.path().join("beta").to_string_lossy().into_owned();
        let mut p = DirPicker::for_path(&path);
        assert_eq!(p.rows()[0], "..");
        p.handle_key(&key(KeyCode::Down), &mut path);
        assert_eq!(
            p.handle_key(&key(KeyCode::Enter), &mut path),
            PickerEvent::Consumed { path_changed: true }
        );
        assert_eq!(PathBuf::from(&path), t.path());
        let i = p.rows().iter().position(|r| r == "beta").unwrap();
        p.selected = Some(i);
        assert_eq!(
            p.handle_key(&key(KeyCode::Enter), &mut path),
            PickerEvent::Submit
        );
        assert_eq!(PathBuf::from(&path), t.path().join("beta"));
        assert_eq!(
            p.handle_key(&key(KeyCode::Enter), &mut path),
            PickerEvent::Submit
        );
    }
}
