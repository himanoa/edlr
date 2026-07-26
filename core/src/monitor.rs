use crate::journal::position::PositionStore;
use crate::journal::{parser, tailer::JournalTailer};
use crate::router::Router;
use crate::status::StatusReader;
use crate::watch::wake_source;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// 監視ループ本体。wake のたびに Journal と Status.json を poll して配信する。
/// エラーで panic せず、ログして継続する。
///
/// `positions` が `Some` の場合、起動時に保存済みの読み取り位置から再開し、
/// 配信のたびに新しい位置を永続化する(デーモン再起動のたびに現行 Journal を
/// 丸ごと再配信しないようにするため)。`None` は永続化なし(従来の挙動)。
pub async fn run(
    dir: PathBuf,
    router: Router,
    interval: Duration,
    positions: Option<Arc<PositionStore>>,
) {
    let saved = positions.as_ref().and_then(|store| store.load(&dir));
    let mut tailer = JournalTailer::resume_from(dir.clone(), saved);
    let mut status = StatusReader::new(dir.join("Status.json"));
    let mut wake = wake_source(&dir, interval);
    // 保存失敗の warn を 1 度だけ出すためのフラグ(毎 poll ごとにログを溢れさせない)。
    let mut warned_about_saving = false;

    while wake.rx.recv().await.is_some() {
        match tailer.poll() {
            Ok(lines) => {
                for line in lines {
                    match parser::parse_line(&line.text, line.replay) {
                        Some(event) => router.publish(event),
                        None => tracing::warn!("skipping unparsable journal line: {}", line.text),
                    }
                }
                // 配信した後に保存する(at-least-once)。保存に失敗しても
                // デーモンは止めず、警告を 1 度だけ出して続行する。
                if let (Some(store), Some(position)) = (positions.as_ref(), tailer.position()) {
                    if let Err(e) = store.save(&dir, &position) {
                        if !warned_about_saving {
                            tracing::warn!(
                                "failed to persist the journal position ({e}); \
                                 continuing without persistence"
                            );
                            warned_about_saving = true;
                        }
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
