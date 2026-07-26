use crate::journal::position::{Position, PositionStore};
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
    // 直近に保存を試みた位置。wake は追記が無くても poll_interval_ms ごとに
    // 起きるため、これが無いとゲームを起動していない間もずっと同じ位置を
    // 書き続けてしまう(read + write + rename を 1 秒ごとに、情報量ゼロで)。
    // 成否に関わらず更新する: 書けない状況で毎秒失敗し続けるのを避け、
    // 位置が動いたときだけ再試行する。
    let mut last_saved: Option<Position> = None;

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
                    if last_saved.as_ref() != Some(&position) {
                        if let Err(e) = store.save(&dir, &position) {
                            if !warned_about_saving {
                                tracing::warn!(
                                    "failed to persist the journal position ({e}); \
                                     will retry when the position changes"
                                );
                                warned_about_saving = true;
                            }
                        }
                        last_saved = Some(position);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn append(path: &std::path::Path, s: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    /// 条件が満たされるまで待つ(満たされなければ false)。
    async fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// 位置が変わらない限り保存を走らせてはならない。ゲームが起動していない
    /// 間も毎 poll(既定 1 秒)書き続けると、情報量ゼロの read + write + rename
    /// を 1 日 8 万回以上回すことになる。
    #[tokio::test]
    async fn does_not_rewrite_the_position_file_while_the_position_is_unchanged() {
        let journal = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let path = journal.path().join("Journal.2026-07-27T120000.01.log");
        append(&path, "{\"timestamp\":\"t\",\"event\":\"A\"}\n");

        let store = Arc::new(PositionStore::new(state.path().to_path_buf()));
        let saved_file = state.path().join("journal-position.json");
        let handle = tokio::spawn(run(
            journal.path().to_path_buf(),
            Router::new(16),
            Duration::from_millis(20),
            Some(store.clone()),
        ));

        assert!(
            wait_until(|| saved_file.is_file()).await,
            "the first poll must persist a position"
        );

        // 保存ファイルを消したあと、位置が変わらなければ作り直されないこと。
        std::fs::remove_file(&saved_file).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await; // 15 回ぶんの poll
        assert!(
            !saved_file.exists(),
            "an unchanged position must not be written again on every poll"
        );

        // 位置が変われば保存が再開する。
        append(&path, "{\"timestamp\":\"t\",\"event\":\"B\"}\n");
        assert!(
            wait_until(|| saved_file.is_file()).await,
            "a changed position must be persisted again"
        );

        handle.abort();
    }
}
