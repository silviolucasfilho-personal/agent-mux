use agent_mux::tracing::correlate::{
    self, Adopted, ClaimRegistry, CorrelationSpec, release_claim, try_claim,
};
use agent_mux::transcript::Provider;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

fn registry() -> ClaimRegistry {
    Mutex::new(HashSet::new())
}

fn write_file(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn codex_meta_line(session_id: &str, cwd: &Path) -> String {
    format!(
        r#"{{"timestamp":"2026-08-31T10:00:00.000Z","type":"session_meta","payload":{{"session_id":"{session_id}","id":"thread-1","cwd":"{}","originator":"codex_cli_rs","cli_version":"0.151.0"}}}}"#,
        cwd.display()
    )
}

/// A date dir codex would use for "now" (UTC).
fn codex_today_dir(sessions: &Path) -> PathBuf {
    let odt = time::OffsetDateTime::from(SystemTime::now());
    sessions.join(format!(
        "{:04}/{:02}/{:02}",
        odt.year(),
        odt.month() as u8,
        odt.day()
    ))
}

#[test]
fn known_claude_adopts_expected_path_when_it_appears() {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    let expected = projects.join("-tmp-proj").join("uuid-1.jsonl");
    let claims = registry();
    let mut spec = CorrelationSpec::KnownClaude {
        session_id: "uuid-1".into(),
        expected_path: expected.clone(),
        projects_dir: projects.clone(),
        resume: false,
    };
    let started = Instant::now();
    assert_eq!(correlate::poll(&mut spec, started, &claims), None);
    write_file(&expected, "{}\n");
    let adopted = correlate::poll(&mut spec, started, &claims).unwrap();
    assert_eq!(
        adopted,
        Adopted {
            session_id: "uuid-1".into(),
            path: expected,
            correlation: "deterministic",
            resume_prime: false,
        }
    );
}

#[test]
fn known_claude_glob_fallback_finds_uuid_under_any_slug() {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    // the file lands under a DIFFERENT slug than computed (slug-scheme drift)
    let actual = projects.join("-some-hashed-slug-xyz").join("uuid-2.jsonl");
    write_file(&actual, "{}\n");
    let claims = registry();
    let mut spec = CorrelationSpec::KnownClaude {
        session_id: "uuid-2".into(),
        expected_path: projects.join("-computed-slug").join("uuid-2.jsonl"),
        projects_dir: projects.clone(),
        resume: true,
    };
    let adopted = correlate::poll(&mut spec, Instant::now(), &claims).unwrap();
    assert_eq!(adopted.path, actual);
    assert_eq!(adopted.correlation, "deterministic");
    assert!(adopted.resume_prime, "resume flag must carry through");
}

#[test]
fn watch_claude_adopts_newest_session_in_cwd_or_across_projects() {
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    let work_dir = dir.path().join("my-app");
    std::fs::create_dir_all(&work_dir).unwrap();

    let slug = agent_mux::history::project_slug(&work_dir);
    let app_proj = projects.join(&slug);
    let s1 = app_proj.join("sess-old.jsonl");
    let s2 = app_proj.join("sess-new.jsonl");
    write_file(&s1, "{}\n");
    std::thread::sleep(Duration::from_millis(50));
    write_file(&s2, "{}\n");

    let claims = registry();
    let mut spec = CorrelationSpec::WatchClaude {
        projects_dir: projects.clone(),
        cwd: work_dir.clone(),
        t0: SystemTime::now() - Duration::from_secs(10),
    };
    let adopted = correlate::poll(&mut spec, Instant::now(), &claims).unwrap();
    assert_eq!(adopted.session_id, "sess-new");
    assert_eq!(adopted.path, s2);
    assert_eq!(adopted.correlation, "watched");
    assert!(!adopted.resume_prime);
}

#[test]
fn known_antigravity_prefers_transcript_full() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let logs = root
        .join("brain")
        .join("conv-1")
        .join(".system_generated")
        .join("logs");
    write_file(&logs.join("transcript.jsonl"), "{}\n");
    write_file(&logs.join("transcript_full.jsonl"), "{}\n");
    let claims = registry();
    let mut spec = CorrelationSpec::KnownAntigravity {
        conversation_id: "conv-1".into(),
        root: root.to_path_buf(),
    };
    let adopted = correlate::poll(&mut spec, Instant::now(), &claims).unwrap();
    assert!(adopted.path.ends_with("transcript_full.jsonl"));
    assert!(adopted.resume_prime);
    assert_eq!(adopted.correlation, "deterministic");
}

#[test]
fn codex_adopts_only_matching_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    let cwd = dir.path().join("workdir");
    std::fs::create_dir_all(&cwd).unwrap();
    let other_cwd = dir.path().join("elsewhere");
    std::fs::create_dir_all(&other_cwd).unwrap();
    let date_dir = codex_today_dir(&sessions);
    // a rollout for a DIFFERENT cwd: must be rejected
    write_file(
        &date_dir.join("rollout-2026-08-31T10-00-00-other.jsonl"),
        &(codex_meta_line("other-session", &other_cwd) + "\n"),
    );
    let claims = registry();
    let t0 = SystemTime::now() - Duration::from_secs(1);
    let mut spec = CorrelationSpec::WatchCodex {
        sessions_dir: sessions.clone(),
        cwd: cwd.clone(),
        t0,
    };
    assert_eq!(correlate::poll(&mut spec, Instant::now(), &claims), None);
    // now ours appears
    let ours = date_dir.join("rollout-2026-08-31T10-00-01-ours.jsonl");
    write_file(&ours, &(codex_meta_line("our-session", &cwd) + "\n"));
    let adopted = correlate::poll(&mut spec, Instant::now(), &claims).unwrap();
    assert_eq!(adopted.session_id, "our-session");
    assert_eq!(adopted.path, ours);
    assert_eq!(adopted.correlation, "watched");
    assert!(
        !adopted.resume_prime,
        "watch-adopted content must be exported"
    );
}

#[test]
fn codex_stale_rollouts_predating_spawn_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let sessions = dir.path().join("sessions");
    let cwd = dir.path().join("workdir");
    std::fs::create_dir_all(&cwd).unwrap();
    let old = codex_today_dir(&sessions).join("rollout-old.jsonl");
    write_file(&old, &(codex_meta_line("old-session", &cwd) + "\n"));
    // t0 well after the file's mtime
    let t0 = SystemTime::now() + Duration::from_secs(3600);
    let claims = registry();
    let mut spec = CorrelationSpec::WatchCodex {
        sessions_dir: sessions,
        cwd,
        t0,
    };
    assert_eq!(correlate::poll(&mut spec, Instant::now(), &claims), None);
}

#[test]
fn claim_registry_prevents_double_adoption_and_releases() {
    let claims = registry();
    assert!(try_claim(&claims, Provider::Codex, "sess-1"));
    assert!(
        !try_claim(&claims, Provider::Codex, "sess-1"),
        "double claim"
    );
    // same id under a different provider is a different key
    assert!(try_claim(&claims, Provider::Claude, "sess-1"));
    release_claim(&claims, Provider::Codex, "sess-1");
    assert!(
        try_claim(&claims, Provider::Codex, "sess-1"),
        "released claims re-adopt"
    );
}

#[test]
fn exit_then_resume_readopts_after_release() {
    // A session exits (pipeline releases its claim); resuming the same
    // conversation in the same app run must correlate again.
    let dir = tempfile::tempdir().unwrap();
    let projects = dir.path().join("projects");
    let expected = projects.join("-p").join("uuid-3.jsonl");
    write_file(&expected, "{}\n");
    let claims = registry();
    let make_spec = || CorrelationSpec::KnownClaude {
        session_id: "uuid-3".into(),
        expected_path: expected.clone(),
        projects_dir: projects.clone(),
        resume: true,
    };
    let mut spec = make_spec();
    assert!(correlate::poll(&mut spec, Instant::now(), &claims).is_some());
    // second pipeline while the first still owns it: spins, not steals
    let mut spec2 = make_spec();
    assert!(correlate::poll(&mut spec2, Instant::now(), &claims).is_none());
    // first pipeline exits and releases
    release_claim(&claims, Provider::Claude, "uuid-3");
    assert!(correlate::poll(&mut spec2, Instant::now(), &claims).is_some());
}

fn agy_conv(root: &Path, id: &str, content: &str) {
    write_file(
        &root
            .join("brain")
            .join(id)
            .join(".system_generated")
            .join("logs")
            .join("transcript_full.jsonl"),
        content,
    );
}

#[test]
fn antigravity_snapshot_then_presence_lock_tier() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // pre-existing conversation: snapshotted, never a candidate
    agy_conv(root, "old-conv", "{}\n");
    let claims = registry();
    let t0 = SystemTime::now() - Duration::from_secs(1);
    let mut spec = CorrelationSpec::WatchAntigravity {
        root: root.to_path_buf(),
        cwd: PathBuf::from("/nonexistent-cwd"),
        t0,
        initial: None,
    };
    let started = Instant::now();
    // first poll takes the snapshot
    assert_eq!(correlate::poll(&mut spec, started, &claims), None);
    // a new conversation appears with a presence lock
    agy_conv(root, "new-conv", "{}\n");
    write_file(&root.join("presence").join("new-conv.lock"), "");
    let adopted = correlate::poll(&mut spec, started, &claims).unwrap();
    assert_eq!(adopted.session_id, "new-conv");
    assert_eq!(adopted.correlation, "watched");
    assert!(adopted.path.ends_with("transcript_full.jsonl"));
}

#[test]
fn antigravity_cwd_substring_tier_is_heuristic() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let claims = registry();
    let cwd = PathBuf::from("/home/user/my-special-project");
    let mut spec = CorrelationSpec::WatchAntigravity {
        root: root.to_path_buf(),
        cwd: cwd.clone(),
        t0: SystemTime::now() - Duration::from_secs(1),
        initial: None,
    };
    let started = Instant::now();
    assert_eq!(correlate::poll(&mut spec, started, &claims), None); // snapshot
    // no presence lock, but the transcript mentions our cwd
    agy_conv(
        root,
        "conv-a",
        &format!("{{\"content\":\"listing {} now\"}}\n", cwd.display()),
    );
    let adopted = correlate::poll(&mut spec, started, &claims).unwrap();
    assert_eq!(adopted.session_id, "conv-a");
    assert_eq!(adopted.correlation, "heuristic");
}

#[test]
fn antigravity_sole_candidate_tier_needs_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let claims = registry();
    let mut spec = CorrelationSpec::WatchAntigravity {
        root: root.to_path_buf(),
        cwd: PathBuf::from("/mentioned-nowhere"),
        t0: SystemTime::now() - Duration::from_secs(1),
        initial: None,
    };
    let started_recent = Instant::now();
    assert_eq!(correlate::poll(&mut spec, started_recent, &claims), None); // snapshot
    agy_conv(root, "conv-solo", "{\"content\":\"unrelated\"}\n");
    // too early: not adopted on timing alone
    assert_eq!(correlate::poll(&mut spec, started_recent, &claims), None);
    // after the 15s window (backdated start): the sole candidate is adopted
    let started_old = Instant::now() - Duration::from_secs(16);
    let adopted = correlate::poll(&mut spec, started_old, &claims).unwrap();
    assert_eq!(adopted.session_id, "conv-solo");
    assert_eq!(adopted.correlation, "heuristic");
}

#[test]
fn antigravity_two_candidates_are_not_adopted_on_timing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let claims = registry();
    let mut spec = CorrelationSpec::WatchAntigravity {
        root: root.to_path_buf(),
        cwd: PathBuf::from("/mentioned-nowhere"),
        t0: SystemTime::now() - Duration::from_secs(1),
        initial: None,
    };
    let started = Instant::now() - Duration::from_secs(20);
    assert_eq!(correlate::poll(&mut spec, started, &claims), None); // snapshot
    agy_conv(root, "conv-x", "{}\n");
    agy_conv(root, "conv-y", "{}\n");
    // ambiguous: neither is adopted
    assert_eq!(correlate::poll(&mut spec, started, &claims), None);
}
