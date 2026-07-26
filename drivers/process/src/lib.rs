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
struct Instance {
    index: u32,
    port: u16,
    child: Option<Child>,
    exit_code: Option<i32>,
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

        let needs_spawn = group.instances.iter().any(|i| i.child.is_none());
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
            if instance.child.is_some() {
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

    /// 現在の状態を返す(spawn はしない)。未起動の key でも `spec` に沿った
    /// `exited` 相当の一覧を返す。
    pub fn status(&self, key: &str, spec: &ProcessSpec) -> Vec<InstanceStatus> {
        let mut groups = self.lock();
        let group = groups.entry(key.to_string()).or_insert_with(|| Group {
            instances: Vec::new(),
            last_spawn_attempt: None,
        });
        reap(group);
        align_instances(group, spec);
        snapshot(group)
    }

    /// 当該サイドカーの全インスタンスを停止する。既に止まっていても成功。
    pub fn stop(&self, key: &str) {
        let mut groups = self.lock();
        if let Some(group) = groups.get_mut(key) {
            for instance in group.instances.iter_mut() {
                terminate(instance, self.shutdown_grace);
            }
        }
    }

    /// 全 key の全インスタンスを停止する(デーモン shutdown 用)。
    pub fn stop_all(&self) {
        let mut groups = self.lock();
        for group in groups.values_mut() {
            for instance in group.instances.iter_mut() {
                terminate(instance, self.shutdown_grace);
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
    group.instances = spec
        .ports
        .iter()
        .enumerate()
        .map(|(index, port)| Instance {
            index: index as u32,
            port: *port,
            child: None,
            exit_code: None,
        })
        .collect();
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
fn terminate(instance: &mut Instance, grace: Duration) {
    let Some(child) = instance.child.as_mut() else {
        return;
    };
    let pid = child.id() as i32;

    // SAFETY: `pid` は自分が spawn した(まだ wait していない)子のもので、
    // `process_group(0)` により pid == pgid。負値にしてグループへ送る。
    unsafe {
        libc::killpg(pid, libc::SIGTERM);
    }

    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                instance.exit_code = status.code();
                instance.child = None;
                return;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    unsafe {
        libc::killpg(pid, libc::SIGKILL);
    }
    let _ = child.wait();
    instance.exit_code = None;
    instance.child = None;
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
}
