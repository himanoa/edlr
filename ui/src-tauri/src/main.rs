#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[allow(dead_code)]
mod daemon;

fn main() {
    // ウィンドウを出してフロントエンドを表示するだけの薄い皮。
    // デーモンへの接続はフロントエンド側の WebSocket が担う。
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
