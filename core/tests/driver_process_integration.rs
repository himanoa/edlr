//! `Registry` 経由でのサイドカー設定・承認・制御と、暗黙の HTTP 許可、
//! shutdown での確実な停止を、実 wasm を介さずに検証する。
//! (wasm 側の呼び出し経路は `core/src/plugin/host.rs` の単体テストが担当。)

use std::time::Duration;

use edlr_core::plugin::SidecarConfig;

mod support;

#[test]
fn granting_a_sidecar_adds_its_ports_to_the_http_allowlist() {
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

#[test]
fn revoking_a_grant_stops_running_instances() {
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
    env.registry.set_sidecar_grant("sc-plugin", "tts", true).unwrap();

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

#[test]
fn changing_the_config_stops_the_running_sidecar() {
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
    env.registry.set_sidecar_grant("sc-plugin", "tts", true).unwrap();
    env.registry
        .control_sidecar("sc-plugin", "tts", edlr_core::plugin::SidecarAction::Start)
        .unwrap();

    let updated = env
        .registry
        .set_sidecar_config(
            "sc-plugin",
            "tts",
            &SidecarConfig { port: 50325, ..config },
        )
        .expect("config change");
    assert!(
        !updated[0].instances[0].running,
        "changing the config must stop the running sidecar"
    );
}

#[test]
fn stop_all_sidecars_leaves_nothing_running() {
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
    env.registry.set_sidecar_grant("sc-plugin", "tts", true).unwrap();
    env.registry
        .control_sidecar("sc-plugin", "tts", edlr_core::plugin::SidecarAction::Start)
        .unwrap();

    env.registry.stop_all_sidecars();
    std::thread::sleep(Duration::from_millis(200));

    let sidecars = env.registry.sidecars("sc-plugin").unwrap();
    assert!(sidecars[0].instances.iter().all(|i| !i.running));
}
