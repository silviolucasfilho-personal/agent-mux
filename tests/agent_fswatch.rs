use agent_mux::agent::fswatch;
use agent_mux::events::AppEvent;
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::mpsc;

async fn expect_agents_changed(rx: &mut mpsc::Receiver<AppEvent>) {
    let deadline = Duration::from_secs(15);
    let got = tokio::time::timeout(deadline, async {
        loop {
            match rx.recv().await {
                Some(AppEvent::AgentsChanged) => break true,
                Some(_) => continue,
                None => break false,
            }
        }
    })
    .await;
    assert_eq!(got, Ok(true), "expected AgentsChanged within {deadline:?}");
}

#[tokio::test]
async fn writing_a_package_into_a_watched_root_emits_agents_changed() {
    let root = tempdir().unwrap();
    let (tx, mut rx) = mpsc::channel(32);
    fswatch::spawn(vec![root.path().to_path_buf()], tx).unwrap();
    // Give the OS watcher a moment to arm before producing events.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let pkg = root.path().join("reviewer");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("AGENTS.md"),
        "---\nid: reviewer\nharnesses: [claude]\n---\nReview.",
    )
    .unwrap();

    expect_agents_changed(&mut rx).await;
}

#[tokio::test]
async fn a_root_created_after_spawn_is_picked_up() {
    let parent = tempdir().unwrap();
    let root = parent.path().join(".agent-mux").join("agents");
    let (tx, mut rx) = mpsc::channel(32);
    fswatch::spawn(vec![root.clone()], tx).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Creating the root itself is a change worth a rescan…
    std::fs::create_dir_all(&root).unwrap();
    expect_agents_changed(&mut rx).await;

    // …and once it exists, packages inside it are watched too.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let pkg = root.join("audit");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("AGENTS.md"),
        "---\nid: audit\nharnesses: [codex]\n---\nAudit.",
    )
    .unwrap();
    expect_agents_changed(&mut rx).await;
}

#[tokio::test]
async fn bursts_are_debounced_into_one_event() {
    let root = tempdir().unwrap();
    let (tx, mut rx) = mpsc::channel(32);
    fswatch::spawn(vec![root.path().to_path_buf()], tx).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    for i in 0..5 {
        let pkg = root.path().join(format!("a{i}"));
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("AGENTS.md"),
            format!("---\nid: a{i}\nharnesses: [claude]\n---\nX."),
        )
        .unwrap();
    }
    expect_agents_changed(&mut rx).await;

    // After the burst settles, no further events trickle in.
    tokio::time::sleep(fswatch::DEBOUNCE * 4).await;
    let mut extra = 0;
    while let Ok(Some(AppEvent::AgentsChanged)) =
        tokio::time::timeout(Duration::from_millis(50), rx.recv()).await
    {
        extra += 1;
    }
    assert!(extra <= 1, "burst produced {} trailing events", extra + 1);
}
