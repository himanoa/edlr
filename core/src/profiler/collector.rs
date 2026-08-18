//! sink と collector スレッド(命令的: チャネル・スレッド・ファイル IO)。
//! `Ring` と `Sample` は純粋(→ super::bucket / super::sample)だが、共有
//! (`Mutex`)・チャネル・ディスク書き込み・スレッド生成はすべてこちらに置く。
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::bucket::Ring;
use super::Sample;

const CHANNEL_CAPACITY: usize = 4096;

enum CollectorMsg {
    Sample(Sample),
    Shutdown,
}

/// `Profiler::start` の中身。collector スレッドを持つ実体。
struct Inner {
    tx: SyncSender<CollectorMsg>,
    lost: Arc<AtomicU64>,
    ring: Arc<Mutex<Ring>>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

enum ProfilerImpl {
    Live(Arc<Inner>),
    Noop(Arc<Mutex<Ring>>),
}

#[derive(Clone)]
pub struct Profiler {
    inner: ProfilerImpl,
}

impl Clone for ProfilerImpl {
    fn clone(&self) -> Self {
        match self {
            ProfilerImpl::Live(inner) => ProfilerImpl::Live(Arc::clone(inner)),
            ProfilerImpl::Noop(ring) => ProfilerImpl::Noop(Arc::clone(ring)),
        }
    }
}

impl Profiler {
    /// `profiler_dir` (`<state-base>/profiler`) に JSONL を追記する collector
    /// スレッドを起動する。
    pub fn start(profiler_dir: PathBuf) -> Profiler {
        let (tx, rx) = sync_channel::<CollectorMsg>(CHANNEL_CAPACITY);
        let lost = Arc::new(AtomicU64::new(0));
        let ring = Arc::new(Mutex::new(Ring::new()));

        let handle = {
            let lost = Arc::clone(&lost);
            let ring = Arc::clone(&ring);
            std::thread::spawn(move || collector_loop(rx, ring, lost, profiler_dir))
        };

        Profiler {
            inner: ProfilerImpl::Live(Arc::new(Inner {
                tx,
                lost,
                ring,
                handle: Mutex::new(Some(handle)),
            })),
        }
    }

    /// テスト・未配線環境用の null sink。スレッドもファイルも作らない。
    pub fn noop() -> Profiler {
        Profiler {
            inner: ProfilerImpl::Noop(Arc::new(Mutex::new(Ring::new()))),
        }
    }

    /// `try_send`。満杯なら捨てて lost を数える。noop なら何もしない。
    pub fn record(&self, sample: Sample) {
        let ProfilerImpl::Live(inner) = &self.inner else {
            return;
        };
        if inner.tx.try_send(CollectorMsg::Sample(sample)).is_err() {
            inner.lost.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn ring(&self) -> Arc<Mutex<Ring>> {
        match &self.inner {
            ProfilerImpl::Live(inner) => Arc::clone(&inner.ring),
            ProfilerImpl::Noop(ring) => Arc::clone(ring),
        }
    }

    /// センチネルで collector スレッドを止めて flush・join する。
    /// 複数回呼んでも 2 回目以降は何もしない(handle が既に取り出し済み)。
    pub fn shutdown(&self) {
        let ProfilerImpl::Live(inner) = &self.inner else {
            return;
        };
        let _ = inner.tx.try_send(CollectorMsg::Shutdown);
        let handle = inner.handle.lock().unwrap().take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }
}

/// 次の秒境界までの待ち時間。
fn duration_until_next_second_boundary() -> Duration {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let subsec = now.subsec_nanos() as u64;
    Duration::from_nanos(1_000_000_000u64.saturating_sub(subsec))
}

fn now_secs_f64() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn today_stamp() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 指定日付のファイルを開く(失敗したら None、collector_loop 側で warn する)。
fn open_dated_file(dir: &std::path::Path, stamp: &str) -> std::io::Result<File> {
    std::fs::create_dir_all(dir)?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{stamp}.jsonl")))
}

/// 書き込み先の状態。エラーが出たら以降は諦めて `None` を保持する。
struct Sink {
    writer: Option<BufWriter<File>>,
    stamp: String,
    warned: bool,
}

impl Sink {
    fn open(dir: &std::path::Path) -> Sink {
        let stamp = today_stamp();
        let writer = match open_dated_file(dir, &stamp) {
            Ok(f) => Some(BufWriter::new(f)),
            Err(e) => {
                tracing::warn!("profiler: failed to open sink file: {e}");
                None
            }
        };
        Sink {
            writer,
            stamp,
            warned: false,
        }
    }

    fn reopen_if_date_changed(&mut self, dir: &std::path::Path) {
        let stamp = today_stamp();
        if stamp == self.stamp {
            return;
        }
        self.stamp = stamp;
        self.writer = match open_dated_file(dir, &self.stamp) {
            Ok(f) => Some(BufWriter::new(f)),
            Err(e) => {
                if !self.warned {
                    tracing::warn!("profiler: failed to open sink file: {e}");
                    self.warned = true;
                }
                None
            }
        };
    }

    fn write_line(&mut self, line: &str) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        if writeln!(writer, "{line}").is_err() && !self.warned {
            tracing::warn!("profiler: failed to write sink line, giving up on this file");
            self.warned = true;
            self.writer = None;
        }
    }

    fn flush(&mut self) {
        let Some(writer) = self.writer.as_mut() else {
            return;
        };
        if writer.flush().is_err() && !self.warned {
            tracing::warn!("profiler: failed to flush sink file, giving up on this file");
            self.warned = true;
            self.writer = None;
        }
    }
}

/// SinkLost を合成して ring + sink へ書く(秒境界処理をここ 1 箇所に集約。
/// gauge 走査は Task 4 でこの関数の呼び出し元に足す)。
fn emit_second_boundary(ring: &Arc<Mutex<Ring>>, sink: &mut Sink, dir: &std::path::Path, lost: u64) {
    let sample = Sample::SinkLost {
        ts: now_secs_f64(),
        lost,
    };
    ring.lock().unwrap().insert(&sample);
    sink.write_line(&sample.to_jsonl_line());
    sink.flush();
    sink.reopen_if_date_changed(dir);
}

fn collector_loop(
    rx: std::sync::mpsc::Receiver<CollectorMsg>,
    ring: Arc<Mutex<Ring>>,
    lost: Arc<AtomicU64>,
    dir: PathBuf,
) {
    let mut sink = Sink::open(&dir);
    loop {
        match rx.recv_timeout(duration_until_next_second_boundary()) {
            Ok(CollectorMsg::Sample(sample)) => {
                ring.lock().unwrap().insert(&sample);
                sink.write_line(&sample.to_jsonl_line());
            }
            Ok(CollectorMsg::Shutdown) => {
                emit_second_boundary(&ring, &mut sink, &dir, lost.load(Ordering::Relaxed));
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                emit_second_boundary(&ring, &mut sink, &dir, lost.load(Ordering::Relaxed));
            }
            Err(RecvTimeoutError::Disconnected) => {
                emit_second_boundary(&ring, &mut sink, &dir, lost.load(Ordering::Relaxed));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiler::{CallKind, CallSample, Outcome, Subject};

    fn sample_call(ts: f64) -> Sample {
        Sample::Call(CallSample {
            ts,
            subject: Subject::Plugin,
            id: "p1".into(),
            call: CallKind::OnEvent,
            detail: "E".into(),
            duration_us: 10,
            outcome: Outcome::Ok,
        })
    }

    #[test]
    fn recorded_samples_land_in_the_ring_and_the_jsonl_file() {
        let tmp = tempfile::tempdir().unwrap();
        let profiler = Profiler::start(tmp.path().join("profiler"));
        profiler.record(sample_call(100.0));
        // 秒境界の flush を待つ
        std::thread::sleep(std::time::Duration::from_millis(1500));
        profiler.shutdown();

        let ring = profiler.ring();
        let ring = ring.lock().unwrap();
        assert_eq!(ring.keys().len(), 1);

        let files: Vec<_> = std::fs::read_dir(tmp.path().join("profiler"))
            .unwrap()
            .collect();
        assert_eq!(files.len(), 1);
        let content = std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
        assert!(content.lines().any(|l| l.contains("\"on-event\"")));
    }

    #[test]
    fn a_noop_profiler_records_nothing_and_never_blocks() {
        let profiler = Profiler::noop();
        for _ in 0..10_000 {
            profiler.record(sample_call(1.0));
        }
        assert_eq!(profiler.ring().lock().unwrap().keys().len(), 0);
    }

    #[test]
    fn shutdown_flushes_pending_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let profiler = Profiler::start(tmp.path().join("profiler"));
        profiler.record(sample_call(100.0));
        profiler.shutdown(); // flush 待ちなしで即 shutdown
        let files: Vec<_> = std::fs::read_dir(tmp.path().join("profiler"))
            .unwrap()
            .collect();
        let content = std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
        assert!(!content.is_empty());
    }
}
