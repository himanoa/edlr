use clap::Parser;
use edlr_core::driver::host::DriverHost;
use edlr_core::driver::start_drivers;
use edlr_core::plugin::filesystem::FilesystemConfigStore;
use edlr_core::plugin::host::PluginHost;
use edlr_core::plugin::sidecar::SidecarConfigStore;
use edlr_core::plugin::{start_plugins, GrantsStore, SettingsStore};
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

    /// プラグイン capability 承認の保存先ディレクトリ(未指定時は
    /// $XDG_CONFIG_HOME/edlr/grants、未設定なら ~/.config/edlr/grants)
    #[arg(long)]
    grants_dir: Option<PathBuf>,

    /// ドライバディレクトリ(未指定時は $XDG_CONFIG_HOME/edlr/drivers、
    /// 未設定なら ~/.config/edlr/drivers)。存在しなくてもエラーには
    /// ならない(ドライバ 0 件として起動する)
    #[arg(long)]
    drivers_dir: Option<PathBuf>,

    /// Journal 読み取り位置の保存先ディレクトリ(未指定時は
    /// $XDG_STATE_HOME/edlr、未設定なら ~/.local/state/edlr)
    #[arg(long)]
    state_dir: Option<PathBuf>,
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
    let grants_dir = args.grants_dir.clone().unwrap_or_else(|| {
        config::config_subdir(xdg_config_home.as_deref(), home.as_deref(), "grants")
    });
    let drivers_dir = args.drivers_dir.clone().unwrap_or_else(|| {
        config::config_subdir(xdg_config_home.as_deref(), home.as_deref(), "drivers")
    });
    let state_dir = args.state_dir.clone().unwrap_or_else(|| {
        let xdg_state_home = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
        config::state_base(xdg_state_home.as_deref(), home.as_deref())
    });
    let positions =
        std::sync::Arc::new(edlr_core::journal::position::PositionStore::new(state_dir));

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

    // プラグインホストの起動に失敗しても(wasmtime エンジン初期化失敗など)
    // デーモン本体は止めない。プラグイン機能なしで継続する。
    // `registry` は RPC 経由でのプラグイン一覧・設定操作に消費する想定で
    // ここに保持しておく。
    //
    // 各プラグインの load/init は同期・ブロッキングであり(`start_plugins`
    // は最後のプラグインの起動結果が確定するまで戻らない)、これを直接
    // await すると tokio のワーカースレッドを長時間専有してしまう。
    // `spawn_blocking` で専用スレッドに逃がし、非同期ランタイムをブロック
    // しないようにする。また、`monitor::run` がイベントを配信し始める前に
    // プラグインの購読を確立できるよう、`monitor::run` の起動より先に完了
    // させる。
    let router_for_plugins = router.clone();
    let registry_and_drivers = match (PluginHost::new(), DriverHost::new()) {
        (Ok(host), Ok(driver_host)) => {
            tracing::info!(
                plugins_dir = %plugins_dir.display(),
                drivers_dir = %drivers_dir.display(),
                "starting plugins"
            );
            let settings_store = SettingsStore::new(settings_dir.clone());
            let grants_store = GrantsStore::new(grants_dir.clone());
            let sidecar_config_store = SidecarConfigStore::new(settings_dir.clone());
            // edlr 自身の状態ディレクトリを承認先に選ばせない。ここでは
            // 既定値ではなく、CLI(`--settings-dir` など)で上書きされた
            // 実際のパスを渡す — そうしないと、既定と違う場所を使っている
            // デーモンでその実際の場所が無防備になる。ドライバのディレクトリ
            // もプラグイン同様に「掴ませない」対象に加える -- そうしないと
            // プラグインがファイルシステムルートとしてドライバディレクトリ
            // 自体を要求でき、他のドライバの設定/データを覗ける。
            let filesystem_config_store = FilesystemConfigStore::new(
                settings_dir.clone(),
                vec![grants_dir.clone(), plugins_dir.clone(), drivers_dir.clone()],
            );
            let plugins_dir_for_blocking = plugins_dir.clone();

            // ドライバ用のストアはプラグインとは別の名前空間に置く
            // (`settings_dir/drivers`、`grants_dir/drivers`)。プラグイン id
            // とドライバ id は互いに衝突しうる(共有する名前空間ではない)
            // ため、同じディレクトリを共用してはならない。
            let driver_settings_store = SettingsStore::new(settings_dir.join("drivers"));
            let driver_grants_store = GrantsStore::new_for_drivers(grants_dir.clone());
            let driver_sidecar_config_store =
                SidecarConfigStore::new(settings_dir.join("drivers"));
            // ドライバにも、プラグインのディレクトリ・自身のドライバ
            // ディレクトリをファイルシステムルートとして掴ませない。
            let driver_filesystem_config_store = FilesystemConfigStore::new(
                settings_dir.join("drivers"),
                vec![grants_dir.clone(), plugins_dir.clone(), drivers_dir.clone()],
            );

            let bus = edlr_driver_channel::Bus::new();
            let drivers_dir_for_blocking = drivers_dir.clone();
            match tokio::task::spawn_blocking(move || {
                let drivers = start_drivers(
                    &drivers_dir_for_blocking,
                    driver_settings_store,
                    driver_sidecar_config_store,
                    driver_filesystem_config_store,
                    driver_grants_store,
                    bus.clone(),
                    driver_host,
                );
                let registry = start_plugins(
                    &plugins_dir_for_blocking,
                    settings_store,
                    sidecar_config_store,
                    filesystem_config_store,
                    grants_store,
                    &router_for_plugins,
                    bus,
                    drivers.clone(),
                    host,
                );
                (registry, drivers)
            })
            .await
            {
                Ok((registry, drivers)) => Some((registry, drivers)),
                Err(e) => {
                    tracing::warn!("plugin startup task panicked, continuing without plugins: {e}");
                    None
                }
            }
        }
        (Err(e), _) => {
            tracing::warn!("failed to initialize plugin host, continuing without plugins: {e}");
            None
        }
        (_, Err(e)) => {
            tracing::warn!("failed to initialize driver host, continuing without plugins: {e}");
            None
        }
    };

    // `DriverRegistry` は `Clone`(内部は `Arc` 共有)で安価に持ち回れるため、
    // プラグイン側に配線したのと同じインスタンスをそのまま `ServerState` にも
    // 渡す(`server::bus_result_json` のドキュメント参照: 本番ではこの 2 つが
    // 同じ `DriverRegistry` を指すことを前提にしている)。
    let (registry, drivers) = match registry_and_drivers {
        Some((registry, drivers)) => (Some(registry), Some(drivers)),
        None => (None, None),
    };

    // `drivers` は `ServerState` にも渡すが、shutdown シーケンス(下記)でも
    // `stop_all_sidecars` を呼ぶために手元に残しておく必要があるので、渡す
    // のは複製(`DriverRegistry` も `Clone` = 内部は `Arc` 共有で安価)。
    let state = server::ServerState::new(&router, registry.clone(), drivers.clone());
    tokio::spawn(server::serve(listener, state, args.ui_dir.clone()));

    let mut rx = router.subscribe();

    tokio::spawn(monitor::run(
        dir,
        router.clone(),
        Duration::from_millis(args.poll_interval_ms),
        Some(positions),
    ));

    // SIGTERM/SIGINT ハンドラ。ハンドラを一切登録しないままだと、デーモンは
    // これらのシグナルに対してデフォルト動作(即死)をする -- サイドカーの
    // 後始末(`stop_all_sidecars`)が一切走らないまま終了し、`Ctrl-C` や
    // Tauri 側からの通常の停止経路でサイドカーが孤児として残ってしまう
    // (このブランチの最終レビューで見つかった Critical な取りこぼし)。
    // ここで拾って明示的な shutdown シーケンス(下の `stop_all_sidecars`)に
    // 合流させることで、`rx.recv()` が `Closed` で抜ける経路と同じ後始末を
    // 通す。Unix 専用(`signal(SignalKind::terminate())`)。このドライバ自体
    // (`drivers/process`)が既に `std::os::unix::process::CommandExt` を
    // 無条件に使っており、このホストは元々 Unix 専用であるため問題ない。
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to install SIGTERM handler");
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .expect("failed to install SIGINT handler");

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
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
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, shutting down");
                break;
            }
            _ = sigint.recv() => {
                tracing::info!("received SIGINT, shutting down");
                break;
            }
        }
    }

    // デーモン終了経路: 生きているサイドカーを確実に停止し、バス購読タスクに
    // shutdown を知らせてから抜ける。
    //
    // **`shutdown_bus_subscribers` を `main` が返る(= `#[tokio::main]` が
    // 暗黙の `Runtime` を drop する)前に呼ぶことが必須**: `[[bus]] subscribe`
    // を宣言するプラグインが 1 つでもあれば、その購読転送タスク
    // (`crate::plugin::runner::spawn_bus_subscriber`)は `spawn_blocking` の
    // 中でブロッキング受信をしており、これを呼ばずに `Runtime` を drop する
    // と `Runtime::drop` がそのタスクの完了を無期限に待ち、プロセスが
    // SIGTERM/SIGINT を受けても終了できなくなる(実際に踏んだ Critical
    // バグ。`Registry::shutdown_bus_subscribers` のドキュメントコメント参照)。
    // `stop_all_sidecars` より先に呼ぶ必要はない(両者は独立)が、どのみち
    // 同じ `registry` を消費する `spawn_blocking` 呼び出しより前に、軽い
    // 同期呼び出しとして済ませておく。
    //
    // `ProcessDriver::stop_all`(`PluginHost::drop` 経由でも最後の砦として
    // 呼ばれる)は、まだ稼働中の `Registry`/`PluginHost` を保持したままの
    // 明示的な shutdown シーケンスの一部として、ここでも呼んでおく。
    //
    // `stop_all_sidecars`(同期版 `ProcessDriver::stop_all`)は SIGTERM を
    // 無視するサイドカーがいれば `shutdown_grace`(既定 3 秒)×インスタンス数
    // ブロックしうる。ここは非同期ランタイムの最後の一手なので tokio の
    // ワーカースレッドを塞がないよう `spawn_blocking` に逃がしてから待つ。
    if let Some(registry) = registry {
        registry.shutdown_bus_subscribers();
        let _ = tokio::task::spawn_blocking(move || registry.stop_all_sidecars()).await;
    }

    // **Critical: 最終レビューで見つかった取りこぼし。** 上のプラグイン側
    // `stop_all_sidecars` だけでは、ドライバのサイドカー(設計書の動機である
    // VOICEVOX のような合成エンジンそのもの)が孤児として残る -- `registry`
    // (プラグイン)と `drivers`(`DriverRegistry`)はそれぞれ別の `ProcessDriver`
    // インスタンス(`PluginHost`/`DriverHost` がそれぞれ own する)を持つ、
    // 独立したサイドカー集合だからである。デーモンを終了するたびに VOICEVOX
    // が 1 プロセスずつ増え、次回起動時にポートを取り合って新しいインスタンス
    // が bind に失敗する、という形で実害が出る(`core/tests/
    // daemon_signal_shutdown_integration.rs` の
    // `sigterm_to_daemon_stops_running_driver_sidecars` がこの回帰を防ぐ)。
    //
    // ドライバ専用スレッド(`crate::driver::runner::run_driver_thread`)自体を
    // 明示的に止める必要は無い: プラグインの `spawn_bus_subscriber` と違い、
    // これは(`tokio::task::spawn_blocking` ではなく)素の `std::thread::spawn`
    // で動いており、`Runtime::drop` はこのスレッドの完了を待たない。つまり
    // `for message in messages_rx` で無期限にブロックしたままでもプロセス終了
    // 自体は妨げない(このスレッドが `Bus::DriverSlot` に居座る送信側のせいで
    // 自然終了しないのは事実だが、それは shutdown を妨げる要因ではなく、単に
    // `DriverHost` の `Arc` が最後まで drop されず、その `Drop`(最後の砦の
    // `stop_all`)が発火しないというだけ -- だからこそ、ここで
    // `DriverRegistry::stop_all_sidecars` を明示的に呼ぶ必要がある)。
    if let Some(drivers) = drivers {
        let _ = tokio::task::spawn_blocking(move || drivers.stop_all_sidecars()).await;
    }
}
