//! `MEMORY.md`: one line per note, rebuilt from the notes' frontmatter.
//!
//! The text is byte-stable: a fixed order (pinned, then kind, then name)
//! and nothing time-dependent, so a harness given it at every start sees
//! the same bytes until a note changes, and its prompt cache keeps them.
//! Past the cap the oldest unpinned notes leave the index (observed ones
//! first) and stay on disk; the last line says how many.

use super::note::{self, Confidence, Note};
use super::paths;
use std::path::Path;
use std::time::SystemTime;

/// The first line of every index.
pub const TITLE: &str = "# Memory";

/// One index line.
pub fn line(n: &Note) -> String {
    let pin = if n.pinned { "📌 " } else { "" };
    format!(
        "- {pin}[{}]({}/{}.md) — {} ({})",
        n.name,
        paths::NOTES,
        n.name,
        n.description.replace(['\n', '\r'], " "),
        n.kind.as_str()
    )
}

/// The notes the index lists and how many it leaves out, in index order.
pub fn select(mut notes: Vec<Note>, cap: usize) -> (Vec<Note>, usize) {
    let total = notes.len();
    let pinned = notes.iter().filter(|n| n.pinned).count();
    let room = cap.max(pinned);
    if total > room {
        // drop order: observed before the rest, oldest first
        let mut droppable: Vec<usize> = (0..notes.len()).filter(|i| !notes[*i].pinned).collect();
        droppable.sort_by(|a, b| {
            let (x, y) = (&notes[*a], &notes[*b]);
            (x.confidence != Confidence::Observed)
                .cmp(&(y.confidence != Confidence::Observed))
                .then_with(|| x.source.when.cmp(&y.source.when))
                .then_with(|| x.name.cmp(&y.name))
        });
        let mut drop: Vec<usize> = droppable.into_iter().take(total - room).collect();
        drop.sort_unstable_by(|a, b| b.cmp(a));
        for i in drop {
            notes.remove(i);
        }
    }
    notes.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(a.kind.cmp(&b.kind))
            .then_with(|| a.name.cmp(&b.name))
    });
    let left_out = total - notes.len();
    (notes, left_out)
}

/// The index text for these notes.
pub fn render(notes: Vec<Note>, cap: usize, footer_template: &str) -> String {
    let (listed, left_out) = select(notes, cap);
    let mut out = String::from(TITLE);
    out.push_str("\n\n");
    if listed.is_empty() {
        out.push_str("No notes yet.\n");
    }
    for n in &listed {
        out.push_str(&line(n));
        out.push('\n');
    }
    if left_out > 0 {
        out.push('\n');
        out.push_str(&footer_template.replace("{count}", &left_out.to_string()));
        out.push('\n');
    }
    out
}

/// Rebuilds the folder's index; returns its text.
pub fn rebuild(dir: &Path, cap: usize, footer_template: &str) -> std::io::Result<String> {
    let (notes, _) = note::load_all(dir);
    let text = render(notes, cap, footer_template);
    let path = paths::index_path(dir);
    if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        paths::write_atomic(&path, &text)?;
    } else {
        // same bytes: mark it fresh without rewriting the content
        let _ = std::fs::File::options()
            .append(true)
            .open(&path)
            .and_then(|f| f.set_modified(SystemTime::now()));
    }
    Ok(text)
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Whether the index is missing or older than a note or the notes folder
/// (a removed note changes the folder's time).
pub fn is_stale(dir: &Path) -> bool {
    let Some(index) = mtime(&paths::index_path(dir)) else {
        return true;
    };
    let notes_dir = paths::notes_dir(dir);
    if mtime(&notes_dir).is_some_and(|t| t > index) {
        return true;
    }
    note::files(dir)
        .iter()
        .any(|p| mtime(p).is_some_and(|t| t > index))
}

/// Rebuilds the index when it is stale; returns whether it did.
pub fn ensure_fresh(dir: &Path, cap: usize, footer_template: &str) -> std::io::Result<bool> {
    if !dir.is_dir() {
        return Ok(false);
    }
    if is_stale(dir) {
        rebuild(dir, cap, footer_template)?;
        return Ok(true);
    }
    Ok(false)
}

/// Moves notes past their `expires_at` to `notes/.expired/`; returns
/// their names.
pub fn expire(dir: &Path, today: &str) -> Vec<String> {
    let (notes, _) = note::load_all(dir);
    notes
        .into_iter()
        .filter(|n| n.expires_at.as_deref().is_some_and(|d| d < today))
        .filter(|n| note::shelve(dir, &n.name, paths::EXPIRED).is_ok())
        .map(|n| n.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::note::{Kind, Source};

    fn n(name: &str, kind: Kind, conf: Confidence, when: &str, pinned: bool) -> Note {
        Note {
            name: name.into(),
            description: format!("about {name}"),
            kind,
            source: Source {
                harness: "claude".into(),
                when: Some(when.into()),
                ..Default::default()
            },
            confidence: conf,
            pinned,
            expires_at: None,
            body: String::new(),
        }
    }

    #[test]
    fn order_is_pinned_kind_name_and_the_cap_drops_old_observed_first() {
        let notes = vec![
            n("zeta", Kind::User, Confidence::Stated, "2026-01-01", false),
            n(
                "alpha",
                Kind::Project,
                Confidence::Observed,
                "2026-01-02",
                false,
            ),
            n(
                "beta",
                Kind::Project,
                Confidence::Observed,
                "2026-03-01",
                false,
            ),
            n(
                "pin",
                Kind::Reference,
                Confidence::Observed,
                "2025-01-01",
                true,
            ),
        ];
        let (listed, out) = select(notes.clone(), 10);
        let names: Vec<_> = listed.iter().map(|n| n.name.as_str()).collect();
        assert_eq!((names, out), (vec!["pin", "zeta", "alpha", "beta"], 0));
        let (listed, out) = select(notes.clone(), 2);
        let names: Vec<_> = listed.iter().map(|n| n.name.as_str()).collect();
        // alpha (oldest observed) then beta go; the stated note stays
        assert_eq!((names, out), (vec!["pin", "zeta"], 2));
        let text = render(notes, 2, "{count} more");
        assert!(text.ends_with("\n2 more\n"), "{text}");
        assert!(text.contains("- 📌 [pin](notes/pin.md) — about pin (reference)"));
    }

    #[test]
    fn rebuild_is_byte_stable_and_freshness_follows_the_notes() {
        let d = tempfile::tempdir().unwrap();
        paths::ensure(d.path()).unwrap();
        assert!(is_stale(d.path()));
        note::write(
            d.path(),
            &n("a", Kind::User, Confidence::Stated, "x", false),
        )
        .unwrap();
        let first = rebuild(d.path(), 60, "").unwrap();
        assert!(!is_stale(d.path()));
        std::thread::sleep(std::time::Duration::from_millis(20));
        note::write(
            d.path(),
            &n("b", Kind::User, Confidence::Stated, "y", false),
        )
        .unwrap();
        assert!(is_stale(d.path()));
        assert!(ensure_fresh(d.path(), 60, "").unwrap());
        let second = std::fs::read_to_string(paths::index_path(d.path())).unwrap();
        assert_ne!(first, second);
        assert_eq!(rebuild(d.path(), 60, "").unwrap(), second);
        assert!(!ensure_fresh(d.path(), 60, "").unwrap());
        std::thread::sleep(std::time::Duration::from_millis(20));
        note::shelve(d.path(), "b", paths::REMOVED).unwrap();
        assert!(ensure_fresh(d.path(), 60, "").unwrap());
        assert_eq!(
            std::fs::read_to_string(paths::index_path(d.path())).unwrap(),
            first
        );
    }

    #[test]
    fn expired_notes_are_shelved() {
        let d = tempfile::tempdir().unwrap();
        paths::ensure(d.path()).unwrap();
        let mut old = n("old", Kind::Project, Confidence::Stated, "x", true);
        old.expires_at = Some("2026-01-01".into());
        note::write(d.path(), &old).unwrap();
        note::write(
            d.path(),
            &n("keep", Kind::Project, Confidence::Stated, "x", false),
        )
        .unwrap();
        assert_eq!(expire(d.path(), "2026-01-01"), Vec::<String>::new());
        assert_eq!(expire(d.path(), "2026-01-02"), vec!["old".to_string()]);
        assert!(d.path().join("notes/.expired/old.md").is_file());
    }
}
