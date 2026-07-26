//! 位置の永続化が「再起動しても同じイベントを配り直さない」ことを、
//! monitor::run を実際に回して確認する。

use std::sync::Arc;
use std::time::Duration;

use edlr_core::journal::position::PositionStore;
use edlr_core::monitor;
use edlr_core::router::Router;

fn append(path: &std::path::Path, s: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(s.as_bytes()).unwrap();
}

/// `monitor::run` を短時間回し、その間に配信されたイベントを集める。
async fn collect_for(
    dir: &std::path::Path,
    positions: Arc<PositionStore>,
    millis: u64,
) -> Vec<edlr_core::event::Event> {
    let router = Router::new(256);
    let mut rx = router.subscribe();
    let handle = tokio::spawn(monitor::run(
        dir.to_path_buf(),
        router,
        Duration::from_millis(20),
        Some(positions),
    ));

    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(millis);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => seen.push((*event).clone()),
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    handle.abort();
    seen
}

#[tokio::test]
async fn a_restart_does_not_redeliver_what_was_already_read() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = tmp.path().join("journal");
    std::fs::create_dir(&journal).unwrap();
    let path = journal.join("Journal.2026-07-27T120000.01.log");
    append(
        &path,
        "{\"timestamp\":\"2026-07-27T12:00:00Z\",\"event\":\"FSDJump\"}\n",
    );

    let positions = Arc::new(PositionStore::new(tmp.path().join("state")));

    let first = collect_for(&journal, positions.clone(), 300).await;
    assert_eq!(first.len(), 1, "the pre-existing line is delivered once");

    // 2 回目の起動。ファイルは変わっていない。
    let second = collect_for(&journal, positions.clone(), 300).await;
    assert!(
        second.is_empty(),
        "a restart must not redeliver lines that were already consumed"
    );
}

#[tokio::test]
async fn lines_written_while_the_daemon_was_down_arrive_exactly_once_as_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = tmp.path().join("journal");
    std::fs::create_dir(&journal).unwrap();
    let path = journal.join("Journal.2026-07-27T120000.01.log");
    append(
        &path,
        "{\"timestamp\":\"2026-07-27T12:00:00Z\",\"event\":\"FSDJump\"}\n",
    );

    let positions = Arc::new(PositionStore::new(tmp.path().join("state")));
    collect_for(&journal, positions.clone(), 300).await;

    // デーモンが止まっている間に追記される。
    append(
        &path,
        "{\"timestamp\":\"2026-07-27T12:05:00Z\",\"event\":\"Docked\"}\n",
    );

    let second = collect_for(&journal, positions.clone(), 300).await;
    assert_eq!(second.len(), 1);
    match &second[0] {
        edlr_core::event::Event::Journal { event, replay, .. } => {
            assert_eq!(event, "Docked");
            assert!(replay, "it was already in the file when the daemon started");
        }
        other => panic!("expected a journal event, got {other:?}"),
    }
}

#[tokio::test]
async fn without_a_position_store_the_old_behaviour_is_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let journal = tmp.path().join("journal");
    std::fs::create_dir(&journal).unwrap();
    append(
        &journal.join("Journal.2026-07-27T120000.01.log"),
        "{\"timestamp\":\"2026-07-27T12:00:00Z\",\"event\":\"FSDJump\"}\n",
    );

    let first = collect_for_without_store(&journal, 300).await;
    let second = collect_for_without_store(&journal, 300).await;

    assert_eq!(first.len(), 1);
    assert_eq!(
        second.len(),
        1,
        "with no store there is nothing to resume from, so the file is re-read"
    );
}

/// `positions = None`(state ディレクトリに書けない環境の劣化動作)。
async fn collect_for_without_store(
    dir: &std::path::Path,
    millis: u64,
) -> Vec<edlr_core::event::Event> {
    let router = Router::new(256);
    let mut rx = router.subscribe();
    let handle = tokio::spawn(monitor::run(
        dir.to_path_buf(),
        router,
        Duration::from_millis(20),
        None,
    ));

    let mut seen = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(millis);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => seen.push((*event).clone()),
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    handle.abort();
    seen
}
