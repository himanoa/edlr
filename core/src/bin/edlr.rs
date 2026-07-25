use clap::Parser;
use edlr_core::plugin::host::PluginHost;
use edlr_core::plugin::{start_plugins, SettingsStore};
use edlr_core::{config, monitor, router::Router, server};
use std::path::PathBuf;
use std::time::Duration;

/// EliteDangerousLogRouter daemon
#[derive(Parser)]
#[command(name = "edlr", version)]
struct Args {
    /// Journal ディレクトリ(未指定時は既知パスを探索)
    #[arg(long)]
    journal_dir: Option<PathBuf>,

    /// ポーリング間隔(ミリ秒)
    #[arg(long, default_value_t = 1000)]
    poll_interval_ms: u64,

    /// HTTP/WebSocket サーバの listen アドレス
    #[arg(long, default_value = "127.0.0.1:8137")]
    listen: std::net::SocketAddr,

    /// UI 静的ファイルのディレクトリ(指定時のみ配信)
    #[arg(long)]
    ui_dir: Option<PathBuf>,

    /// プラグインディレクトリ(未指定時は $XDG_CONFIG_HOME/edlr/plugins、
    /// 未設定なら ~/.config/edlr/plugins)。存在しなくてもエラーにはならない
    /// (プラグイン 0 件として起動する)
    #[arg(long)]
    plugins_dir: Option<PathBuf>,

    /// プラグイン設定の保存先ディレクトリ(未指定時は $XDG_CONFIG_HOME/edlr/settings、
    /// 未設定なら ~/.config/edlr/settings)
    #[arg(long)]
    settings_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();

    let dir = args.journal_dir.or_else(|| {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        config::default_journal_dir(&home)
    });
    let Some(dir) = dir else {
        eprintln!("error: journal directory not found; specify one with --journal-dir <PATH>");
        std::process::exit(1);
    };

    if !dir.is_dir() {
        eprintln!("error: journal directory does not exist: {}", dir.display());
        std::process::exit(1);
    }

    tracing::info!("watching {}", dir.display());
    let router = Router::new(256);

    let home = std::env::var_os("HOME").map(PathBuf::from);
    let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let plugins_dir = args.plugins_dir.clone().unwrap_or_else(|| {
        config::config_subdir(xdg_config_home.as_deref(), home.as_deref(), "plugins")
    });
    let settings_dir = args.settings_dir.clone().unwrap_or_else(|| {
        config::config_subdir(xdg_config_home.as_deref(), home.as_deref(), "settings")
    });

    if let Some(ui_dir) = &args.ui_dir {
        if !ui_dir.is_dir() {
            eprintln!("error: --ui-dir {} is not a directory", ui_dir.display());
            std::process::exit(1);
        }
    }
    let listener = match tokio::net::TcpListener::bind(args.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: failed to bind {}: {e}", args.listen);
            std::process::exit(1);
        }
    };
    tracing::info!("http/ws server listening on {}", args.listen);
    let state = server::ServerState::new(&router);
    tokio::spawn(server::serve(listener, state, args.ui_dir.clone()));

    let mut rx = router.subscribe();

    // プラグインホストの起動に失敗しても(wasmtime エンジン初期化失敗など)
    // デーモン本体は止めない。プラグイン機能なしで継続する。
    // `_registry` は後続の Plan B(RPC 経由でのプラグイン一覧・設定操作)が
    // 消費する想定でここに保持しておく。
    //
    // 各プラグインの load/init は同期・ブロッキングであり(`start_plugins`
    // は最後のプラグインの起動結果が確定するまで戻らない)、これを直接
    // await すると tokio のワーカースレッドを長時間専有してしまう。
    // `spawn_blocking` で専用スレッドに逃がし、非同期ランタイムをブロック
    // しないようにする。また、`monitor::run` がイベントを配信し始める前に
    // プラグインの購読を確立できるよう、`monitor::run` の起動より先に完了
    // させる。
    let router_for_plugins = router.clone();
    let _registry = match PluginHost::new() {
        Ok(host) => {
            tracing::info!(plugins_dir = %plugins_dir.display(), "starting plugins");
            let settings_store = SettingsStore::new(settings_dir);
            let plugins_dir_for_blocking = plugins_dir.clone();
            match tokio::task::spawn_blocking(move || {
                start_plugins(
                    &plugins_dir_for_blocking,
                    settings_store,
                    &router_for_plugins,
                    host,
                )
            })
            .await
            {
                Ok(registry) => Some(registry),
                Err(e) => {
                    tracing::warn!("plugin startup task panicked, continuing without plugins: {e}");
                    None
                }
            }
        }
        Err(e) => {
            tracing::warn!("failed to initialize plugin host, continuing without plugins: {e}");
            None
        }
    };

    tokio::spawn(monitor::run(
        dir,
        router.clone(),
        Duration::from_millis(args.poll_interval_ms),
    ));

    loop {
        match rx.recv().await {
            Ok(event) => {
                let json = match &*event {
                    edlr_core::event::Event::Journal { raw, .. } => raw.to_string(),
                    edlr_core::event::Event::Status { raw } => raw.to_string(),
                };
                println!("{json}");
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("stdout consumer lagged, dropped {n} events");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}
