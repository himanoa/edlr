#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;
mod config;

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

/// 未起動ならデーモンを spawn して Child を返す。起動済み・失敗時は None。
fn autostart_daemon(config_journal_dir: Option<PathBuf>) -> Option<std::process::Child> {
    if daemon::daemon_running(daemon::DAEMON_ADDR) {
        eprintln!(
            "edlr daemon already running on {}; leaving it alone",
            daemon::DAEMON_ADDR
        );
        return None;
    }
    let Some(bin) = resolve_bin() else {
        eprintln!(
            "edlr binary not found (set EDLR_BIN or put edlr on PATH); starting UI without daemon"
        );
        return None;
    };
    let journal_dir = config::resolve_journal_dir(
        std::env::var_os("EDLR_JOURNAL_DIR").map(PathBuf::from),
        config_journal_dir,
    );
    match daemon::spawn_daemon(&bin, journal_dir.as_deref()) {
        Ok(child) => {
            eprintln!(
                "spawned edlr daemon (pid {}) from {}",
                child.id(),
                bin.display()
            );
            Some(child)
        }
        Err(e) => {
            eprintln!("failed to spawn edlr daemon from {}: {e}", bin.display());
            None
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
}

/// デーモンを停止して回収する。`Child::kill()`(SIGKILL)を呼ぶ唯一の場所。
///
/// 将来サイドカーを導入する際は、ここだけを SIGTERM + プロセスグループ化へ
/// 差し替えれば済むようにしてある(設計書「スコープ外」の前提条件を参照)。
fn stop_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
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
        stop_child(&mut old);
    }

    let child = daemon::spawn_daemon(&bin, journal_dir)
        .map_err(|e| format!("failed to spawn edlr daemon: {e}"))?;
    *guard = Some(child);
    Ok(())
}

fn snapshot(state: &AppState) -> config::ConfigDto {
    let config = state.config.lock().unwrap_or_else(|p| p.into_inner());
    let error = state.config_error.lock().unwrap_or_else(|p| p.into_inner());
    config::ConfigDto {
        journal_dir: config
            .journal_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
        daemon_managed: state.owns_daemon,
        config_error: error.clone(),
    }
}

#[tauri::command]
fn get_config(state: tauri::State<'_, AppState>) -> config::ConfigDto {
    snapshot(&state)
}

/// journal_dir を検証・保存し、Tauri 管理下のデーモンを再起動する。
///
/// 外部起動のデーモンを掴んでいる場合は保存のみ行い、再起動しない
/// (`daemonManaged: false` を返して UI 側に反映を促す)。
/// 再起動に失敗しても保存はロールバックしない。ユーザーが入力した正しい値まで
/// 失われるため。
#[tauri::command]
fn set_journal_dir(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<config::ConfigDto, String> {
    let dir = PathBuf::from(&path);
    if !dir.is_dir() {
        return Err(format!("ディレクトリが存在しません: {path}"));
    }

    let updated = edlr_config::AppConfig {
        journal_dir: Some(dir.clone()),
    };
    updated
        .save(&state.config_path)
        .map_err(|e| format!("設定の保存に失敗しました: {e}"))?;

    {
        let mut guard = state.config.lock().unwrap_or_else(|p| p.into_inner());
        *guard = updated;
    }
    {
        let mut guard = state.config_error.lock().unwrap_or_else(|p| p.into_inner());
        *guard = None;
    }

    if state.owns_daemon {
        restart_daemon(&state.daemon, Some(&dir))?;
    }

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

fn main() {
    // ウィンドウを出してフロントエンドを表示する薄い皮 + デーモンの道連れ起動。
    // 既に起動済みのデーモンには spawn も kill もしない。
    let loaded = config::load_from_env();
    if let Some(error) = &loaded.error {
        eprintln!("failed to load {}: {error}", loaded.path.display());
    }
    let child = autostart_daemon(loaded.config.journal_dir.clone());
    let owns_daemon = child.is_some();

    // state は manage へムーブされるため、停止用にこのハンドルを手元へ残す。
    let daemon = Arc::new(Mutex::new(child));

    let state = AppState {
        daemon: Arc::clone(&daemon),
        owns_daemon,
        config_path: loaded.path,
        config: Mutex::new(loaded.config),
        config_error: Mutex::new(loaded.error),
    };

    let app = match tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            get_config,
            set_journal_dir,
            pick_journal_dir
        ])
        .build(tauri::generate_context!())
    {
        Ok(app) => app,
        Err(e) => {
            kill_daemon(&daemon);
            eprintln!("error while building tauri application: {e}");
            std::process::exit(1);
        }
    };

    app.run(move |_app, event| {
        if let tauri::RunEvent::Exit = event {
            kill_daemon(&daemon);
        }
    });
}

/// 保持しているデーモンがあれば停止する(終了時・ビルド失敗時)。
fn kill_daemon(slot: &Mutex<Option<std::process::Child>>) {
    let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(mut child) = guard.take() {
        stop_child(&mut child);
    }
}
