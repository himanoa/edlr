//! http/process/fs ドライバの共有 `Arc` 3 点。`PluginHost`/`DriverHost` の
//! どちらも同じ値(fs の読み取り上限・一覧件数上限・サイドカーの停止猶予・
//! spawn 最短間隔)で構築するため、その組み立てをここへ集約する。HTTP の
//! タイムアウトだけは呼び出し元(プラグイン 1.5 秒 / ドライバ 25 秒)で
//! 異なるため、引数として受け取る。

use std::sync::Arc;
use std::time::Duration;

/// `edlr_driver_http::HttpDriver::send` が返せる最大レスポンスボディ長。
/// `crate::host::plugin::HTTP_MAX_BODY` / `crate::host::driver::HTTP_MAX_BODY`
/// と同値(呼び出し元にも同名の公開定数が残っている)。
const HTTP_MAX_BODY: usize = 8 * 1024 * 1024;

/// `driver-fs` の 1 回の読み取り上限。`HTTP_MAX_BODY` と同値。
const FS_READ_LIMIT: usize = HTTP_MAX_BODY;

/// `list` が返すエントリ数の上限。
const FS_LIST_LIMIT: usize = 10_000;

/// サイドカー停止時、SIGTERM から SIGKILL へ昇格するまでの猶予。
const SIDECAR_SHUTDOWN_GRACE: Duration =
    Duration::from_secs(edlr_config::SIDECAR_SHUTDOWN_GRACE_SECS);

/// 同一サイドカーの spawn 試行の最短間隔。
const SIDECAR_SPAWN_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// `PluginHost`/`DriverHost` が 1 つずつ持ち、全プラグイン/ドライバ
/// インスタンスで共有する http/process/fs の 3 ドライバ。
pub(crate) struct SharedDrivers {
    http: Arc<edlr_driver_http::HttpDriver>,
    process: Arc<edlr_driver_process::ProcessDriver>,
    fs: Arc<edlr_driver_fs::FsDriver>,
}

impl SharedDrivers {
    pub(crate) fn new(http_timeout: Duration) -> anyhow::Result<SharedDrivers> {
        let http = Arc::new(
            edlr_driver_http::HttpDriver::new(http_timeout, HTTP_MAX_BODY)
                .map_err(|e| anyhow::anyhow!("failed to build http driver: {e}"))?,
        );
        let process = Arc::new(edlr_driver_process::ProcessDriver::new(
            SIDECAR_SHUTDOWN_GRACE,
            SIDECAR_SPAWN_MIN_INTERVAL,
        ));
        let fs = Arc::new(edlr_driver_fs::FsDriver::new(FS_READ_LIMIT, FS_LIST_LIMIT));

        Ok(SharedDrivers { http, process, fs })
    }

    /// Returns a clone of the shared `HttpDriver` `Arc`. Cloning an `Arc` is
    /// cheap; this does not build a new HTTP client.
    pub(crate) fn http(&self) -> Arc<edlr_driver_http::HttpDriver> {
        self.http.clone()
    }

    /// Returns a clone of the shared `ProcessDriver` `Arc`. Cloning an `Arc`
    /// is cheap; this does not spawn or otherwise touch any sidecar process.
    pub(crate) fn process(&self) -> Arc<edlr_driver_process::ProcessDriver> {
        self.process.clone()
    }

    /// Returns a clone of the shared `FsDriver` `Arc`. Cloning an `Arc` is
    /// cheap; this does not touch any file.
    pub(crate) fn fs(&self) -> Arc<edlr_driver_fs::FsDriver> {
        self.fs.clone()
    }
}
