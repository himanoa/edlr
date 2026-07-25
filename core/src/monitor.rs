use crate::journal::{parser, tailer::JournalTailer};
use crate::router::Router;
use crate::status::StatusReader;
use crate::watch::wake_source;
use std::path::PathBuf;
use std::time::Duration;

/// 監視ループ本体。wake のたびに Journal と Status.json を poll して配信する。
/// エラーで panic せず、ログして継続する。
pub async fn run(dir: PathBuf, router: Router, interval: Duration) {
    let mut tailer = JournalTailer::new(dir.clone());
    let mut status = StatusReader::new(dir.join("Status.json"));
    let mut wake = wake_source(&dir, interval);

    while wake.rx.recv().await.is_some() {
        match tailer.poll() {
            Ok(lines) => {
                for line in lines {
                    match parser::parse_line(&line) {
                        Some(event) => router.publish(event),
                        None => tracing::warn!("skipping unparsable journal line: {line}"),
                    }
                }
            }
            Err(e) => tracing::warn!("journal poll failed (will retry): {e}"),
        }
        if let Some(raw) = status.poll() {
            router.publish(crate::event::Event::Status { raw });
        }
    }
}
