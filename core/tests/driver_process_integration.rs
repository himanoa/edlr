//! `Registry` 経由でのサイドカー設定・承認・制御と、暗黙の HTTP 許可、
//! shutdown での確実な停止を、実 wasm を介さずに検証する。
//! (wasm 側の呼び出し経路は `core/src/plugin/host.rs` の単体テストが担当。)

use std::time::{Duration, Instant};

use edlr_core::plugin::SidecarConfig;

mod support;

#[tokio::test]
async fn granting_a_sidecar_adds_its_ports_to_the_http_allowlist() {
    let env = support::sidecar_env("tts", 50301, true);

    // 未承認では暗黙許可も無い。
    assert!(!support::effective_hosts(&env.registry, "sc-plugin")
        .iter()
        .any(|h| h.contains("50301")));

    env.registry
        .set_sidecar_config(
            "sc-plugin",
            "tts",
            &SidecarConfig {
                command: "/bin/sh".into(),
                // `replicas: 2` かつ `scalable: true` の組み合わせは
                // `SidecarConfigStore::update_and_effective`(Task 3、
                // `core/src/plugin/sidecar.rs::validate`)が `args` に
                // `{port}` を含むことを要求する(各レプリカが同じ固定引数で
                // 起動され、実際には同じポートを取り合うだけの無意味な構成に
                // ならないようにするため)。ブリーフのサンプルはこの既存の
                // 検証済み契約と整合しなかったため、意図(50301 と 50302 の
                // 両方が実効許可ホストに載ることの確認)を変えずに `{port}`
                // を足している。
                args: vec!["-c".into(), "sleep 30 # {port}".into()],
                port: 50301,
                replicas: 2,
            },
        )
        .expect("config should save");
    env.registry
        .set_sidecar_grant("sc-plugin", "tts", true)
        .expect("grant should save");

    let hosts = support::effective_hosts(&env.registry, "sc-plugin");
    assert!(hosts.contains(&"http://127.0.0.1:50301".to_string()));
    assert!(hosts.contains(&"http://127.0.0.1:50302".to_string()));

    // 取消で暗黙許可も消える。
    env.registry
        .set_sidecar_grant("sc-plugin", "tts", false)
        .expect("revoke should save");
    assert!(!support::effective_hosts(&env.registry, "sc-plugin")
        .iter()
        .any(|h| h.contains("50301")));
}

#[tokio::test]
async fn revoking_a_grant_stops_running_instances() {
    let env = support::sidecar_env("tts", 50311, false);
    env.registry
        .set_sidecar_config(
            "sc-plugin",
            "tts",
            &SidecarConfig {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                port: 50311,
                replicas: 1,
            },
        )
        .unwrap();
    env.registry
        .set_sidecar_grant("sc-plugin", "tts", true)
        .unwrap();

    let started = env
        .registry
        .control_sidecar("sc-plugin", "tts", edlr_core::plugin::SidecarAction::Start)
        .expect("start");
    assert!(started[0].instances[0].running);

    let after = env
        .registry
        .set_sidecar_grant("sc-plugin", "tts", false)
        .expect("revoke");
    assert!(
        !after[0].instances[0].running,
        "revoking a grant must stop the running sidecar"
    );
}

#[tokio::test]
async fn changing_the_config_stops_the_running_sidecar() {
    let env = support::sidecar_env("tts", 50321, true);
    let config = SidecarConfig {
        command: "/bin/sh".into(),
        args: vec!["-c".into(), "sleep 30".into()],
        port: 50321,
        replicas: 1,
    };
    env.registry
        .set_sidecar_config("sc-plugin", "tts", &config)
        .unwrap();
    env.registry
        .set_sidecar_grant("sc-plugin", "tts", true)
        .unwrap();
    env.registry
        .control_sidecar("sc-plugin", "tts", edlr_core::plugin::SidecarAction::Start)
        .unwrap();

    let updated = env
        .registry
        .set_sidecar_config(
            "sc-plugin",
            "tts",
            &SidecarConfig {
                port: 50325,
                ..config
            },
        )
        .expect("config change");
    assert!(
        !updated[0].instances[0].running,
        "changing the config must stop the running sidecar"
    );
}

#[tokio::test]
async fn stop_all_sidecars_leaves_nothing_running() {
    let env = support::sidecar_env("tts", 50331, false);
    env.registry
        .set_sidecar_config(
            "sc-plugin",
            "tts",
            &SidecarConfig {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                port: 50331,
                replicas: 1,
            },
        )
        .unwrap();
    env.registry
        .set_sidecar_grant("sc-plugin", "tts", true)
        .unwrap();
    env.registry
        .control_sidecar("sc-plugin", "tts", edlr_core::plugin::SidecarAction::Start)
        .unwrap();

    env.registry.stop_all_sidecars();
    std::thread::sleep(Duration::from_millis(200));

    let sidecars = env.registry.sidecars("sc-plugin").unwrap();
    assert!(sidecars[0].instances.iter().all(|i| !i.running));
}

/// リグレッションテスト: `Registry::refresh_sidecar_runtime` は一時
/// `sidecar_runtime_lock` を全プラグイン共有のグローバルロックにしていたが、
/// このロックの臨界区間は同期版 `ProcessDriver::stop`(SIGTERM を無視する
/// 子がいれば `shutdown_grace` 秒ブロックしうる)を含むため、あるプラグイン
/// のサイドカー停止待ちの間、無関係な別プラグインの `set_sidecar_grant` /
/// `set_sidecar_config` まで足止めしてしまっていた。ロックをプラグイン ID
/// ごとに分けたことで、この足止めが起きないことを確認する。
///
/// タイミング依存のテストなので、実際の SIGTERM 無視プロセスの grace(3 秒、
/// `SIDECAR_SHUTDOWN_GRACE`)に対して十分余裕を持たせた閾値(500ms)を使う。
#[tokio::test]
async fn sidecar_operations_on_different_plugins_do_not_block_each_other() {
    let env = support::two_plugin_sidecar_env("tts", 50341, 50342);

    // plugin A: SIGTERM を無視する子を起動しておく。取消で `stop` が走ると
    // `shutdown_grace`(既定 3 秒)まるまるブロックする(SIGKILL への昇格待ち)。
    env.registry
        .set_sidecar_config(
            "sc-plugin-a",
            "tts",
            &SidecarConfig {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), "trap '' TERM; sleep 30".into()],
                port: 50341,
                replicas: 1,
            },
        )
        .unwrap();
    env.registry
        .set_sidecar_grant("sc-plugin-a", "tts", true)
        .unwrap();
    env.registry
        .control_sidecar(
            "sc-plugin-a",
            "tts",
            edlr_core::plugin::SidecarAction::Start,
        )
        .unwrap();

    // plugin B: 承認/設定操作の対象になれるよう設定だけ済ませておく。
    env.registry
        .set_sidecar_config(
            "sc-plugin-b",
            "tts",
            &SidecarConfig {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                port: 50342,
                replicas: 1,
            },
        )
        .unwrap();

    // plugin A の承認を別スレッドで取り消す。これは stop() を経由するため、
    // SIGTERM を無視する子が SIGKILL に昇格するまで(最大 grace 秒)ブロックする。
    let registry_for_revoke = env.registry.clone();
    let revoker = std::thread::spawn(move || {
        registry_for_revoke
            .set_sidecar_grant("sc-plugin-a", "tts", false)
            .unwrap();
    });

    // revoker が実際に stop() のブロッキング区間へ入るのを待つ猶予。
    std::thread::sleep(Duration::from_millis(150));

    // plugin A の取消がまだ(grace 中で)進行中のはずのこのタイミングで、
    // plugin B の操作が短時間で返ることを確認する: プラグイン単位ロックで
    // あれば plugin A の id 別ロックとは無関係に即座に進むはず。
    let started = Instant::now();
    env.registry
        .set_sidecar_grant("sc-plugin-b", "tts", true)
        .expect("plugin B's grant must succeed independently of plugin A's in-flight stop");
    let elapsed = started.elapsed();

    revoker.join().expect("revoker thread should not panic");

    assert!(
        elapsed < Duration::from_millis(500),
        "plugin B's set_sidecar_grant must not be blocked by plugin A's in-flight \
         stop (a global sidecar_runtime_lock would serialize them); took {elapsed:?}"
    );
}
