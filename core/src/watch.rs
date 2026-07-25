use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;

/// ファイル変更の「起きろ」シグナル源。inotify + 常時インターバルのハイブリッド。
/// 読み取り側が冪等なので、シグナルは coalesce(容量 1、あふれは破棄)する。
pub struct WakeSource {
    _watcher: Option<RecommendedWatcher>,
    pub rx: mpsc::Receiver<()>,
}

pub fn wake_source(dir: &Path, interval: Duration) -> WakeSource {
    let (tx, rx) = mpsc::channel(1);

    let notify_tx = tx.clone();
    let watcher = (|| -> notify::Result<RecommendedWatcher> {
        let mut w = notify::recommended_watcher(move |_res: notify::Result<notify::Event>| {
            let _ = notify_tx.try_send(());
        })?;
        w.watch(dir, RecursiveMode::NonRecursive)?;
        Ok(w)
    })();
    let watcher = match watcher {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!("inotify unavailable, relying on polling only: {e}");
            None
        }
    };

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if tx.is_closed() {
                break;
            }
            let _ = tx.try_send(());
        }
    });

    WakeSource { _watcher: watcher, rx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn wakes_on_file_write() {
        let dir = tempfile::tempdir().unwrap();
        // インターバルを長くして inotify 経路で起きることを確認
        let mut ws = wake_source(dir.path(), Duration::from_secs(60));
        std::fs::write(dir.path().join("Journal.x.log"), "x\n").unwrap();
        tokio::time::timeout(Duration::from_secs(5), ws.rx.recv())
            .await
            .expect("should wake within 5s")
            .expect("channel should be open");
    }

    #[tokio::test]
    async fn wakes_on_interval_without_fs_events() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = wake_source(dir.path(), Duration::from_millis(50));
        tokio::time::timeout(Duration::from_secs(5), ws.rx.recv())
            .await
            .expect("should tick")
            .expect("channel should be open");
    }
}
