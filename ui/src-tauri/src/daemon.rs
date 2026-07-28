use std::io;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

pub const DAEMON_ADDR: &str = "127.0.0.1:8137";

/// デーモンの shutdown シーケンスに含まれる、独立したサイドカー集合の数。
///
/// `core/src/bin/edlr.rs` の終了経路は `stop_all_sidecars` を 2 回、逐次に
/// 呼ぶ: プラグイン側(`Registry`)とドライバ側(`DriverRegistry`)は
/// それぞれ別の `ProcessDriver` インスタンスを持つ独立した集合であるため。
/// 各集合の内部では全インスタンスが 1 つの猶予を共有して並行に停止される
/// (`drivers/process` の `kill_and_wait_all`)ので、集合ごとの最悪時間は
/// インスタンス数によらず `SIDECAR_SHUTDOWN_GRACE_SECS` 1 回ぶん。
const SIDECAR_SETS_STOPPED_SEQUENTIALLY: u64 = 2;

/// SIGTERM を送ってから SIGKILL へフォールバックするまでの既定の猶予。
///
/// **`core::plugin::host::SIDECAR_SHUTDOWN_GRACE`(= サイドカー停止 1 回分の
/// 猶予)と同じ値にしてはいけない**。以前はここが `SIDECAR_SHUTDOWN_GRACE` と
/// 同じ 3 秒になっていた(最終レビューで見つかった Critical な取りこぼし:
/// SIGTERM を無視するサイドカーが 1 つあるだけで、Tauri がデーモンを SIGKILL
/// するタイミングとデーモンがそのサイドカーへ `killpg(SIGKILL)` するタイミング
/// がちょうど競合し、Tauri 側が先に負けてデーモンを道連れに殺してしまい、
/// サイドカーが孤児として残っていた)。
///
/// `STOP_GRACE` はデーモン側の後始末の最悪時間を**厳密に超える**必要がある。
/// その内訳(いずれも `edlr_config` の共有定数):
///
/// - `SIDECAR_SHUTDOWN_GRACE_SECS × SIDECAR_SETS_STOPPED_SEQUENTIALLY` --
///   プラグイン側とドライバ側のサイドカー集合を逐次に止める。**集合の中では
///   全インスタンスが 1 つの猶予を共有する**(`kill_and_wait_all`)ので、
///   インスタンス数には比例しない
/// - `DRIVER_CALL_DEADLINE_SECS` -- ドライバはメッセージ 1 件の処理中に
///   HTTP 呼び出し等でブロックしうり、その間は停止要求にも応答できない。
///   ドライバ専用スレッドはメッセージループを直列に回しているため、
///   ブロック中の呼び出しが `DriverInstance::CALL_DEADLINE` に達して戻るまで
///   次のシャットダウン処理へ進めない
/// - `PLUGIN_ON_STOP_GRACE_SECS` -- 全 Running プラグインの on-stop 完了待ち。
///   `Registry::shutdown_plugins` は停止要求を全件へ先に送ってから 1 つの
///   共有デッドラインで join するので、こちらもプラグイン数に比例しない
///
/// かつては `shutdown_plugins` も `ProcessDriver::finish_stop` も 1 件ずつ
/// 逐次に待っていたため、この見積もりに "想定インスタンス数上限"(20)が
/// 2 箇所も掛かり、`STOP_GRACE` は 200 秒必要だった -- 最悪ケースでデスクトップ
/// アプリが終了時に数分ハングして見える値である。両方を並行化したことで、
/// 見積もりから台数依存が消えた。
///
/// この関係は下のコンパイル時アサーションで固定してある: いずれかの定数だけが
/// 将来変更されても、この関係が崩れればビルドが失敗する。デーモンが早く終了
/// した場合、`stop_child_gracefully` は `try_wait` のポーリングで即座に戻る
/// (この猶予いっぱい待つのは、デーモンが本当にハングしている最悪ケースのみ)。
pub const STOP_GRACE: Duration = Duration::from_secs(60);

/// `STOP_GRACE` が超えていなければならないデーモン側の最悪後始末時間。
/// 内訳は `STOP_GRACE` のドキュメントコメント参照。
const DAEMON_WORST_CASE_SHUTDOWN_SECS: u64 = edlr_config::SIDECAR_SHUTDOWN_GRACE_SECS
    * SIDECAR_SETS_STOPPED_SEQUENTIALLY
    + edlr_config::DRIVER_CALL_DEADLINE_SECS
    + edlr_config::PLUGIN_ON_STOP_GRACE_SECS;

const _: () = assert!(
    STOP_GRACE.as_secs() > DAEMON_WORST_CASE_SHUTDOWN_SECS,
    "STOP_GRACE must strictly exceed the daemon's worst-case shutdown time: two \
     sequential sidecar-set stops (each set stops its instances concurrently under \
     one shared grace), plus one driver call deadline (a driver blocked in an HTTP \
     call cannot answer a stop request until its deadline elapses), plus one plugin \
     on-stop grace (Registry::shutdown_plugins signals every plugin first, then joins \
     them all against a single shared deadline) -- see STOP_GRACE's doc comment."
);

/// 子プロセスを SIGTERM で止め、`grace` 待って死ななければ SIGKILL に
/// フォールバックする。`Child::kill()`(= SIGKILL)を無条件に呼ぶのは
/// この関数内の最後の手段としてのみ。
///
/// デーモンにはシグナルハンドラが無いままだと `Child::kill()`(SIGKILL)で
/// 即死させることになり、デーモンが `stop_all_sidecars` を実行する猶予を
/// 一切与えられない -- 稼働中のサイドカーが孤児として残る(このブランチの
/// 最終レビューで見つかった Critical な取りこぼし)。デーモン側に
/// SIGTERM/SIGINT ハンドラを足した(`core/src/bin/edlr.rs`)のと対で、
/// Tauri 側もここで SIGTERM をまず送り、後始末の時間を与える。
///
/// SAFETY: `child.id()` は自分が `spawn` した(まだ `wait` していない)
/// プロセスの PID であり、シグナル送信のみで所有権は奪わない。
pub fn stop_child_gracefully(child: &mut Child, grace: Duration) {
    let pid = child.id() as libc::pid_t;
    // SAFETY: see the doc comment above.
    let sigterm_sent = unsafe { libc::kill(pid, libc::SIGTERM) } == 0;
    if sigterm_sent {
        let deadline = Instant::now() + grace;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {}
                Err(_) => break,
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// addr に TCP 接続できればデーモン生存とみなす。
pub fn daemon_running(addr: &str) -> bool {
    match addr.parse::<SocketAddr>() {
        Ok(a) => TcpStream::connect_timeout(&a, Duration::from_millis(300)).is_ok(),
        Err(_) => false,
    }
}

/// PATH から実行ファイルを探す。
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// 探索順: env_bin(無条件)→ exe_dir の edlr → PATH ヒット → dev fallback(実在時のみ)。
/// PATH 探索・環境変数の読み取りは呼び出し側で行い、ここは順序決定のみを担う。
pub fn resolve_edlr_bin(
    env_bin: Option<PathBuf>,
    exe_dir: Option<&Path>,
    path_hit: Option<PathBuf>,
    dev_fallback: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(p) = env_bin {
        return Some(p);
    }
    if let Some(dir) = exe_dir {
        let candidate = dir.join("edlr");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Some(p) = path_hit {
        return Some(p);
    }
    dev_fallback.filter(|p| p.is_file())
}

/// デーモンを子プロセスとして起動する(stdout/stderr は継承)。
pub fn spawn_daemon(bin: &Path, journal_dir: Option<&Path>) -> io::Result<Child> {
    let mut cmd = Command::new(bin);
    if let Some(dir) = journal_dir {
        cmd.arg("--journal-dir").arg(dir);
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// Regression test for a Critical finding from the re-review: `STOP_GRACE`
    /// used to equal `SIDECAR_SHUTDOWN_GRACE` exactly, so a single
    /// SIGTERM-ignoring sidecar could make Tauri SIGKILL the daemon at the
    /// exact moment (or just before) the daemon itself escalated to
    /// `killpg(SIGKILL)` on that sidecar -- with 2+ stubborn instances, Tauri
    /// was guaranteed to lose the race and orphan them. This mirrors the
    /// compile-time assertion next to `STOP_GRACE` in a form that shows up in
    /// the test suite too, and pins the actual numbers rather than just the
    /// relation, so a change to either shared constant is visible here.
    ///
    /// The worst case no longer scales with the number of sidecar instances or
    /// plugins: `ProcessDriver::stop_all` and `Registry::shutdown_plugins` both
    /// signal everything first and then wait against a single shared deadline.
    /// That is what let `STOP_GRACE` drop from 200s to 60s.
    #[test]
    fn stop_grace_exceeds_the_daemons_worst_case_shutdown_time() {
        assert!(
            STOP_GRACE.as_secs() > DAEMON_WORST_CASE_SHUTDOWN_SECS,
            "STOP_GRACE ({STOP_GRACE:?}) must strictly exceed the daemon's worst-case \
             shutdown time ({DAEMON_WORST_CASE_SHUTDOWN_SECS}s = \
             SIDECAR_SHUTDOWN_GRACE_SECS * SIDECAR_SETS_STOPPED_SEQUENTIALLY + \
             DRIVER_CALL_DEADLINE_SECS + PLUGIN_ON_STOP_GRACE_SECS); otherwise Tauri \
             can SIGKILL the daemon before it has finished stopping every sidecar (or \
             before a driver blocked in a call can respond, or before every plugin's \
             on-stop hook has run), orphaning any sidecars that ignore SIGTERM"
        );
    }

    /// The per-set worst case must still leave room for the sidecar grace
    /// itself -- pins the actual numbers, not just the relation, so a change to
    /// either shared constant shows up in the test suite.
    #[test]
    fn daemon_worst_case_accounts_for_both_sidecar_sets_and_the_driver_deadline() {
        assert_eq!(
            DAEMON_WORST_CASE_SHUTDOWN_SECS,
            3 * 2 + 30 + 5,
            "the shared shutdown constants changed; re-check STOP_GRACE"
        );
    }

    #[test]
    fn daemon_running_detects_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        assert!(daemon_running(&addr));
        drop(listener);
        assert!(!daemon_running(&addr));
        assert!(!daemon_running("not an addr"));
    }

    fn make_exec(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn resolve_prefers_env_bin_unconditionally() {
        let p = std::path::PathBuf::from("/nonexistent/edlr");
        assert_eq!(resolve_edlr_bin(Some(p.clone()), None, None, None), Some(p));
    }

    #[test]
    fn resolve_order_is_exe_dir_then_path_then_dev_fallback() {
        let exe_dir = tempfile::tempdir().unwrap();
        let sibling = make_exec(exe_dir.path(), "edlr");
        let path_hit = std::path::PathBuf::from("/from/path/edlr");
        let dev = tempfile::tempdir().unwrap();
        let dev_bin = make_exec(dev.path(), "edlr");

        // exe_dir の edlr が最優先
        assert_eq!(
            resolve_edlr_bin(
                None,
                Some(exe_dir.path()),
                Some(path_hit.clone()),
                Some(dev_bin.clone())
            ),
            Some(sibling)
        );
        // exe_dir に無ければ PATH ヒット
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_edlr_bin(
                None,
                Some(empty.path()),
                Some(path_hit.clone()),
                Some(dev_bin.clone())
            ),
            Some(path_hit)
        );
        // PATH にも無ければ dev fallback(実在する場合のみ)
        assert_eq!(
            resolve_edlr_bin(None, Some(empty.path()), None, Some(dev_bin.clone())),
            Some(dev_bin)
        );
        assert_eq!(
            resolve_edlr_bin(
                None,
                Some(empty.path()),
                None,
                Some(std::path::PathBuf::from("/nonexistent"))
            ),
            None
        );
    }

    #[test]
    fn find_in_path_finds_sh() {
        let sh = find_in_path("sh").expect("sh should be on PATH");
        assert!(sh.is_file());
        assert_eq!(find_in_path("edlr-definitely-not-a-real-binary"), None);
    }

    /// `fs::write` でスクリプトを書いてから `Command::spawn` で実行するテストの
    /// 共通ヘルパー。ETXTBSY (`ExecutableFileBusy`) が出た場合はリトライする。
    ///
    /// `fs::write` はファイルディスクリプタを閉じてから返るが、このテスト
    /// モジュールは複数のテストが**同一プロセスの別スレッド**で並行に走る。
    /// あるスレッド A がスクリプトを書き込んでいる(まだ書き込み用 fd を
    /// クローズし切っていない)最中に、別スレッド B が `Command::spawn` で
    /// fork すると、B が fork した子プロセスは A の書き込み用 fd をそのまま
    /// 継承する。fork はプロセス全体の fd テーブルを複製するため、A のファイル
    /// への書き込み用 fd が「A のスレッドだけのもの」ではなく「プロセス全体で
    /// 開いている fd」としてカーネルに見えてしまうのが原因。この状態で A が
    /// 自分のスクリプトを `execve` しようとすると、カーネルは「このファイルは
    /// 書き込み用に開かれている」と判断して `ETXTBSY`(Text file busy)で
    /// exec を拒否する。
    ///
    /// これはテスト対象コード(`stop_child_gracefully` など)のバグではなく、
    /// マルチスレッドなテストハーネスから fork/exec すること自体に内在する
    /// レースである。したがって「直す」ことはできず、リトライで吸収するのが
    /// 正しい緩和策になる。将来だれかが「たまたま通ったから」とリトライを
    /// 単純化・削除しないよう、ここに理由を残しておく。
    fn spawn_script_retrying_etxtbsy(path: &std::path::Path) -> Child {
        retry_spawn_on_etxtbsy(path, || Command::new(path).spawn())
    }

    /// `spawn_script_retrying_etxtbsy` の中身を切り出した汎用版。
    ///
    /// `spawn_daemon_passes_journal_dir_and_child_can_be_killed` のように、
    /// 生の `Command::new(..).spawn()` ではなく本番コードの `spawn_daemon`
    /// 経由で同じスクリプトを実行するテストも同一の ETXTBSY レースに
    /// 晒されている(実際に本修正の検証中に再現した)ため、`spawn` を渡す
    /// クロージャで抽象化して両方から使えるようにしてある。
    fn retry_spawn_on_etxtbsy(
        path: &std::path::Path,
        mut spawn: impl FnMut() -> io::Result<Child>,
    ) -> Child {
        const MAX_ATTEMPTS: u32 = 10;
        for attempt in 1..=MAX_ATTEMPTS {
            match spawn() {
                Ok(child) => return child,
                Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy => {
                    if attempt == MAX_ATTEMPTS {
                        panic!(
                            "giving up after {MAX_ATTEMPTS} attempts: {path:?} still \
                             ETXTBSY (Text file busy) -- this is the classic \
                             multithreaded fork/exec race where another test thread's \
                             write fd to its own script was inherited by our fork; if \
                             this ever reproduces reliably, look for a widened window \
                             around fs::write(...).unwrap() + spawn(), not a bug in the \
                             code under test"
                        );
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("failed to spawn {path:?}: {e}"),
            }
        }
        unreachable!("loop always returns or panics");
    }

    /// Regression test for the "SIGKILL orphans sidecars" finding: a
    /// SIGTERM-ignoring child must still end up dead after
    /// `stop_child_gracefully`, via the SIGKILL fallback, and the call must
    /// actually wait out (at least) the grace period rather than returning
    /// early.
    #[test]
    fn stop_child_gracefully_falls_back_to_sigkill_for_a_stubborn_child() {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let script = dir.path().join("stubborn");
        // `ready` を作ってから sleep する: trap の設定より先に SIGTERM が
        // 届くと(spawn 直後は default action のまま)、意図せずデフォルトの
        // 終了動作で死んでしまい、SIGKILL フォールバックを検証できない
        // レースがあるため、trap 設定後の合図を待ってから送る。
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ntrap '' TERM\ntouch {}\nsleep 30\n",
                ready.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let mut child = spawn_script_retrying_etxtbsy(&script);
        let pid = child.id() as libc::pid_t;

        for _ in 0..200 {
            if ready.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let started = Instant::now();
        stop_child_gracefully(&mut child, Duration::from_millis(300));
        let elapsed = started.elapsed();

        assert!(
            elapsed >= Duration::from_millis(300),
            "must wait out the grace period before escalating to SIGKILL; took {elapsed:?}"
        );
        // SAFETY: existence check only, no signal sent.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(
            !alive,
            "stubborn child must be dead after the SIGKILL fallback"
        );
    }

    /// A child that honors SIGTERM should not make `stop_child_gracefully`
    /// wait out the full grace period -- otherwise every ordinary shutdown
    /// would be needlessly slow.
    #[test]
    fn stop_child_gracefully_returns_promptly_when_sigterm_is_honored() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("polite");
        fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let mut child = spawn_script_retrying_etxtbsy(&script);

        let started = Instant::now();
        // grace は意図的に長め(10 秒)にしておく: これより十分短い時間で
        // 戻ることを確認できれば、「猶予いっぱい待たされた」のではなく
        // SIGTERM で早期に終了したと言える。しきい値(5 秒)自体にも
        // 余裕を持たせ、workspace 全体のテストを並列実行したときの
        // スケジューラの揺らぎで誤って FAILED にならないようにしている。
        stop_child_gracefully(&mut child, Duration::from_secs(10));
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(5),
            "must not wait out the full grace period when SIGTERM is honored; took {elapsed:?}"
        );
    }

    #[test]
    fn spawn_daemon_passes_journal_dir_and_child_can_be_killed() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-edlr");
        // 引数をファイルに書いてから sleep する偽デーモン
        fs::write(
            &script,
            "#!/bin/sh\necho \"$@\" > \"$(dirname \"$0\")/args.txt\"\nsleep 30\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let jdir = dir.path().join("journal");
        let mut child = retry_spawn_on_etxtbsy(&script, || spawn_daemon(&script, Some(&jdir)));
        // args.txt が書かれるまで少し待つ
        for _ in 0..50 {
            if dir.path().join("args.txt").exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let args = fs::read_to_string(dir.path().join("args.txt")).unwrap();
        assert!(args.contains("--journal-dir"));
        assert!(args.contains("journal"));
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
