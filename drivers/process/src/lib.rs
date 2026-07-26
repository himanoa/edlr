//! プラグインのサイドカープロセスを起動・監視・確実に停止するドライバ。
//!
//! プロセスの所有権はこのドライバ(= ホスト)にあり、呼び出し元(プラグイン)は
//! PID もハンドルも受け取らない。停止は必ずプロセスグループ単位で行うため、
//! サイドカーが更に子を作っていても孤児が残らない。

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// 起動するプロセスの仕様。`args` 内の `{port}` は各インスタンスの実ポートに展開される。
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessSpec {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub ports: Vec<u16>,
}

/// 1 インスタンスの現在状態。
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceStatus {
    pub index: u32,
    pub port: u16,
    pub running: bool,
    pub exit_code: Option<i32>,
}

#[derive(Debug)]
pub enum ProcessError {
    /// 直近の spawn 試行から `spawn_min_interval` が経過していない。
    RateLimited(String),
    /// 実行ファイルが無い、権限が無い等で spawn 自体に失敗した。
    Spawn(String),
}

impl std::fmt::Display for ProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessError::RateLimited(msg) => write!(f, "{msg}"),
            ProcessError::Spawn(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ProcessError {}

/// 起動済みインスタンス 1 件。`child` が `None` なら終了済み。
///
/// `terminating` は「`stop`/`stop_all` が子プロセスを取り出してロック外で
/// killpg 中」を表す。この間 `child` は `None` になるが、それだけでは
/// `ensure_started` から「未起動」と区別できず二重 spawn してしまうため、
/// 別フラグとして持つ(詳細は `take_for_stop` / `finish_stop` を参照)。
struct Instance {
    index: u32,
    port: u16,
    child: Option<Child>,
    exit_code: Option<i32>,
    terminating: bool,
}

/// 1 サイドカー(= 1 key)分のインスタンス群と、直近の spawn 試行時刻。
struct Group {
    instances: Vec<Instance>,
    last_spawn_attempt: Option<Instant>,
}

pub struct ProcessDriver {
    groups: Mutex<HashMap<String, Group>>,
    shutdown_grace: Duration,
    spawn_min_interval: Duration,
}

impl ProcessDriver {
    pub fn new(shutdown_grace: Duration, spawn_min_interval: Duration) -> ProcessDriver {
        ProcessDriver {
            groups: Mutex::new(HashMap::new()),
            shutdown_grace,
            spawn_min_interval,
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Group>> {
        self.groups
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 生きていないインスタンスだけを spawn し、直後の状態を返す。
    /// 既に全て生きていればレート制限にも掛からず、そのまま成功する。
    pub fn ensure_started(
        &self,
        key: &str,
        spec: &ProcessSpec,
    ) -> Result<Vec<InstanceStatus>, ProcessError> {
        let mut groups = self.lock();
        let group = groups.entry(key.to_string()).or_insert_with(|| Group {
            instances: Vec::new(),
            last_spawn_attempt: None,
        });

        reap(group);
        align_instances(group, spec);

        // `terminating` なインスタンスは `stop`/`stop_all` がロック外で
        // killpg 中のもの。まだ `child` は `None` だが respawn 対象ではない。
        let needs_spawn = group
            .instances
            .iter()
            .any(|i| i.child.is_none() && !i.terminating);
        if !needs_spawn {
            return Ok(snapshot(group));
        }

        if let Some(last) = group.last_spawn_attempt {
            let elapsed = last.elapsed();
            if elapsed < self.spawn_min_interval {
                return Err(ProcessError::RateLimited(format!(
                    "sidecar spawn rate-limited: retry in {} ms",
                    (self.spawn_min_interval - elapsed).as_millis()
                )));
            }
        }
        group.last_spawn_attempt = Some(Instant::now());

        let mut first_error: Option<String> = None;
        for instance in group.instances.iter_mut() {
            if instance.child.is_some() || instance.terminating {
                continue;
            }
            match spawn_one(key, spec, instance.port) {
                Ok(child) => {
                    instance.child = Some(child);
                    instance.exit_code = None;
                }
                Err(e) if first_error.is_none() => first_error = Some(e),
                Err(_) => {}
            }
        }

        if let Some(e) = first_error {
            return Err(ProcessError::Spawn(e));
        }
        Ok(snapshot(group))
    }

    /// 現在の状態を返す(spawn も、生きているプロセスの停止・再構築もしない)。
    /// 未起動の key(= まだインスタンスが 1 つも無い)なら `spec` に沿った
    /// `exited` 相当の一覧を作って返すが、既にインスタンスがある場合は
    /// `spec.ports` と食い違っていてもそれらをそのまま報告する。構成の
    /// 再構築(≒ 既存プロセスの停止)は `ensure_started` だけの責務であり、
    /// 読み取り専用のはずの `status` が副作用で健全なプロセスを殺すことは
    /// あってはならない。
    pub fn status(&self, key: &str, spec: &ProcessSpec) -> Vec<InstanceStatus> {
        let mut groups = self.lock();
        let group = groups.entry(key.to_string()).or_insert_with(|| Group {
            instances: Vec::new(),
            last_spawn_attempt: None,
        });
        reap(group);
        seed_instances_if_empty(group, spec);
        snapshot(group)
    }

    /// 当該サイドカーの全インスタンスを停止する。既に止まっていても成功。
    ///
    /// SIGTERM 送信〜`shutdown_grace` 猶予〜SIGKILL の待ちはロックを解放
    /// してから行う(`take_for_stop` / `finish_stop` を参照)。そうしないと
    /// 全サイドカー共有の `groups` ロックを最大 `shutdown_grace` 秒保持し
    /// 続けてしまい、他のキーの `ensure_started`/`status` まで巻き添えで
    /// ブロックされ、「`ensure_started` は非ブロッキング」という前提が
    /// 崩れる。
    pub fn stop(&self, key: &str) {
        let taken = {
            let mut groups = self.lock();
            match groups.get_mut(key) {
                Some(group) => take_for_stop(group),
                None => Vec::new(),
            }
        };
        self.finish_stop(key, taken);
    }

    /// 全 key の全インスタンスを停止する(デーモン shutdown 用)。
    /// `stop` と同じ理由でロックを解放してから待つ。
    pub fn stop_all(&self) {
        let per_key: Vec<(String, Vec<TakenChild>)> = {
            let mut groups = self.lock();
            groups
                .iter_mut()
                .map(|(key, group)| (key.clone(), take_for_stop(group)))
                .collect()
        };
        for (key, taken) in per_key {
            self.finish_stop(&key, taken);
        }
    }

    /// `taken` (`take_for_stop` が切り離した子プロセス群)へ実際に
    /// SIGTERM/SIGKILL を送って待ち、結果を該当インスタンスへ書き戻す。
    /// ロックは子ごとに個別に取り直すので、他のキー(や他の呼び出し)を
    /// 長時間ブロックしない。
    fn finish_stop(&self, key: &str, taken: Vec<TakenChild>) {
        for mut taken in taken {
            let exit_code = kill_and_wait(&mut taken.child, self.shutdown_grace);
            let mut groups = self.lock();
            if let Some(group) = groups.get_mut(key) {
                // index/port に加えて `terminating` も確認する: この間に
                // ポート構成が変わって `align_instances` がインスタンス列を
                // 丸ごと作り直していたら、そこの `terminating` は初期値の
                // `false` に戻っているので一致せず、書き戻しをスキップする
                // (別物のインスタンスを誤って更新しない)。
                if let Some(instance) = group.instances.iter_mut().find(|i| {
                    i.index == taken.index && i.port == taken.port && i.terminating
                }) {
                    instance.exit_code = exit_code;
                    instance.terminating = false;
                }
            }
        }
    }
}

impl Drop for ProcessDriver {
    /// 明示的な `stop_all` を呼び忘れた経路(テストや panic 巻き戻し)でも
    /// 孤児を残さないための最後の砦。通常経路は `stop_all` を明示的に呼ぶこと。
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// `spec.ports` に合わせてインスタンス列を作り直す。ポート構成が変わった
/// (replicas 変更など)場合は、生きているインスタンスを停止してから作り直す。
/// `ensure_started` だけが呼ぶ(`status` は読み取り専用の
/// `seed_instances_if_empty` を使う)。
fn align_instances(group: &mut Group, spec: &ProcessSpec) {
    let same = group.instances.len() == spec.ports.len()
        && group
            .instances
            .iter()
            .zip(spec.ports.iter())
            .all(|(instance, port)| instance.port == *port);
    if same {
        return;
    }

    for instance in group.instances.iter_mut() {
        terminate(instance, Duration::from_millis(0));
    }
    group.instances = new_instances(spec);
}

/// 未起動の key(インスタンスが 1 つも無い)にだけ `spec.ports` 分の
/// `exited` 相当のインスタンスを作る。既にインスタンスがあるときは
/// `spec.ports` と食い違っていても一切触らない(= 生きているプロセスを
/// 停止しない)。`status` から呼ばれる読み取り専用版。
fn seed_instances_if_empty(group: &mut Group, spec: &ProcessSpec) {
    if group.instances.is_empty() {
        group.instances = new_instances(spec);
    }
}

fn new_instances(spec: &ProcessSpec) -> Vec<Instance> {
    spec.ports
        .iter()
        .enumerate()
        .map(|(index, port)| Instance {
            index: index as u32,
            port: *port,
            child: None,
            exit_code: None,
            terminating: false,
        })
        .collect()
}

/// `stop`/`stop_all` 用に、まだ停止処理を始めていない生存インスタンスの
/// 子プロセスをロック内で切り離す。切り離すと同時に `terminating = true`
/// を立てるので、ロックを離れた直後から `ensure_started` はこのインスタンス
/// を「未起動」として respawn しようとしなくなる。
struct TakenChild {
    index: u32,
    port: u16,
    child: Child,
}

fn take_for_stop(group: &mut Group) -> Vec<TakenChild> {
    let mut taken = Vec::new();
    for instance in group.instances.iter_mut() {
        if instance.terminating {
            continue;
        }
        if let Some(child) = instance.child.take() {
            instance.terminating = true;
            taken.push(TakenChild {
                index: instance.index,
                port: instance.port,
                child,
            });
        }
    }
    taken
}

/// 終了済みの子を回収し、`exit_code` を記録する。
fn reap(group: &mut Group) {
    for instance in group.instances.iter_mut() {
        let Some(child) = instance.child.as_mut() else {
            continue;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                instance.exit_code = status.code();
                instance.child = None;
            }
            Ok(None) => {}
            Err(_) => {
                instance.child = None;
            }
        }
    }
}

fn snapshot(group: &Group) -> Vec<InstanceStatus> {
    group
        .instances
        .iter()
        .map(|instance| InstanceStatus {
            index: instance.index,
            port: instance.port,
            running: instance.child.is_some(),
            exit_code: instance.exit_code,
        })
        .collect()
}

fn spawn_one(key: &str, spec: &ProcessSpec, port: u16) -> Result<Child, String> {
    use std::os::unix::process::CommandExt;

    let args: Vec<String> = spec
        .args
        .iter()
        .map(|arg| arg.replace("{port}", &port.to_string()))
        .collect();

    let mut command = Command::new(&spec.command);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 新しいプロセスグループに置く。停止時に killpg するので、サイドカーが
    // 更に子を作っていても道連れにできる(孤児が残らない)。
    command.process_group(0);

    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", spec.command.display()))?;

    if let Some(stdout) = child.stdout.take() {
        forward_output(key.to_string(), "stdout", stdout);
    }
    if let Some(stderr) = child.stderr.take() {
        forward_output(key.to_string(), "stderr", stderr);
    }

    Ok(child)
}

/// 子の出力を 1 行ずつホストのログへ流す。プラグインには渡さない。
fn forward_output<R: std::io::Read + Send + 'static>(key: String, stream: &'static str, reader: R) {
    std::thread::spawn(move || {
        use std::io::BufRead;
        let buffered = std::io::BufReader::new(reader);
        for line in buffered.lines() {
            let Ok(line) = line else { break };
            tracing::info!(sidecar = %key, stream, "{line}");
        }
    });
}

/// SIGTERM をプロセスグループへ送り、`grace` 待って死ななければ SIGKILL。
/// `align_instances` からの同期的な即時停止(`grace` は主に 0)用で、ロックを
/// 保持したまま呼ばれる。`stop`/`stop_all` はロックを保持したまま `grace`
/// 秒待つわけにいかないため、代わりに `take_for_stop` + `kill_and_wait` を
/// 使う(このドライバ全体で実際に kill 待ちをするロジックは
/// `kill_and_wait` に一本化してある)。
fn terminate(instance: &mut Instance, grace: Duration) {
    let Some(mut child) = instance.child.take() else {
        return;
    };
    instance.exit_code = kill_and_wait(&mut child, grace);
}

/// SIGTERM をプロセスグループへ送り、`grace` 待って死ななければ SIGKILL。
/// 生き残った終了コードは正常終了時のみ `Some`(SIGKILL に昇格した場合は
/// `wait` の結果を捨てて `None` を返す — 強制終了なので意味のある終了コード
/// ではないため)。呼び出し元はロックを保持していないことが前提
/// (`stop`/`stop_all` は `take_for_stop` で子を切り離してから呼ぶ)。
fn kill_and_wait(child: &mut Child, grace: Duration) -> Option<i32> {
    let pid = child.id() as i32;

    // SAFETY: `pid` は自分が spawn した(まだ wait していない)子のもので、
    // `process_group(0)` により pid == pgid。
    unsafe {
        libc::killpg(pid, libc::SIGTERM);
    }

    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code(),
            Ok(None) => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // SAFETY: 同上。grace 経過後の強制終了。
    unsafe {
        libc::killpg(pid, libc::SIGKILL);
    }
    let _ = child.wait();
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driver() -> ProcessDriver {
        ProcessDriver::new(Duration::from_millis(200), Duration::from_millis(0))
    }

    /// 起動しっぱなしになる無害なプロセス。ポートは使わないが、
    /// `{port}` 展開と引数の受け渡しを一緒に確認できる形にしておく。
    fn sleep_spec(ports: Vec<u16>) -> ProcessSpec {
        ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "echo port={port}; sleep 30".to_string()],
            ports,
        }
    }

    #[test]
    fn ensure_started_spawns_one_process_per_port_and_is_idempotent() {
        let driver = driver();
        let spec = sleep_spec(vec![50021, 50022]);

        let first = driver.ensure_started("p/tts", &spec).expect("first start");
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|i| i.running));
        assert_eq!(first[0].port, 50021);
        assert_eq!(first[1].port, 50022);
        assert_eq!(first[1].index, 1);

        let second = driver.ensure_started("p/tts", &spec).expect("second start");
        assert!(second.iter().all(|i| i.running));

        driver.stop("p/tts");
        assert!(driver.status("p/tts", &spec).iter().all(|i| !i.running));
    }

    #[test]
    fn respawn_within_min_interval_is_rate_limited() {
        let driver = ProcessDriver::new(Duration::from_millis(200), Duration::from_secs(1));
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "exit 3".to_string()],
            ports: vec![50100],
        };

        driver.ensure_started("p/quick", &spec).expect("first start");
        // 終了を待ってから再度 ensure_started する。
        std::thread::sleep(Duration::from_millis(200));

        let err = driver
            .ensure_started("p/quick", &spec)
            .expect_err("respawn within the min interval must be rejected");
        assert!(matches!(err, ProcessError::RateLimited(_)));
    }

    #[test]
    fn exited_process_reports_exit_code_and_not_running() {
        let driver = driver();
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "exit 7".to_string()],
            ports: vec![50101],
        };

        driver.ensure_started("p/exits", &spec).expect("start");
        std::thread::sleep(Duration::from_millis(200));

        let status = driver.status("p/exits", &spec);
        assert_eq!(status.len(), 1);
        assert!(!status[0].running);
        assert_eq!(status[0].exit_code, Some(7));
    }

    #[test]
    fn missing_executable_is_a_spawn_error() {
        let driver = driver();
        let spec = ProcessSpec {
            command: PathBuf::from("/nonexistent/edlr-test-binary"),
            args: vec![],
            ports: vec![50102],
        };

        let err = driver
            .ensure_started("p/missing", &spec)
            .expect_err("spawning a nonexistent binary must fail");
        assert!(matches!(err, ProcessError::Spawn(_)));
    }

    #[test]
    fn stop_kills_grandchildren_via_process_group() {
        let tmp = std::env::temp_dir().join(format!("edlr-pgid-test-{}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);

        let driver = driver();
        let spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".to_string(),
                format!("(sleep 60 & echo $! > {}); wait", tmp.display()),
            ],
            ports: vec![50103],
        };

        driver.ensure_started("p/tree", &spec).expect("start");

        // 孫が PID を書き出すまで待つ(最大 2 秒)。
        let mut grandchild_pid = None;
        for _ in 0..200 {
            if let Ok(content) = std::fs::read_to_string(&tmp) {
                if let Ok(pid) = content.trim().parse::<i32>() {
                    grandchild_pid = Some(pid);
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let grandchild_pid = grandchild_pid.expect("grandchild should report its pid");

        driver.stop("p/tree");
        std::thread::sleep(Duration::from_millis(300));

        // signal 0 は「シグナルを送らず存在確認だけする」。0 が返る = まだ生きている。
        // SAFETY: 存在確認のみで、シグナルは送らない。
        let alive = unsafe { libc::kill(grandchild_pid, 0) } == 0;
        let _ = std::fs::remove_file(&tmp);
        assert!(
            !alive,
            "grandchild {grandchild_pid} survived stop(); process-group kill is not working"
        );
    }

    #[test]
    fn stop_does_not_block_other_keys_while_waiting_out_the_grace_period() {
        // grace を長めにし、SIGTERM を無視する子を使うことで、`stop()` が
        // 猶予期間いっぱい SIGKILL までブロックされる状況を作る。
        let driver = ProcessDriver::new(Duration::from_secs(2), Duration::from_millis(0));
        let stubborn_spec = ProcessSpec {
            command: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), "trap '' TERM; sleep 30".to_string()],
            ports: vec![50110],
        };
        let other_spec = sleep_spec(vec![50111]);

        driver
            .ensure_started("p/stubborn", &stubborn_spec)
            .expect("start stubborn");
        driver
            .ensure_started("p/other", &other_spec)
            .expect("start other");

        let driver = std::sync::Arc::new(driver);
        let stopper = {
            let driver = std::sync::Arc::clone(&driver);
            std::thread::spawn(move || {
                driver.stop("p/stubborn");
            })
        };

        // stop("p/stubborn") が SIGKILL に昇格するまでロックを握っていたら、
        // 別キーの status() はここで長時間ブロックされるはず。
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        let status = driver.status("p/other", &other_spec);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "status() for an unrelated key must not block on another key's stop(); took {:?}",
            started.elapsed()
        );
        assert!(status.iter().all(|i| i.running));

        stopper.join().expect("stop thread should not panic");
        driver.stop("p/other");
    }

    #[test]
    fn status_does_not_kill_running_processes_on_port_mismatch() {
        let driver = driver();
        let original_spec = sleep_spec(vec![50120]);
        driver
            .ensure_started("p/live", &original_spec)
            .expect("start");

        // spec のポート構成を変えて status() を呼ぶ。読み取り専用のはずの
        // status() が align_instances 相当の再構築をしてしまうと、ここで
        // 生きているプロセスが殺される。
        let mismatched_spec = sleep_spec(vec![50121, 50122]);
        let status = driver.status("p/live", &mismatched_spec);

        assert!(
            status.iter().all(|i| i.running),
            "status() must not stop a healthy process just because spec.ports changed"
        );

        driver.stop("p/live");
    }
}
