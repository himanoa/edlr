//! SIGINT/SIGTERM で道連れ起動した子プロセスを片付けるハンドラ。
//!
//! ウィンドウを閉じる通常経路は `RunEvent::Exit`(`main.rs` の
//! `kill_daemon`/`kill_dev_server`)が後始末するが、シグナルで死ぬ経路では
//! そこを通らない。特に:
//!
//! - vite dev サーバは自分専用のプロセスグループにいる
//!   (`devserver::spawn_in_own_process_group`)ため、端末の Ctrl-C
//!   (フォアグラウンドグループへの SIGINT)すら届かず、孤児として 5173 を
//!   握り続ける(実際に検証中に踏んだ)。
//! - デーモンは同じグループにいるので Ctrl-C は自力で処理できるが、
//!   `kill <edlr-ui の PID>` のように親だけを狙われると孤児になる。
//!
//! ハンドラ内で使ってよいのは async-signal-safe な操作のみ。ここでは
//! atomic の load と `killpg`/`kill`/`signal`/`raise` に限定している。

use std::sync::atomic::{AtomicI32, Ordering};

/// 道連れで止める vite のプロセスグループ ID。0 = 未設定。
static DEV_SERVER_PGID: AtomicI32 = AtomicI32::new(0);

/// 道連れで止めるデーモンの PID。0 = 未設定(外部起動のデーモンには触らない)。
static DAEMON_PID: AtomicI32 = AtomicI32::new(0);

/// ハンドラ本体: 子へ SIGTERM を転送してから、既定動作に戻して自分も同じ
/// シグナルで死ぬ(async-signal-safe な定石)。デーモンは SIGTERM を自前の
/// ハンドラで拾ってサイドカーまで後始末する(`core/src/bin/edlr.rs`)。
extern "C" fn forward_to_children(sig: libc::c_int) {
    let pgid = DEV_SERVER_PGID.load(Ordering::Relaxed);
    if pgid > 0 {
        // SAFETY: 自分が spawn した子のプロセスグループへの送信のみ。
        unsafe {
            libc::killpg(pgid, libc::SIGTERM);
        }
    }
    let daemon = DAEMON_PID.load(Ordering::Relaxed);
    if daemon > 0 {
        // SAFETY: 自分が spawn した(まだ wait していない)子への送信のみ。
        unsafe {
            libc::kill(daemon, libc::SIGTERM);
        }
    }
    // SAFETY: 既定動作へ戻して再送出する。
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}

/// SIGINT/SIGTERM ハンドラを登録する。`main` の起動時に一度だけ呼ぶ。
pub fn install() {
    // SAFETY: 関数ポインタの登録のみ。ハンドラが async-signal-safe な操作
    // しかしないことは `forward_to_children` 側で保証する。
    unsafe {
        libc::signal(
            libc::SIGINT,
            forward_to_children as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            forward_to_children as *const () as libc::sighandler_t,
        );
    }
}

/// vite を spawn したら呼ぶ(PGID = spawn した子の PID)。
pub fn set_dev_server_pgid(pgid: u32) {
    DEV_SERVER_PGID.store(pgid as i32, Ordering::Relaxed);
}

/// デーモンを spawn / 再起動 / 停止したら呼ぶ。`None` = 道連れ対象なし
/// (外部起動のデーモンを掴んでいる場合や停止済みの場合)。
pub fn set_daemon_pid(pid: Option<u32>) {
    DAEMON_PID.store(pid.map_or(0, |p| p as i32), Ordering::Relaxed);
}
