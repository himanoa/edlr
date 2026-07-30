//! wasmtime `Engine` の所有と epoch ticker。`PluginHost`/`DriverHost` の
//! どちらも同じ形(`Config` に `wasm_component_model`/`epoch_interruption` を
//! 立て、バックグラウンドスレッドで epoch を刻む)で使うため、ここへ集約する。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use wasmtime::{Config, Engine};

/// Interval between epoch ticks driven by the background ticker thread.
pub(crate) const EPOCH_TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Owns the wasmtime `Engine` and a background thread that periodically
/// increments the engine's epoch counter, driving epoch-interruption-based
/// call deadlines for every plugin/driver instance loaded from the owning
/// host.
///
/// 意図的に `Drop` を実装しない: フィールドの drop は所有者(`PluginHost`/
/// `DriverHost`)の `Drop::drop` 本体が終わったあとに走るため、ここに
/// `Drop` を持たせると「ticker 停止 → `process_driver.stop_all()`」という
/// 現行の順序が反転してしまう。ticker の停止は各ホストの `Drop::drop` から
/// 明示的に `stop_ticker()` を呼んで行う。
pub(crate) struct EpochEngine {
    engine: Engine,
    ticker_stop: Arc<AtomicBool>,
}

impl EpochEngine {
    pub(crate) fn new() -> anyhow::Result<EpochEngine> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config)
            .map_err(|e| anyhow::anyhow!("failed to create wasmtime engine: {e}"))?;

        let ticker_stop = Arc::new(AtomicBool::new(false));
        let ticker_engine = engine.clone();
        let ticker_stop_flag = ticker_stop.clone();
        thread::spawn(move || {
            while !ticker_stop_flag.load(Ordering::Relaxed) {
                thread::sleep(EPOCH_TICK_INTERVAL);
                ticker_engine.increment_epoch();
            }
        });

        Ok(EpochEngine { engine, ticker_stop })
    }

    pub(crate) fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Stops the background ticker thread. Idempotent (the thread simply
    /// observes the flag and exits on its next wake-up).
    pub(crate) fn stop_ticker(&self) {
        self.ticker_stop.store(true, Ordering::Relaxed);
    }
}

/// Number of epoch ticks corresponding to `duration`, rounded up, with a
/// minimum of one tick so a zero-length deadline still traps promptly.
pub(crate) fn deadline_ticks(duration: Duration) -> u64 {
    let ticks = duration.as_nanos().div_ceil(EPOCH_TICK_INTERVAL.as_nanos());
    u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
}
