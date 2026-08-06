#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod config;
mod daemon;
mod devserver;
mod signals;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// デーモンのバイナリを探す。探索順は daemon::resolve_edlr_bin に委ねる。
fn resolve_bin() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from));
    let dev_fallback = if cfg!(debug_assertions) {
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/edlr"))
    } else {
        None
    };
    daemon::resolve_edlr_bin(
        std::env::var_os("EDLR_BIN").map(PathBuf::from),
        exe_dir.as_deref(),
        daemon::find_in_path("edlr"),
        dev_fallback,
    )
}

/// 未起動ならデーモンを spawn する。
///
/// 戻り値は `Child`(spawn できた場合のみ)と、どう扱ったかを表す
/// `DaemonStartup`。`Child` の有無だけでは「外部で動いているので触らない」と
/// 「起動に失敗した」を区別できないため、後者を呼び出し元へ明示的に伝える。
///
/// `journal_dir` は呼び出し元(`main`)が `config::resolve_journal_dir` で
/// env と設定ファイルを解決済みの実効値。restart / display と同じ解決
/// ロジックを一箇所に集約するため、ここでは重ねて解決しない。
fn autostart_daemon(
    journal_dir: Option<PathBuf>,
) -> (Option<std::process::Child>, config::DaemonStartup) {
    if daemon::daemon_running(daemon::DAEMON_ADDR) {
        eprintln!(
            "edlr daemon already running on {}; leaving it alone",
            daemon::DAEMON_ADDR
        );
        return (None, config::DaemonStartup::External);
    }
    let Some(bin) = resolve_bin() else {
        let reason = "edlr binary not found (set EDLR_BIN or put edlr on PATH)".to_string();
        eprintln!("{reason}; starting UI without daemon");
        return (None, config::DaemonStartup::Failed(reason));
    };
    match daemon::spawn_daemon(&bin, journal_dir.as_deref()) {
        Ok(child) => {
            eprintln!(
                "spawned edlr daemon (pid {}) from {}",
                child.id(),
                bin.display()
            );
            (Some(child), config::DaemonStartup::Spawned)
        }
        Err(e) => {
            let reason = format!("failed to spawn edlr daemon from {}: {e}", bin.display());
            eprintln!("{reason}");
            (None, config::DaemonStartup::Failed(reason))
        }
    }
}

/// Tauri が保持するアプリ状態。
///
/// `daemon` を `Arc` で包むのは、`tauri::Builder::build` が失敗したときに
/// `main` 側からもデーモンを停止できるようにするため。`state` は `manage` へ
/// ムーブされてしまうので、同じ `Arc` のクローンを `main` に残しておく。
struct AppState {
    /// Tauri が spawn したデーモン。外部起動のデーモンを掴んでいる場合は None。
    daemon: Arc<Mutex<Option<std::process::Child>>>,
    /// このアプリがデーモンのライフサイクルに責任を持つか。
    ///
    /// `main` 起動時に一度だけ決まり、以降は不変。`daemon` スロットが一時的に
    /// `None`(再起動中の spawn 失敗など)になっても倒れない、
    /// `daemonManaged` の「外部起動のデーモンには触らない」という意味を保つための値。
    owns_daemon: bool,
    config_path: PathBuf,
    config: Mutex<edlr_config::AppConfig>,
    config_error: Mutex<Option<String>>,
    /// デーモンが起動していない理由(起動時の spawn 失敗)。
    ///
    /// 再起動に成功したらクリアする。残したままだと、ユーザーが設定を直して
    /// デーモンが動き出した後も古いエラーが出続けてしまう。
    daemon_error: Mutex<Option<String>>,
    /// `EDLR_JOURNAL_DIR` の値。`main` 起動時に一度だけ読み、以降は不変。
    ///
    /// spawn(起動時)・restart(`set_journal_dir`)・display(`snapshot`)の
    /// 3 箇所全てがこの値と `config.journal_dir` を
    /// `config::resolve_journal_dir` に通して実効値を求める。3 箇所が
    /// 別々に env を読んだり読まなかったりすると、UI の表示・再起動時の
    /// ディレクトリ・次回起動時の spawn 先が食い違いうるため、
    /// 1 箇所で読んで共有することでその不変条件を保証する。
    env_journal_dir: Option<PathBuf>,
}

/// デーモンを停止して回収する。
///
/// SIGTERM を送って `daemon::STOP_GRACE` 待ち、デーモンが自ら
/// `stop_all_sidecars` を終えて終了する猶予を与える。それでも死ななければ
/// `Child::kill()`(SIGKILL)にフォールバックする。デーモン側に
/// SIGTERM/SIGINT ハンドラを足す前は、ここが無条件に `Child::kill()` を
/// 呼んでいたため、デーモンが後始末する隙もなく即死し、稼働中のサイドカーが
/// 孤児として残っていた(Critical: 最終レビューで見つかった取りこぼし)。
fn stop_child(child: &mut std::process::Child) {
    daemon::stop_child_gracefully(child, daemon::STOP_GRACE);
}

/// 保持しているデーモンを停止し、`journal_dir` で再 spawn する。
///
/// スロットが既に `None`(前回の spawn 失敗など)でもエラーにはせず、
/// そのまま新しく spawn する。バイナリ解決はスロットを触る前に行うため、
/// バイナリが見つからない場合に生きているデーモンを巻き込んで殺すことはない。
fn restart_daemon(
    slot: &Mutex<Option<std::process::Child>>,
    journal_dir: Option<&Path>,
) -> Result<(), String> {
    let bin = resolve_bin().ok_or_else(|| "edlr binary not found".to_string())?;

    let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(mut old) = guard.take() {
        signals::set_daemon_pid(None);
        stop_child(&mut old);
    }

    let child = daemon::spawn_daemon(&bin, journal_dir)
        .map_err(|e| format!("failed to spawn edlr daemon: {e}"))?;
    signals::set_daemon_pid(Some(child.id()));
    *guard = Some(child);
    Ok(())
}

fn snapshot(state: &AppState) -> config::ConfigDto {
    let config = state.config.lock().unwrap_or_else(|p| p.into_inner());
    let error = state.config_error.lock().unwrap_or_else(|p| p.into_inner());
    let resolved =
        config::resolve_journal_dir(state.env_journal_dir.clone(), config.journal_dir.clone());
    config::ConfigDto {
        journal_dir: resolved.map(|p| p.to_string_lossy().to_string()),
        configured_journal_dir: config
            .journal_dir
            .clone()
            .map(|p| p.to_string_lossy().to_string()),
        daemon_managed: state.owns_daemon,
        config_error: error.clone(),
        daemon_error: state
            .daemon_error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone(),
        env_override: state.env_journal_dir.is_some(),
    }
}

#[tauri::command]
fn get_config(state: tauri::State<'_, AppState>) -> config::ConfigDto {
    snapshot(&state)
}

/// 再起動したデーモンが実際に応答するようになるまで待つ。
///
/// `spawn()` の成否は生存を意味しない(自動検出が外れてもフォールバック
/// journal ディレクトリを作成して起動を続けるため、単に自動検出に失敗した
/// だけではもう `exit(1)` しないが、ポート競合やフォールバック用ディレクトリ
/// の作成失敗など他の要因では直後に落ちうる)。listen し始めたかどうかで
/// 判定する。
fn wait_until_listening(attempts: u32) -> bool {
    for _ in 0..attempts {
        if daemon::daemon_running(daemon::DAEMON_ADDR) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    false
}

/// 設定を保存し、Tauri 管理下のデーモンを実効値で再起動する共通処理。
///
/// `set_journal_dir` と `clear_journal_dir` は保存する中身が違うだけで
/// 手順は同じなので、ここに集約する(片方だけ直して不変条件がずれるのを防ぐ)。
///
/// - 外部起動のデーモンを掴んでいる場合は保存のみ行い、再起動しない
/// - 再起動に失敗しても保存はロールバックしない(ユーザーの正しい入力を消さない)
/// - 再起動できても応答しなければ `daemon_error` を残す
fn apply_config(
    state: &AppState,
    updated: edlr_config::AppConfig,
    fail_prefix: &str,
) -> Result<(), String> {
    updated
        .save(&state.config_path)
        .map_err(|e| format!("設定の保存に失敗しました: {e}"))?;

    let journal_dir = updated.journal_dir.clone();
    {
        let mut guard = state.config.lock().unwrap_or_else(|p| p.into_inner());
        *guard = updated;
    }
    {
        let mut guard = state.config_error.lock().unwrap_or_else(|p| p.into_inner());
        *guard = None;
    }

    if !state.owns_daemon {
        return Ok(());
    }

    // 保存した値そのものではなく、env を含めた実効値で再起動する。
    // `snapshot` の表示・次回起動時の spawn 先と常に同じ値になるように。
    let resolved = config::resolve_journal_dir(state.env_journal_dir.clone(), journal_dir);
    restart_daemon(&state.daemon, resolved.as_deref())
        .map_err(|e| format!("{fail_prefix}ただしデーモンの再起動に失敗しました: {e}"))?;

    // spawn できただけでは足りない。応答を確認できて初めてエラーを消す。
    let came_up = wait_until_listening(10);
    *state.daemon_error.lock().unwrap_or_else(|p| p.into_inner()) =
        config::daemon_error_after_restart(came_up);
    Ok(())
}

/// journal_dir を検証・保存し、Tauri 管理下のデーモンを再起動する。
#[tauri::command(async)]
fn set_journal_dir(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<config::ConfigDto, String> {
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(format!("ディレクトリが存在しません: {path}"));
    }

    apply_config(
        &state,
        edlr_config::AppConfig {
            journal_dir: Some(dir),
        },
        "設定は保存されました。",
    )?;

    Ok(snapshot(&state))
}

/// 設定ファイルの journal_dir を消し、デーモンの自動検出へ戻す。
///
/// `set_journal_dir` に空文字を渡す形にしなかったのは、あちらの
/// `is_dir()` 検証と意味が衝突するため。「消す」は別の操作として明示する。
///
/// デーモンが起動に失敗している状態からの再試行にもこれを使う
/// (実体は「現在の実効値で spawn し直す」であり同じ操作)。
#[tauri::command(async)]
fn clear_journal_dir(state: tauri::State<'_, AppState>) -> Result<config::ConfigDto, String> {
    apply_config(
        &state,
        edlr_config::AppConfig { journal_dir: None },
        "設定は消去されました。",
    )?;

    Ok(snapshot(&state))
}

/// ネイティブのディレクトリ選択ダイアログを開く。キャンセル時は None。
#[tauri::command]
async fn pick_journal_dir(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    rx.await.ok().flatten().map(|path| path.to_string())
}

/// ネイティブのファイル選択ダイアログを開く(サイドカーの実行ファイル用)。
/// キャンセル時は None。
#[tauri::command]
async fn pick_executable(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_file(move |picked| {
        let _ = tx.send(picked);
    });
    rx.await.ok().flatten().map(|path| path.to_string())
}

/// ネイティブのディレクトリ選択ダイアログを開く(プラグインのファイル
/// アクセス設定用)。キャンセル時は None。
#[tauri::command]
async fn pick_directory(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |picked| {
        let _ = tx.send(picked);
    });
    rx.await.ok().flatten().map(|path| path.to_string())
}

/// ネイティブのファイル選択ダイアログを開く(プラグインのファイル
/// アクセス設定で target = "file" のルート用)。キャンセル時は None。
#[tauri::command]
async fn pick_file(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_file(move |picked| {
        let _ = tx.send(picked);
    });
    rx.await.ok().flatten().map(|path| path.to_string())
}

fn main() {
    // ウィンドウを出してフロントエンドを表示する薄い皮 + デーモンの道連れ起動。
    // 既に起動済みのデーモンには spawn も kill もしない。
    //
    // デバッグビルドは WebView が `devUrl`(vite の 5173)を読むため、
    // vite も道連れ起動する(`devserver` モジュール参照)。`tauri dev` 経由や
    // 手動起動の vite がいる場合は spawn しない。リリースビルドは
    // `frontendDist` 同梱なので不要。
    // シグナルで死ぬ経路でも道連れ子(vite・デーモン)を片付けられるよう、
    // 子を spawn する前にハンドラを仕込む(`signals` モジュール参照)。
    signals::install();

    let dev_server = if cfg!(debug_assertions) {
        devserver::ensure_dev_server()
    } else {
        None
    };
    let dev_server = Arc::new(Mutex::new(dev_server));

    let loaded = config::load_from_env();
    if let Some(error) = &loaded.error {
        eprintln!("failed to load {}: {error}", loaded.path.display());
    }
    // env_journal_dir はここで一度だけ読み、以降は AppState 経由で使い回す。
    // spawn(ここ)・restart(set_journal_dir)・display(snapshot)が同じ値を
    // 見るようにするため(3 箇所がそれぞれ env を読むと食い違いうる)。
    let env_journal_dir = std::env::var_os("EDLR_JOURNAL_DIR").map(PathBuf::from);
    let resolved_journal_dir =
        config::resolve_journal_dir(env_journal_dir.clone(), loaded.config.journal_dir.clone());
    let (child, startup) = autostart_daemon(resolved_journal_dir);
    signals::set_daemon_pid(child.as_ref().map(|c| c.id()));
    let owns_daemon = startup.owns_daemon();
    let daemon_error = startup.error();

    // state は manage へムーブされるため、停止用にこのハンドルを手元へ残す。
    let daemon = Arc::new(Mutex::new(child));

    let state = AppState {
        daemon: Arc::clone(&daemon),
        owns_daemon,
        config_path: loaded.path,
        config: Mutex::new(loaded.config),
        config_error: Mutex::new(loaded.error),
        daemon_error: Mutex::new(daemon_error),
        env_journal_dir,
    };

    let app = match tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            set_journal_dir,
            clear_journal_dir,
            pick_journal_dir,
            pick_executable,
            pick_directory,
            pick_file
        ])
        .build(tauri::generate_context!())
    {
        Ok(app) => app,
        Err(e) => {
            kill_daemon(&daemon);
            kill_dev_server(&dev_server);
            eprintln!("error while building tauri application: {e}");
            std::process::exit(1);
        }
    };

    app.run(move |_app, event| {
        if let tauri::RunEvent::Exit = event {
            kill_daemon(&daemon);
            kill_dev_server(&dev_server);
        }
    });
}

/// 保持しているデーモンがあれば停止する(終了時・ビルド失敗時)。
fn kill_daemon(slot: &Mutex<Option<std::process::Child>>) {
    let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(mut child) = guard.take() {
        signals::set_daemon_pid(None);
        stop_child(&mut child);
    }
}

/// 道連れ起動した vite dev サーバがあれば停止する(終了時・ビルド失敗時)。
fn kill_dev_server(slot: &Mutex<Option<std::process::Child>>) {
    let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(mut child) = guard.take() {
        devserver::stop_dev_server(&mut child, devserver::STOP_GRACE);
    }
}
