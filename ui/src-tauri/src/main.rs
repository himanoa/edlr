#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;

use std::path::PathBuf;

/// 未起動ならデーモンを spawn して Child を返す。起動済み・失敗時は None。
fn autostart_daemon() -> Option<std::process::Child> {
    if daemon::daemon_running(daemon::DAEMON_ADDR) {
        eprintln!(
            "edlr daemon already running on {}; leaving it alone",
            daemon::DAEMON_ADDR
        );
        return None;
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from));
    // 開発ビルドのみ: リポジトリ内の target/debug/edlr を最後の候補にする
    let dev_fallback = if cfg!(debug_assertions) {
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/edlr"))
    } else {
        None
    };
    let bin = daemon::resolve_edlr_bin(
        std::env::var_os("EDLR_BIN").map(PathBuf::from),
        exe_dir.as_deref(),
        daemon::find_in_path("edlr"),
        dev_fallback,
    );
    let Some(bin) = bin else {
        eprintln!(
            "edlr binary not found (set EDLR_BIN or put edlr on PATH); starting UI without daemon"
        );
        return None;
    };
    let journal_dir = std::env::var_os("EDLR_JOURNAL_DIR").map(PathBuf::from);
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

fn main() {
    // ウィンドウを出してフロントエンドを表示する薄い皮 + デーモンの道連れ起動。
    // 既に起動済みのデーモンには spawn も kill もしない。
    let mut child = autostart_daemon();
    tauri::Builder::default()
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |_app, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(mut c) = child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        });
}
