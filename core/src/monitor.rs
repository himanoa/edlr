use crate::journal::position::{Position, PositionStore};
use crate::journal::{
    parser,
    tailer::{JournalLine, JournalTailer},
};
use crate::router::Router;
use crate::status::StatusReader;
use crate::watch::wake_source;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

struct Sources {
    tailer: JournalTailer,
    status: StatusReader,
    position_saver: PositionSaver,
    router: Router,
}

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
    let mut wake = wake_source(&dir, interval);
    let mut sources = Sources {
        tailer: JournalTailer::resume_from(dir.clone(), saved),
        status: StatusReader::new(dir.join("Status.json")),
        position_saver: PositionSaver::new(dir, positions),
        router,
    };

    while wake.rx.recv().await.is_some() {
        let new_sources = tokio::task::spawn_blocking(move || {
            let lines = sources.tailer.poll();
            let status = sources.status.poll();

            match lines {
                Ok(lines) => {
                    publish_lines(&sources.router, lines);
                    sources
                        .position_saver
                        .save_if_moved(sources.tailer.position());
                }
                Err(e) => tracing::warn!("journal poll failed (will retry): {e}"),
            }

            if let Some(s) = status {
                sources
                    .router
                    .publish(crate::event::Event::Status { raw: s });
            }

            sources
        })
        .await
        // JoinError はクロージャ自体が panic したときだけ。従来も poll 内の
        // panic は監視タスクを殺していたので、そのまま伝播させる。
        .expect("monitor poll task panicked");

        sources = new_sources;
    }
}

/// tail した行をパースして配信する。パースできない行は警告して飛ばす。
fn publish_lines(router: &Router, lines: Vec<JournalLine>) {
    for line in lines {
        match parser::parse_line(&line.text, line.replay) {
            Some(event) => router.publish(event),
            None => tracing::warn!("skipping unparsable journal line: {}", line.text),
        }
    }
}

/// 読み取り位置の永続化。位置が動いたときだけ書き、保存に失敗しても
/// デーモンは止めず、警告を 1 度だけ出して続行する。
struct PositionSaver {
    dir: PathBuf,
    store: Option<Arc<PositionStore>>,
    // warn を 1 度だけ出すためのフラグ(毎 poll ごとにログを溢れさせない)。
    warned: bool,
    // 直近に保存を試みた位置。wake は追記が無くても poll_interval_ms ごとに
    // 起きるため、これが無いとゲームを起動していない間もずっと同じ位置を
    // 書き続けてしまう(read + write + rename を 1 秒ごとに、情報量ゼロで)。
    // 成否に関わらず更新する: 書けない状況で毎秒失敗し続けるのを避け、
    // 位置が動いたときだけ再試行する。
    last_saved: Option<Position>,
}

impl PositionSaver {
    fn new(dir: PathBuf, store: Option<Arc<PositionStore>>) -> Self {
        Self {
            dir,
            store,
            warned: false,
            last_saved: None,
        }
    }

    fn save_if_moved(&mut self, position: Option<Position>) {
        let (Some(store), Some(position)) = (self.store.as_ref(), position) else {
            return;
        };
        if self.last_saved.as_ref() == Some(&position) {
            return;
        }
        if let Err(e) = store.save(&self.dir, &position) {
            if !self.warned {
                tracing::warn!(
                    "failed to persist the journal position ({e}); \
                     will retry when the position changes"
                );
                self.warned = true;
            }
        }
        self.last_saved = Some(position);
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
