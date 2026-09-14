use super::model::LiveSession;
use super::service::ServiceError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Maximum serialized file size for a single live snapshot (1 MiB).
pub const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;

/// Live process and session snapshot emitted by an active agent-mux instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LiveSnapshot {
    pub run_id: String,
    pub revision: u64,
    pub heartbeat_ns: i64,
    pub sessions: Vec<LiveSession>,
}

/// Returns true if the heartbeat is older than 5 seconds (5,000,000,000 ns).
pub fn is_stale(heartbeat_ns: i64, now_ns: i64) -> bool {
    now_ns.saturating_sub(heartbeat_ns) > 5_000_000_000
}

#[cfg(unix)]
fn is_owned_by_current_user(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    let euid = unsafe { geteuid() };
    meta.uid() == euid
}

#[cfg(not(unix))]
fn is_owned_by_current_user(_meta: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path, is_dir: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if is_dir { 0o700 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &Path, _is_dir: bool) -> std::io::Result<()> {
    Ok(())
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Atomically publishes a live snapshot using temporary file plus rename.
pub fn publish_snapshot(root: &Path, snapshot: &LiveSnapshot) -> Result<(), ServiceError> {
    if !root.exists() {
        fs::create_dir_all(root)
            .map_err(|e| ServiceError::Internal(format!("cannot create snapshot dir: {e}")))?;
        let _ = set_owner_only_permissions(root, true);
    }

    let root_meta = fs::symlink_metadata(root)
        .map_err(|e| ServiceError::Internal(format!("cannot stat snapshot dir: {e}")))?;
    if root_meta.file_type().is_symlink() {
        return Err(ServiceError::InvalidArgument(
            "snapshot directory cannot be a symlink".into(),
        ));
    }
    if !is_owned_by_current_user(&root_meta) {
        return Err(ServiceError::InvalidArgument(
            "snapshot directory must be owned by current user".into(),
        ));
    }

    let safe_run_id = sanitize_id(&snapshot.run_id);
    if safe_run_id.is_empty() {
        return Err(ServiceError::InvalidArgument("empty run_id".into()));
    }

    let target_file = root.join(format!("{safe_run_id}.json"));
    let temp_file = root.join(format!(".{safe_run_id}.tmp-{}", uuid::Uuid::new_v4()));

    // Bound snapshot to MAX_SNAPSHOT_BYTES by truncating sessions if needed
    let mut bounded_snapshot = snapshot.clone();
    let mut json_bytes = serde_json::to_vec(&bounded_snapshot)
        .map_err(|e| ServiceError::Internal(format!("failed to serialize snapshot: {e}")))?;

    while json_bytes.len() > MAX_SNAPSHOT_BYTES && !bounded_snapshot.sessions.is_empty() {
        bounded_snapshot.sessions.pop();
        json_bytes = serde_json::to_vec(&bounded_snapshot)
            .map_err(|e| ServiceError::Internal(format!("failed to serialize snapshot: {e}")))?;
    }

    if json_bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(ServiceError::Internal(
            "snapshot exceeds 1MiB bound even with 0 sessions".into(),
        ));
    }

    // Write to temp file
    fs::write(&temp_file, &json_bytes)
        .map_err(|e| ServiceError::Internal(format!("cannot write temp snapshot: {e}")))?;
    let _ = set_owner_only_permissions(&temp_file, false);

    // Atomic rename
    if let Err(e) = fs::rename(&temp_file, &target_file) {
        let _ = fs::remove_file(&temp_file);
        return Err(ServiceError::Internal(format!(
            "failed to rename snapshot: {e}"
        )));
    }

    Ok(())
}

/// Removes the live snapshot for a run on clean shutdown.
pub fn clean_up_snapshot(root: &Path, run_id: &str) -> Result<(), ServiceError> {
    if !root.exists() {
        return Ok(());
    }

    let safe_run_id = sanitize_id(run_id);
    let target_file = root.join(format!("{safe_run_id}.json"));

    if target_file.exists() {
        if let Ok(meta) = fs::symlink_metadata(&target_file) {
            if !meta.file_type().is_symlink() && is_owned_by_current_user(&meta) {
                let _ = fs::remove_file(&target_file);
            }
        }
    }

    Ok(())
}

/// Reads all valid, non-stale live snapshots from the runtime snapshot directory.
pub fn read_snapshots(root: &Path, now_ns: i64) -> Result<Vec<LiveSnapshot>, ServiceError> {
    if !root.exists() {
        return Ok(Vec::new());
    }

    let root_meta = match fs::symlink_metadata(root) {
        Ok(m) => m,
        Err(_) => return Ok(Vec::new()),
    };

    if root_meta.file_type().is_symlink() || !is_owned_by_current_user(&root_meta) {
        return Ok(Vec::new());
    }

    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return Ok(Vec::new()),
    };

    // Deduplicate by run_id: keep highest revision
    let mut by_run: HashMap<String, LiveSnapshot> = HashMap::new();

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = match path.file_name().and_then(|f| f.to_str()) {
            Some(f) => f,
            None => continue,
        };

        // Skip temp and non-json files
        if file_name.starts_with('.') || !file_name.ends_with(".json") {
            continue;
        }

        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };

        // Reject symlinks, non-files, and unowned files
        if meta.file_type().is_symlink() || !meta.is_file() || !is_owned_by_current_user(&meta) {
            continue;
        }

        // Validate maximum file size before parsing
        if meta.len() > MAX_SNAPSHOT_BYTES as u64 {
            continue;
        }

        let content = match fs::read(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let snapshot: LiveSnapshot = match serde_json::from_slice(&content) {
            Ok(s) => s,
            Err(_) => continue,
        };

        // Check if heartbeat is stale
        if is_stale(snapshot.heartbeat_ns, now_ns) {
            continue;
        }

        // Keep newest valid revision within a run
        match by_run.get(&snapshot.run_id) {
            Some(existing) if existing.revision >= snapshot.revision => {}
            _ => {
                by_run.insert(snapshot.run_id.clone(), snapshot);
            }
        }
    }

    let mut result: Vec<LiveSnapshot> = by_run.into_values().collect();
    result.sort_by(|a, b| a.run_id.cmp(&b.run_id));
    Ok(result)
}
