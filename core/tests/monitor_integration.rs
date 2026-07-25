use edlr_core::event::Event;
use edlr_core::monitor;
use edlr_core::router::Router;
use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

fn append(path: &std::path::Path, s: &str) {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(s.as_bytes()).unwrap();
}

async fn next_event(
    rx: &mut tokio::sync::broadcast::Receiver<std::sync::Arc<Event>>,
) -> std::sync::Arc<Event> {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("event within 5s")
        .expect("channel open")
}

#[tokio::test]
async fn routes_journal_and_status_events() {
    let dir = tempfile::tempdir().unwrap();
    let router = Router::new(64);
    let mut rx = router.subscribe();
    let _task = tokio::spawn(monitor::run(
        dir.path().to_path_buf(),
        router.clone(),
        Duration::from_millis(50),
    ));

    append(
        &dir.path().join("Journal.2026-07-25T120000.01.log"),
        "{\"timestamp\":\"2026-07-25T12:00:00Z\",\"event\":\"FSDJump\"}\nbroken line\n",
    );
    let ev = next_event(&mut rx).await;
    assert!(matches!(&*ev, Event::Journal { event, .. } if event == "FSDJump"));

    std::fs::write(dir.path().join("Status.json"), r#"{"Flags":16777240}"#).unwrap();
    let ev = next_event(&mut rx).await;
    assert!(matches!(&*ev, Event::Status { raw } if raw["Flags"] == 16777240));
}
