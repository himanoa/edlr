use super::*;
use crate::capability::request::{FilesystemMode, WidgetSize};
use crate::event::Event;
use std::fs;

fn write_manifest(dir: &Path, contents: &str) {
    fs::write(dir.join("manifest.toml"), contents).unwrap();
}

fn write_entry(dir: &Path, name: &str) {
    fs::write(dir.join(name), b"\0asm").unwrap();
}

#[test]
fn rejects_the_reserved_plugin_id_host() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("host");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "host"
name = "Host Impersonator"
version = "0.1.0"
entry = "plugin.wasm"
"#,
    );
    let err = load_manifest(&plugin_dir)
        .expect_err("the id \"host\" is reserved for host-synthesized messages");
    assert!(matches!(err, ManifestError::ReservedId));
}

#[test]
fn parses_full_manifest_with_all_setting_types() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("sample-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "sample-plugin"
name = "Sample Plugin"
version = "0.1.0"
description = "A sample plugin"
entry = "plugin.wasm"
events = ["FSDJump", "*"]

[[settings]]
key = "enabled"
label = "Enabled"
type = "boolean"
default = true

[[settings]]
key = "greeting"
label = "Greeting"
type = "string"
default = "hello"

[[settings]]
key = "count"
label = "Count"
type = "number"
default = 3.0

[[settings]]
key = "mode"
label = "Mode"
type = "select"
default = "a"
options = ["a", "b"]
"#,
    );

    let manifest = load_manifest(&plugin_dir).expect("manifest should parse");

    assert_eq!(manifest.id, "sample-plugin");
    assert_eq!(manifest.name, "Sample Plugin");
    assert_eq!(manifest.version, "0.1.0");
    assert_eq!(manifest.description, "A sample plugin");
    assert_eq!(manifest.entry, "plugin.wasm");
    assert_eq!(
        manifest.events,
        vec!["FSDJump".to_string(), "*".to_string()]
    );
    assert_eq!(manifest.settings.len(), 4);

    assert_eq!(
        manifest.settings[0],
        SettingField::Boolean {
            key: "enabled".into(),
            label: "Enabled".into(),
            default: true,
        }
    );
    assert_eq!(
        manifest.settings[1],
        SettingField::String {
            key: "greeting".into(),
            label: "Greeting".into(),
            default: "hello".into(),
        }
    );
    assert_eq!(
        manifest.settings[2],
        SettingField::Number {
            key: "count".into(),
            label: "Count".into(),
            default: 3.0,
        }
    );
    assert_eq!(
        manifest.settings[3],
        SettingField::Select {
            key: "mode".into(),
            label: "Mode".into(),
            default: "a".into(),
            options: Some(vec!["a".into(), "b".into()]),
            options_from: None,
        }
    );
}

#[test]
fn number_setting_accepts_bare_toml_integer_default() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("int-default-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "int-default-plugin"
name = "Int Default"
version = "0.1.0"
entry = "plugin.wasm"

[[settings]]
key = "volume"
label = "Volume"
type = "number"
default = 80
"#,
    );

    let manifest = load_manifest(&plugin_dir).expect("manifest with integer default should parse");

    assert_eq!(manifest.settings.len(), 1);
    assert_eq!(
        manifest.settings[0],
        SettingField::Number {
            key: "volume".into(),
            label: "Volume".into(),
            default: 80.0,
        }
    );
    assert_eq!(
        manifest.settings[0].default_value(),
        serde_json::json!(80.0)
    );
}

#[test]
fn id_mismatch_with_directory_name_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("myplugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "other-plugin"
name = "Other"
version = "0.1.0"
entry = "plugin.wasm"
"#,
    );

    let err = load_manifest(&plugin_dir).expect_err("id mismatch should be rejected");
    assert!(matches!(err, ManifestError::IdMismatch));
}

#[test]
fn bad_id_format_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("Bad_ID");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "Bad_ID"
name = "Bad"
version = "0.1.0"
entry = "plugin.wasm"
"#,
    );

    let err = load_manifest(&plugin_dir).expect_err("bad id format should be rejected");
    assert!(matches!(err, ManifestError::BadId));
}

#[test]
fn missing_entry_file_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("no-entry-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_manifest(
        &plugin_dir,
        r#"
id = "no-entry-plugin"
name = "No Entry"
version = "0.1.0"
entry = "plugin.wasm"
"#,
    );
    // 意図的に entry ファイルは作らない

    let err = load_manifest(&plugin_dir).expect_err("missing entry should be rejected");
    assert!(matches!(err, ManifestError::MissingEntry));
}

#[test]
fn catch_up_is_parsed_for_cron_and_defaults_to_false() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("catch-up-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "catch-up-plugin"
name = "Catch Up"
version = "0.1.0"
entry = "plugin.wasm"

[[schedule]]
name = "daily"
cron = "0 9 * * *"
catch-up = true

[[schedule]]
name = "hourly"
cron = "0 * * * *"
"#,
    );

    let manifest = load_manifest(&plugin_dir).expect("catch-up should parse");
    assert!(manifest.schedules[0].catch_up);
    assert!(
        !manifest.schedules[1].catch_up,
        "catch-up must default to false"
    );
}

/// interval には追い掛けるべき「定刻」が無い。黙って無視すると書いた人が
/// 効いていると思い込むので、マニフェストごと拒否する。
#[test]
fn catch_up_with_interval_seconds_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("bad-catch-up");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "bad-catch-up"
name = "Bad Catch Up"
version = "0.1.0"
entry = "plugin.wasm"

[[schedule]]
name = "flush"
interval-seconds = 60
catch-up = true
"#,
    );

    let err =
        load_manifest(&plugin_dir).expect_err("catch-up with interval-seconds should be rejected");
    assert!(
        err.to_string().contains("catch-up"),
        "the error should name catch-up, got: {err}"
    );
}

/// `secret` は `default` を取らない(マニフェストに秘密情報を書ける
/// 余地を作らないため)。値は常に空文字列から始まる。
#[test]
fn secret_setting_is_parsed_and_defaults_to_an_empty_string() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("secret-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "secret-plugin"
name = "Secret"
version = "0.1.0"
entry = "plugin.wasm"

[[settings]]
key = "api-key"
label = "API Key"
type = "secret"
"#,
    );

    let manifest = load_manifest(&plugin_dir).expect("secret settings should parse");
    assert_eq!(manifest.settings.len(), 1);
    let field = &manifest.settings[0];
    assert_eq!(field.key(), "api-key");
    assert!(field.is_secret());
    assert_eq!(field.default_value(), serde_json::json!(""));
}

/// `map` は `default` を取らない(常に空オブジェクトから始まる)。
#[test]
fn map_setting_is_parsed_and_defaults_to_an_empty_object() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("map-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "map-plugin"
name = "Map"
version = "0.1.0"
entry = "plugin.wasm"

[[settings]]
key = "aliases"
label = "表示名の置き換え"
type = "map"
"#,
    );

    let manifest = load_manifest(&plugin_dir).expect("map settings should parse");
    assert_eq!(manifest.settings.len(), 1);
    assert_eq!(
        manifest.settings[0],
        SettingField::Map {
            key: "aliases".into(),
            label: "表示名の置き換え".into(),
        }
    );
    assert_eq!(manifest.settings[0].key(), "aliases");
    assert!(!manifest.settings[0].is_secret());
    assert_eq!(manifest.settings[0].default_value(), serde_json::json!({}));
}

/// `map` に `default` を書いたらマニフェストごと拒否する
/// (`deny_unknown_fields` の既存方針。空から始まる型なので初期値は無い)。
#[test]
fn map_setting_with_a_default_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("map-default-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "map-default-plugin"
name = "Map Default"
version = "0.1.0"
entry = "plugin.wasm"

[[settings]]
key = "aliases"
label = "Aliases"
type = "map"

[settings.default]
a = "b"
"#,
    );

    let err = load_manifest(&plugin_dir).expect_err("a default on map should be rejected");
    assert!(matches!(err, ManifestError::Parse(_)), "got: {err}");
}

/// 他の型は `secret` 扱いされない(`is_secret` の取り違えを防ぐ)。
#[test]
fn non_secret_settings_are_not_marked_secret() {
    for field in [
        SettingField::Boolean {
            key: "b".into(),
            label: "B".into(),
            default: true,
        },
        SettingField::String {
            key: "s".into(),
            label: "S".into(),
            default: "x".into(),
        },
        SettingField::Number {
            key: "n".into(),
            label: "N".into(),
            default: 1.0,
        },
        SettingField::Select {
            key: "sel".into(),
            label: "Sel".into(),
            default: "a".into(),
            options: Some(vec!["a".into()]),
            options_from: None,
        },
        SettingField::Map {
            key: "m".into(),
            label: "M".into(),
        },
    ] {
        assert!(
            !field.is_secret(),
            "{field:?} must not be treated as secret"
        );
    }
}

#[test]
fn duplicate_settings_key_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("dup-key-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "dup-key-plugin"
name = "Dup Key"
version = "0.1.0"
entry = "plugin.wasm"

[[settings]]
key = "foo"
label = "Foo"
type = "boolean"
default = true

[[settings]]
key = "foo"
label = "Foo Again"
type = "string"
default = "x"
"#,
    );

    let err = load_manifest(&plugin_dir).expect_err("duplicate key should be rejected");
    assert!(matches!(err, ManifestError::DuplicateKey));
}

#[test]
fn toml_syntax_error_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("broken-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(&plugin_dir, "this is not valid = = toml [[[");

    let err = load_manifest(&plugin_dir).expect_err("toml syntax error should be rejected");
    assert!(matches!(err, ManifestError::Parse(_)));
}

fn journal_event(name: &str) -> Event {
    Event::Journal {
        timestamp: "2026-07-25T00:00:00Z".into(),
        event: name.into(),
        raw: serde_json::json!({}),
        replay: false,
    }
}

fn status_event() -> Event {
    Event::Status {
        raw: serde_json::json!({}),
    }
}

#[test]
fn wildcard_matches_all_journal_events_but_not_status() {
    let events = vec!["*".to_string()];
    assert!(matches_event(&events, &journal_event("FSDJump")));
    assert!(matches_event(&events, &journal_event("Docked")));
    assert!(!matches_event(&events, &status_event()));
}

#[test]
fn status_keyword_matches_only_status_events() {
    let events = vec!["status".to_string()];
    assert!(!matches_event(&events, &journal_event("FSDJump")));
    assert!(matches_event(&events, &status_event()));
}

#[test]
fn exact_event_name_matches_only_that_journal_event() {
    let events = vec!["FSDJump".to_string()];
    assert!(matches_event(&events, &journal_event("FSDJump")));
    assert!(!matches_event(&events, &journal_event("Docked")));
    assert!(!matches_event(&events, &status_event()));
}

#[test]
fn empty_event_list_matches_nothing() {
    let events: Vec<String> = vec![];
    assert!(!matches_event(&events, &journal_event("FSDJump")));
    assert!(!matches_event(&events, &status_event()));
}

#[test]
fn schedule_spec_display_string_matches_expected_format() {
    assert_eq!(
        ScheduleSpec::IntervalSeconds(60).display_string(),
        "every 60s"
    );
    assert_eq!(
        ScheduleSpec::Cron("0 9 * * *".to_string()).display_string(),
        "cron: 0 9 * * *"
    );
}

#[test]
fn capabilities_with_http_request_are_parsed() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("cap-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "cap-plugin"
name = "Cap Plugin"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["https://api.example.com", "https://api2.example.com"]
reason = "fetch fleet data"
"#,
    );

    let manifest = load_manifest(&plugin_dir).expect("manifest should parse");

    assert_eq!(manifest.capabilities.len(), 1);
    assert_eq!(
        manifest.capabilities[0],
        CapabilityRequest::Http {
            hosts: vec![
                "https://api.example.com".to_string(),
                "https://api2.example.com".to_string(),
            ],
            reason: "fetch fleet data".to_string(),
        }
    );
}

#[test]
fn capabilities_default_to_empty_when_omitted() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("no-cap-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "no-cap-plugin"
name = "No Cap"
version = "0.1.0"
entry = "plugin.wasm"
"#,
    );

    let manifest = load_manifest(&plugin_dir).expect("manifest should parse");
    assert!(manifest.capabilities.is_empty());
}

#[test]
fn unknown_capability_kind_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("unknown-kind-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "unknown-kind-plugin"
name = "Unknown Kind"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "filesystem"
hosts = ["https://api.example.com"]
reason = "n/a"
"#,
    );

    let err = load_manifest(&plugin_dir).expect_err("unknown capability kind should error");
    assert!(matches!(err, ManifestError::Parse(_)));
}

#[test]
fn host_without_scheme_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("no-scheme-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "no-scheme-plugin"
name = "No Scheme"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["api.example.com"]
reason = "fetch data"
"#,
    );

    let err = load_manifest(&plugin_dir).expect_err("host without scheme should be rejected");
    assert!(matches!(err, ManifestError::BadCapability(_)));
}

#[test]
fn host_with_path_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("path-host-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "path-host-plugin"
name = "Path Host"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["https://api.example.com/v1"]
reason = "fetch data"
"#,
    );

    let err = load_manifest(&plugin_dir).expect_err("host with path should be rejected");
    assert!(matches!(err, ManifestError::BadCapability(_)));
}

#[test]
fn empty_hosts_list_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("empty-hosts-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "empty-hosts-plugin"
name = "Empty Hosts"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = []
reason = "fetch data"
"#,
    );

    let err = load_manifest(&plugin_dir).expect_err("empty hosts should be rejected");
    assert!(matches!(err, ManifestError::BadCapability(_)));
}

#[test]
fn empty_reason_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("empty-reason-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "empty-reason-plugin"
name = "Empty Reason"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["https://api.example.com"]
reason = ""
"#,
    );

    let err = load_manifest(&plugin_dir).expect_err("empty reason should be rejected");
    assert!(matches!(err, ManifestError::BadCapability(_)));
}

#[test]
fn fingerprint_is_stable_order_independent_and_sensitive_to_content() {
    fn manifest_with_hosts(hosts: Vec<&str>) -> Manifest {
        Manifest {
            id: "fp-plugin".into(),
            name: "FP Plugin".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![CapabilityRequest::Http {
                hosts: hosts.into_iter().map(String::from).collect(),
                reason: "fetch data".into(),
            }],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    let a = manifest_with_hosts(vec!["https://api.example.com", "https://api2.example.com"]);
    let b = manifest_with_hosts(vec!["https://api.example.com", "https://api2.example.com"]);
    let reordered =
        manifest_with_hosts(vec!["https://api2.example.com", "https://api.example.com"]);
    let extra_host = manifest_with_hosts(vec![
        "https://api.example.com",
        "https://api2.example.com",
        "https://api3.example.com",
    ]);
    let mut no_capabilities = a.clone();
    no_capabilities.capabilities.clear();

    let fp_a = a.capabilities_fingerprint().expect("should have a value");
    let fp_b = b.capabilities_fingerprint().expect("should have a value");
    let fp_reordered = reordered
        .capabilities_fingerprint()
        .expect("should have a value");
    let fp_extra = extra_host
        .capabilities_fingerprint()
        .expect("should have a value");

    assert_eq!(
        fp_a, fp_b,
        "identical content must produce identical fingerprint"
    );
    assert_eq!(fp_a, fp_reordered, "host order must not affect fingerprint");
    assert_ne!(
        fp_a, fp_extra,
        "changing the request set must change the fingerprint"
    );
    assert_eq!(
        no_capabilities.capabilities_fingerprint(),
        None,
        "no capability requests must yield None"
    );
}

#[test]
fn fingerprint_does_not_collide_when_reason_contains_delimiter_like_content() {
    // Set A: a single request whose `reason` contains text that looks like a
    // second serialized request (using the delimiters the old naive
    // implementation joined fields with: `;` between requests, `|` between
    // fields within a request).
    let set_a = Manifest {
        id: "fp-plugin".into(),
        name: "FP Plugin".into(),
        version: "0.1.0".into(),
        description: String::new(),
        entry: "plugin.wasm".into(),
        events: vec![],
        settings: vec![],
        capabilities: vec![CapabilityRequest::Http {
            hosts: vec!["https://h1.com".into()],
            reason: "foo;http|hosts=https://h2.com|reason=bar".into(),
        }],
        sidecars: vec![],
        filesystem: vec![],
        bus: vec![],
        dashboard: vec![],
        schedules: vec![],
    };

    // Set B: two separate requests that request an additional host
    // (`h2.com`) beyond what set A actually grants access to.
    let set_b = Manifest {
        id: "fp-plugin".into(),
        name: "FP Plugin".into(),
        version: "0.1.0".into(),
        description: String::new(),
        entry: "plugin.wasm".into(),
        events: vec![],
        settings: vec![],
        capabilities: vec![
            CapabilityRequest::Http {
                hosts: vec!["https://h1.com".into()],
                reason: "foo".into(),
            },
            CapabilityRequest::Http {
                hosts: vec!["https://h2.com".into()],
                reason: "bar".into(),
            },
        ],
        sidecars: vec![],
        filesystem: vec![],
        bus: vec![],
        dashboard: vec![],
        schedules: vec![],
    };

    let fp_a = set_a
        .capabilities_fingerprint()
        .expect("should have a value");
    let fp_b = set_b
        .capabilities_fingerprint()
        .expect("should have a value");

    assert_ne!(
        fp_a, fp_b,
        "a request set that grants an extra host must not share a fingerprint \
         with a single request whose free-text reason merely looks like it"
    );
}

#[test]
fn host_with_userinfo_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("userinfo-host-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "userinfo-host-plugin"
name = "Userinfo Host"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["https://user:pw@api.example.com"]
reason = "fetch data"
"#,
    );

    let err = load_manifest(&plugin_dir).expect_err("host with userinfo should be rejected");
    assert!(matches!(err, ManifestError::BadCapability(_)));
}

#[test]
fn fingerprint_differs_when_host_added_even_with_previously_colliding_reason() {
    // Adversarial pair for the retired FNV-1a-64 fingerprint: the plugin
    // author controls both manifest versions, so under a 64-bit
    // non-cryptographic hash they could pick a `reason` for v2 that
    // collides with v1's fingerprint despite v2 adding a host (e.g.
    // `evil.com`) that was never approved. `reason` is unconstrained
    // free text (beyond trim + invisible-char rejection), so nothing
    // stops an attacker from searching for such a pair against the old
    // hash; SHA-256 makes that search computationally infeasible. This
    // test doesn't reproduce a real FNV collision (that would require
    // an actual birthday search) -- it documents the shape of the
    // attack and asserts the current implementation does not share a
    // fingerprint across a request-set change, which is the property
    // that must hold regardless of what `reason` text is chosen.
    fn manifest_with(hosts: Vec<&str>, reason: &str) -> Manifest {
        Manifest {
            id: "fp-plugin".into(),
            name: "FP Plugin".into(),
            version: "0.1.0".into(),
            description: String::new(),
            entry: "plugin.wasm".into(),
            events: vec![],
            settings: vec![],
            capabilities: vec![CapabilityRequest::Http {
                hosts: hosts.into_iter().map(String::from).collect(),
                reason: reason.to_string(),
            }],
            sidecars: vec![],
            filesystem: vec![],
            bus: vec![],
            dashboard: vec![],
            schedules: vec![],
        }
    }

    let v1 = manifest_with(
        vec!["https://approved.example.com"],
        "please let me sync data",
    );
    let v2 = manifest_with(
        vec!["https://approved.example.com", "https://evil.example.com"],
        "please let me sync data",
    );

    assert_ne!(
        v1.capabilities_fingerprint(),
        v2.capabilities_fingerprint(),
        "adding a host must always change the fingerprint, regardless of reason text"
    );
}

#[test]
fn reason_with_zero_width_character_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("zero-width-reason-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        "id = \"zero-width-reason-plugin\"\nname = \"ZW\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n\n[[capabilities]]\nkind = \"http\"\nhosts = [\"https://api.example.com\"]\nreason = \"fetch\u{200B}data\"\n",
    );

    let err =
        load_manifest(&plugin_dir).expect_err("zero-width character in reason must be rejected");
    assert!(matches!(err, ManifestError::BadCapability(_)));
}

#[test]
fn reason_with_control_character_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("control-char-reason-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        "id = \"control-char-reason-plugin\"\nname = \"CC\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n\n[[capabilities]]\nkind = \"http\"\nhosts = [\"https://api.example.com\"]\nreason = \"fetch\\ndata\"\n",
    );

    let err = load_manifest(&plugin_dir).expect_err("control character in reason must be rejected");
    assert!(matches!(err, ManifestError::BadCapability(_)));
}

#[test]
fn reason_is_trimmed_before_fingerprinting() {
    let tmp = tempfile::tempdir().unwrap();

    let padded_dir = tmp.path().join("padded-reason-plugin");
    fs::create_dir_all(&padded_dir).unwrap();
    write_entry(&padded_dir, "plugin.wasm");
    write_manifest(
        &padded_dir,
        r#"
id = "padded-reason-plugin"
name = "Padded"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["https://api.example.com"]
reason = "  foo  "
"#,
    );

    let bare_dir = tmp.path().join("bare-reason-plugin");
    fs::create_dir_all(&bare_dir).unwrap();
    write_entry(&bare_dir, "plugin.wasm");
    write_manifest(
        &bare_dir,
        r#"
id = "bare-reason-plugin"
name = "Bare"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["https://api.example.com"]
reason = "foo"
"#,
    );

    let padded = load_manifest(&padded_dir).expect("padded reason should parse");
    let bare = load_manifest(&bare_dir).expect("bare reason should parse");

    assert_eq!(
        padded.capabilities[0],
        CapabilityRequest::Http {
            hosts: vec!["https://api.example.com".to_string()],
            reason: "foo".to_string(),
        },
        "reason must be trimmed before being stored"
    );
    assert_eq!(
        padded.capabilities_fingerprint(),
        bare.capabilities_fingerprint(),
        "trimmed and already-bare reasons must fingerprint identically"
    );
}

#[test]
fn old_fnv_format_fingerprint_does_not_validate_against_new_sha256_fingerprint() {
    // Simulates an on-disk grant saved by the retired FNV-1a-64
    // implementation (a 16 hex-char fingerprint) being checked against
    // the current SHA-256 (64 hex-char) implementation. This must not
    // silently validate -- it must simply mismatch and be treated as
    // stale (fail closed), never panic.
    let manifest = Manifest {
        id: "legacy-fp-plugin".into(),
        name: "Legacy".into(),
        version: "0.1.0".into(),
        description: String::new(),
        entry: "plugin.wasm".into(),
        events: vec![],
        settings: vec![],
        capabilities: vec![CapabilityRequest::Http {
            hosts: vec!["https://api.example.com".to_string()],
            reason: "fetch data".to_string(),
        }],
        sidecars: vec![],
        filesystem: vec![],
        bus: vec![],
        dashboard: vec![],
        schedules: vec![],
    };

    let old_style_fingerprint = "0123456789abcdef"; // 16 hex chars, FNV-1a-64 shape
    let current = manifest
        .capabilities_fingerprint()
        .expect("should have a value");

    assert_ne!(
        old_style_fingerprint, current,
        "an old-format fingerprint must not coincide with the new format"
    );
    assert_eq!(current.len(), 64, "SHA-256 hex digest is 64 characters");
}

#[test]
fn host_with_bare_trailing_slash_is_accepted() {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("trailing-slash-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        r#"
id = "trailing-slash-plugin"
name = "Trailing Slash"
version = "0.1.0"
entry = "plugin.wasm"

[[capabilities]]
kind = "http"
hosts = ["https://example.com/"]
reason = "fetch data"
"#,
    );

    let manifest = load_manifest(&plugin_dir).expect("bare trailing slash host should be accepted");
    assert_eq!(
        manifest.capabilities[0],
        CapabilityRequest::Http {
            hosts: vec!["https://example.com/".to_string()],
            reason: "fetch data".to_string(),
        }
    );
}

fn parse_sidecar_manifest(body: &str) -> Result<Manifest, ManifestError> {
    let dir = tempfile::tempdir().unwrap();
    let plugin_dir = dir.path().join("sc-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            "id = \"sc-plugin\"\nname = \"SC\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n{body}"
        ),
    )
    .unwrap();
    load_manifest(&plugin_dir)
}

#[test]
fn sidecar_block_is_parsed() {
    let manifest = parse_sidecar_manifest(
        r#"
[[sidecar]]
name = "tts"
reason = "音声合成エンジンをローカルで動かすため"
args = ["--port", "{port}"]
port = 50021
scalable = true
"#,
    )
    .expect("valid sidecar manifest should load");

    assert_eq!(manifest.sidecars.len(), 1);
    let sidecar = &manifest.sidecars[0];
    assert_eq!(sidecar.name, "tts");
    assert_eq!(sidecar.port, 50021);
    assert!(sidecar.scalable);
    assert_eq!(
        sidecar.args,
        vec!["--port".to_string(), "{port}".to_string()]
    );
}

#[test]
fn scalable_defaults_to_false_and_args_default_to_empty() {
    let manifest = parse_sidecar_manifest(
        r#"
[[sidecar]]
name = "tts"
reason = "reason"
port = 50021
"#,
    )
    .expect("minimal sidecar manifest should load");

    assert!(!manifest.sidecars[0].scalable);
    assert!(manifest.sidecars[0].args.is_empty());
}

#[test]
fn duplicate_sidecar_name_is_rejected() {
    let err = parse_sidecar_manifest(
        r#"
[[sidecar]]
name = "tts"
reason = "a"
port = 50021

[[sidecar]]
name = "tts"
reason = "b"
port = 50030
"#,
    )
    .expect_err("duplicate sidecar names must be rejected");
    assert!(matches!(err, ManifestError::BadSidecar(_)));
}

#[test]
fn bad_sidecar_name_and_empty_reason_are_rejected() {
    assert!(matches!(
        parse_sidecar_manifest("[[sidecar]]\nname = \"TTS\"\nreason = \"a\"\nport = 1\n")
            .expect_err("uppercase name must be rejected"),
        ManifestError::BadSidecar(_)
    ));
    assert!(matches!(
        parse_sidecar_manifest("[[sidecar]]\nname = \"tts\"\nreason = \"  \"\nport = 1\n")
            .expect_err("blank reason must be rejected"),
        ManifestError::BadSidecar(_)
    ));
}

#[test]
fn sidecar_fingerprint_is_stable_and_changes_with_the_request() {
    let manifest = parse_sidecar_manifest(
        "[[sidecar]]\nname = \"tts\"\nreason = \"a\"\nargs = [\"--port\", \"{port}\"]\nport = 50021\n",
    )
    .unwrap();
    let first = manifest.sidecar_fingerprint("tts").expect("fingerprint");
    assert_eq!(first, manifest.sidecar_fingerprint("tts").unwrap());
    assert_eq!(manifest.sidecar_fingerprint("nope"), None);

    let changed = parse_sidecar_manifest(
        "[[sidecar]]\nname = \"tts\"\nreason = \"a\"\nargs = [\"--port\", \"{port}\"]\nport = 50022\n",
    )
    .unwrap();
    assert_ne!(first, changed.sidecar_fingerprint("tts").unwrap());
}

fn parse_fs_manifest(body: &str) -> Result<Manifest, ManifestError> {
    let dir = tempfile::tempdir().unwrap();
    let plugin_dir = dir.path().join("fs-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            "id = \"fs-plugin\"\nname = \"FS\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n{body}"
        ),
    )
    .unwrap();
    load_manifest(&plugin_dir)
}

#[test]
fn filesystem_block_is_parsed() {
    let manifest = parse_fs_manifest(
        "[[filesystem]]\nname = \"exports\"\nreason = \"CSV を書き出すため\"\nmode = \"read-write\"\n",
    )
    .expect("valid filesystem manifest should load");

    assert_eq!(manifest.filesystem.len(), 1);
    assert_eq!(manifest.filesystem[0].name, "exports");
    assert_eq!(manifest.filesystem[0].mode, FilesystemMode::ReadWrite);
}

#[test]
fn read_only_mode_is_parsed() {
    let manifest = parse_fs_manifest(
        "[[filesystem]]\nname = \"input\"\nreason = \"読むだけ\"\nmode = \"read\"\n",
    )
    .unwrap();
    assert_eq!(manifest.filesystem[0].mode, FilesystemMode::Read);
}

#[test]
fn unknown_mode_duplicate_name_and_blank_reason_are_rejected() {
    assert!(matches!(
        parse_fs_manifest("[[filesystem]]\nname = \"a\"\nreason = \"r\"\nmode = \"write\"\n")
            .expect_err("unknown mode"),
        ManifestError::Parse(_) | ManifestError::BadFilesystem(_)
    ));
    assert!(matches!(
        parse_fs_manifest(
            "[[filesystem]]\nname = \"a\"\nreason = \"r\"\nmode = \"read\"\n\n[[filesystem]]\nname = \"a\"\nreason = \"r2\"\nmode = \"read\"\n"
        )
        .expect_err("duplicate name"),
        ManifestError::BadFilesystem(_)
    ));
    assert!(matches!(
        parse_fs_manifest("[[filesystem]]\nname = \"a\"\nreason = \"  \"\nmode = \"read\"\n")
            .expect_err("blank reason"),
        ManifestError::BadFilesystem(_)
    ));
    assert!(matches!(
        parse_fs_manifest("[[filesystem]]\nname = \"Exports\"\nreason = \"r\"\nmode = \"read\"\n")
            .expect_err("uppercase name"),
        ManifestError::BadFilesystem(_)
    ));
}

#[test]
fn filesystem_fingerprint_is_stable_and_changes_with_the_request() {
    let manifest =
        parse_fs_manifest("[[filesystem]]\nname = \"exports\"\nreason = \"r\"\nmode = \"read\"\n")
            .unwrap();
    let first = manifest.filesystem_fingerprint("exports").unwrap();
    assert_eq!(first, manifest.filesystem_fingerprint("exports").unwrap());
    assert_eq!(manifest.filesystem_fingerprint("nope"), None);

    let changed = parse_fs_manifest(
        "[[filesystem]]\nname = \"exports\"\nreason = \"r\"\nmode = \"read-write\"\n",
    )
    .unwrap();
    assert_ne!(first, changed.filesystem_fingerprint("exports").unwrap());
}

fn manifest_with_bus(bus: Vec<BusRequest>) -> Manifest {
    Manifest {
        id: "translator".into(),
        name: "Translator".into(),
        version: "0.1.0".into(),
        description: String::new(),
        entry: "plugin.wasm".into(),
        events: Vec::new(),
        settings: Vec::new(),
        capabilities: Vec::new(),
        sidecars: Vec::new(),
        filesystem: Vec::new(),
        bus,
        dashboard: Vec::new(),
        schedules: Vec::new(),
    }
}

#[test]
fn parses_bus_requests() {
    // NOTE: deviates from the brief's literal test body -- the brief wrote
    // manifest.toml directly under a randomly-named tempdir with
    // `id = "translator"` and an `entry = "plugin.wasm"` that is never
    // created. That trips the pre-existing `IdMismatch`/`MissingEntry`
    // checks in `load_manifest` (id must equal the plugin directory name;
    // entry file must exist), which are unrelated to this task's bus
    // parsing. Following the same `plugin_dir` + stub entry file
    // convention already used by `parse_sidecar_manifest` /
    // `parse_fs_manifest` in this same file instead.
    let dir = tempfile::tempdir().unwrap();
    let plugin_dir = dir.path().join("translator");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.wasm"), b"\0asm").unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
id = "translator"
name = "Translator"
version = "0.1.0"
entry = "plugin.wasm"

[[bus]]
driver = "ed-state"
publish = ["ship-status"]
subscribe = ["current-system"]
reason = "現在システムを購読して翻訳先を切り替えるため"
"#,
    )
    .unwrap();
    let manifest = load_manifest(&plugin_dir).unwrap();
    let request = manifest
        .bus_request("ed-state")
        .expect("bus request parsed");
    assert_eq!(request.publish, vec!["ship-status".to_string()]);
    assert_eq!(request.subscribe, vec!["current-system".to_string()]);
}

#[test]
fn rejects_a_bus_block_with_neither_publish_nor_subscribe() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("manifest.toml"),
        r#"
id = "translator"
name = "Translator"
version = "0.1.0"
entry = "plugin.wasm"

[[bus]]
driver = "ed-state"
reason = "何もしない"
"#,
    )
    .unwrap();
    assert!(load_manifest(dir.path()).is_err());
}

#[test]
fn rejects_duplicate_bus_drivers() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("manifest.toml"),
        r#"
id = "translator"
name = "Translator"
version = "0.1.0"
entry = "plugin.wasm"

[[bus]]
driver = "ed-state"
publish = ["a"]
reason = "one"

[[bus]]
driver = "ed-state"
publish = ["b"]
reason = "two"
"#,
    )
    .unwrap();
    assert!(load_manifest(dir.path()).is_err());
}

/// Regression test for a Minor review finding: `validate_bus` rejected
/// duplicate `[[bus]]` blocks for the same driver, but not duplicate
/// topic names *within* one block's `publish`/`subscribe` list.
/// `subscribe = ["a", "a"]` used to be accepted and created two separate
/// subscriptions (`crate::manifest::driver::validate_topics` already
/// dedupes `[[topics]]` the same way; this brings `[[bus]]` in line).
#[test]
fn rejects_duplicate_topics_within_one_bus_blocks_publish_or_subscribe() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("manifest.toml"),
        r#"
id = "translator"
name = "Translator"
version = "0.1.0"
entry = "plugin.wasm"

[[bus]]
driver = "ed-state"
subscribe = ["current-system", "current-system"]
reason = "duplicate subscribe topic"
"#,
    )
    .unwrap();
    assert!(
        load_manifest(dir.path()).is_err(),
        "a duplicated subscribe topic within one [[bus]] block must be rejected"
    );

    let dir2 = tempfile::tempdir().unwrap();
    std::fs::write(
        dir2.path().join("manifest.toml"),
        r#"
id = "translator"
name = "Translator"
version = "0.1.0"
entry = "plugin.wasm"

[[bus]]
driver = "ed-state"
publish = ["set-system", "set-system"]
reason = "duplicate publish topic"
"#,
    )
    .unwrap();
    assert!(
        load_manifest(dir2.path()).is_err(),
        "a duplicated publish topic within one [[bus]] block must be rejected"
    );
}

#[test]
fn bus_fingerprint_changes_with_the_requested_topics() {
    let base = BusRequest {
        driver: "ed-state".into(),
        publish: vec!["a".into()],
        subscribe: vec![],
        reason: "r".into(),
    };
    let mut widened = base.clone();
    widened.publish.push("b".into());

    let m1 = manifest_with_bus(vec![base]);
    let m2 = manifest_with_bus(vec![widened]);
    assert_ne!(
        m1.bus_fingerprint("ed-state"),
        m2.bus_fingerprint("ed-state")
    );
}

#[test]
fn bus_fingerprint_ignores_topic_order() {
    let a = BusRequest {
        driver: "ed-state".into(),
        publish: vec!["a".into(), "b".into()],
        subscribe: vec![],
        reason: "r".into(),
    };
    let mut reordered = a.clone();
    reordered.publish.reverse();
    assert_eq!(
        manifest_with_bus(vec![a]).bus_fingerprint("ed-state"),
        manifest_with_bus(vec![reordered]).bus_fingerprint("ed-state")
    );
}

/// dashboard セクションだけ差し替えた manifest をロードするヘルパー。
fn load_with_dashboard_section(section: &str) -> Result<Manifest, ManifestError> {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("widgety");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        &format!(
            "id = \"widgety\"\nname = \"W\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n{section}"
        ),
    );
    load_manifest(&plugin_dir)
}

#[test]
fn dashboard_section_parses_and_validates() {
    let manifest = load_with_dashboard_section(
        "[[dashboard]]\nid = \"status\"\ntitle = \"Status\"\nentry = \"ui/status/index.html\"\nsize = \"medium\"\n",
    )
    .expect("dashboard manifest should parse");
    assert_eq!(manifest.dashboard.len(), 1);
    let w = manifest.dashboard_widget("status").expect("widget present");
    assert_eq!(w.title, "Status");
    assert_eq!(w.size, WidgetSize::Medium);
    assert_eq!(w.size.as_str(), "medium");
    assert!(manifest.dashboard_widget("missing").is_none());
}

#[test]
fn dashboard_rejects_bad_id_duplicate_and_traversal_entry() {
    let bad_id = load_with_dashboard_section(
        "[[dashboard]]\nid = \"Bad_ID\"\ntitle = \"t\"\nentry = \"ui/a.html\"\nsize = \"small\"\n",
    );
    assert!(matches!(bad_id, Err(ManifestError::BadDashboard(_))));

    let dup = load_with_dashboard_section(
        "[[dashboard]]\nid = \"a\"\ntitle = \"t\"\nentry = \"ui/a.html\"\nsize = \"small\"\n\n[[dashboard]]\nid = \"a\"\ntitle = \"t\"\nentry = \"ui/b.html\"\nsize = \"small\"\n",
    );
    assert!(matches!(dup, Err(ManifestError::BadDashboard(_))));

    let traversal = load_with_dashboard_section(
        "[[dashboard]]\nid = \"a\"\ntitle = \"t\"\nentry = \"../outside.html\"\nsize = \"small\"\n",
    );
    assert!(matches!(traversal, Err(ManifestError::BadDashboard(_))));

    let absolute = load_with_dashboard_section(
        "[[dashboard]]\nid = \"a\"\ntitle = \"t\"\nentry = \"/etc/passwd\"\nsize = \"small\"\n",
    );
    assert!(matches!(absolute, Err(ManifestError::BadDashboard(_))));

    let empty_title = load_with_dashboard_section(
        "[[dashboard]]\nid = \"a\"\ntitle = \"  \"\nentry = \"ui/a.html\"\nsize = \"small\"\n",
    );
    assert!(matches!(empty_title, Err(ManifestError::BadDashboard(_))));
}

#[test]
fn dashboard_entry_missing_file_does_not_fail_load() {
    // entry ファイル不在はロード成功(resolved 判定は Registry 側の責務)
    let manifest = load_with_dashboard_section(
        "[[dashboard]]\nid = \"a\"\ntitle = \"t\"\nentry = \"ui/nonexistent.html\"\nsize = \"large\"\n",
    );
    assert!(manifest.is_ok());
}

fn manifest_with_dashboard_widget(widget: DashboardWidget) -> Manifest {
    Manifest {
        id: "p".into(),
        name: "P".into(),
        version: "0".into(),
        description: String::new(),
        entry: "plugin.wasm".into(),
        events: vec![],
        settings: vec![],
        capabilities: vec![],
        sidecars: vec![],
        filesystem: vec![],
        bus: vec![],
        dashboard: vec![widget],
        schedules: vec![],
    }
}

#[test]
fn dashboard_fingerprint_changes_with_each_field() {
    let widget = |title: &str, entry: &str, size: WidgetSize| DashboardWidget {
        id: "a".into(),
        title: title.into(),
        entry: entry.into(),
        size,
    };
    let base = manifest_with_dashboard_widget(widget("t", "ui/a.html", WidgetSize::Small));
    let fp = base.dashboard_fingerprint("a").unwrap();
    assert_eq!(fp, base.dashboard_fingerprint("a").unwrap());
    assert_ne!(
        fp,
        manifest_with_dashboard_widget(widget("t2", "ui/a.html", WidgetSize::Small))
            .dashboard_fingerprint("a")
            .unwrap()
    );
    assert_ne!(
        fp,
        manifest_with_dashboard_widget(widget("t", "ui/b.html", WidgetSize::Small))
            .dashboard_fingerprint("a")
            .unwrap()
    );
    assert_ne!(
        fp,
        manifest_with_dashboard_widget(widget("t", "ui/a.html", WidgetSize::Large))
            .dashboard_fingerprint("a")
            .unwrap()
    );
    assert!(base.dashboard_fingerprint("missing").is_none());
}

/// `schedule` セクションだけを差し替えた manifest を組み立てて `load_manifest`
/// に通すヘルパー。他の `load_with_*_section` ヘルパーと同じ流儀:
/// id/name/version/entry は固定で、呼び出し側は `[[schedule]]` の中身だけ渡す。
fn try_manifest_from(section: &str) -> Result<Manifest, ManifestError> {
    let tmp = tempfile::tempdir().unwrap();
    let plugin_dir = tmp.path().join("schedule-plugin");
    fs::create_dir_all(&plugin_dir).unwrap();
    write_entry(&plugin_dir, "plugin.wasm");
    write_manifest(
        &plugin_dir,
        &format!(
            "id = \"schedule-plugin\"\nname = \"Schedule Plugin\"\nversion = \"0.1.0\"\nentry = \"plugin.wasm\"\n{section}"
        ),
    );
    load_manifest(&plugin_dir)
}

fn manifest_from(section: &str) -> Manifest {
    try_manifest_from(section).expect("manifest should parse")
}

#[test]
fn schedule_with_interval_is_parsed() {
    let m = manifest_from(
        r#"
[[schedule]]
name = "flush"
interval-seconds = 60
"#,
    );
    assert_eq!(m.schedules.len(), 1);
    assert_eq!(m.schedules[0].name, "flush");
    assert!(matches!(
        m.schedules[0].spec,
        ScheduleSpec::IntervalSeconds(60)
    ));
}

#[test]
fn schedule_with_cron_is_parsed() {
    let m = manifest_from(
        r#"
[[schedule]]
name = "daily-report"
cron = "0 9 * * *"
"#,
    );
    assert_eq!(m.schedules.len(), 1);
    assert_eq!(m.schedules[0].name, "daily-report");
    assert_eq!(
        m.schedules[0].spec,
        ScheduleSpec::Cron("0 9 * * *".to_string())
    );
}

#[test]
fn schedule_requires_exactly_one_of_interval_and_cron() {
    let both = try_manifest_from(
        r#"
[[schedule]]
name = "both"
interval-seconds = 60
cron = "0 9 * * *"
"#,
    );
    // The interval-seconds/cron exclusivity check runs inside
    // `ScheduleRequest`'s `Deserialize` impl (it only needs the single
    // table's own fields, not the whole schedule list), so a violation
    // surfaces as a TOML deserialize failure (`ManifestError::Parse`),
    // the same way an unrecognized `[[capabilities]] kind` does.
    assert!(matches!(both, Err(ManifestError::Parse(_))));

    let neither = try_manifest_from(
        r#"
[[schedule]]
name = "neither"
"#,
    );
    assert!(matches!(neither, Err(ManifestError::Parse(_))));
}

#[test]
fn schedule_rejects_bad_names_and_duplicates() {
    let bad_name = try_manifest_from(
        r#"
[[schedule]]
name = "Bad_Name"
interval-seconds = 60
"#,
    );
    assert!(matches!(bad_name, Err(ManifestError::BadSchedule(_))));

    let duplicate = try_manifest_from(
        r#"
[[schedule]]
name = "flush"
interval-seconds = 60

[[schedule]]
name = "flush"
interval-seconds = 30
"#,
    );
    assert!(matches!(duplicate, Err(ManifestError::BadSchedule(_))));
}

#[test]
fn schedule_rejects_invalid_cron_expression() {
    let err = try_manifest_from(
        r#"
[[schedule]]
name = "bad-cron"
cron = "not a cron"
"#,
    );
    // Parsed and rejected inside `ScheduleRequest::deserialize` via
    // `cron::Schedule::from_str`, so it surfaces as a TOML parse error
    // (still: a manifest error, so the plugin becomes Disabled).
    assert!(matches!(err, Err(ManifestError::Parse(_))));
}

/// Issue manifest-rjoa の再現。TOML では、テーブルヘッダより後ろに書いた
/// キーはそのテーブルの子になる。`[[sidecar]]` の後ろに置いた `settings` は
/// `sidecar[0].settings` として解釈され、以前はそのまま黙って捨てられていた。
#[test]
fn rejects_a_top_level_key_written_after_a_table_header() {
    let err = try_manifest_from(
        r#"
[[sidecar]]
name = "worker"
reason = "音声合成を行う"
port = 51000

settings = [{ key = "voice", label = "Voice", type = "string", default = "a" }]
"#,
    );
    let err = err.expect_err("a stray key inside [[sidecar]] should be rejected");
    match err {
        ManifestError::Parse(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("settings"),
                "error should name the offending key: {msg}"
            );
        }
        other => panic!("expected ManifestError::Parse, got {other:?}"),
    }
}

/// `[[settings]]` の後ろに書いてしまったトップレベルキーも、同じ経路
/// (設定フィールドの知らないキー)で弾かれる。
#[test]
fn rejects_an_unknown_key_inside_a_settings_table() {
    let err = try_manifest_from(
        r#"
[[settings]]
key = "greeting"
label = "Greeting"
type = "string"
default = "hello"
events = ["FSDJump"]
"#,
    );
    let err = err.expect_err("a stray key inside [[settings]] should be rejected");
    assert!(
        matches!(err, ManifestError::Parse(_)),
        "expected ManifestError::Parse, got {err:?}"
    );
}

#[test]
fn rejects_an_unknown_key_inside_a_capabilities_table() {
    let err = try_manifest_from(
        r#"
[[capabilities]]
kind = "http"
hosts = ["https://example.com"]
reason = "r"
events = ["FSDJump"]
"#,
    );
    let err = err.expect_err("a stray key inside [[capabilities]] should be rejected");
    assert!(
        matches!(err, ManifestError::Parse(_)),
        "expected ManifestError::Parse, got {err:?}"
    );
}

#[test]
fn unknown_top_level_keys_are_reported() {
    let unknown = unknown_top_level_keys(
        r#"
id = "sample-plugin"
name = "Sample"
version = "0.1.0"
entry = "plugin.wasm"
evens = ["FSDJump"]
"#,
        MANIFEST_TOP_LEVEL_KEYS,
    );
    assert_eq!(unknown, vec!["evens".to_string()]);
}

#[test]
fn a_manifest_using_only_known_top_level_keys_reports_nothing() {
    let unknown = unknown_top_level_keys(
        r#"
id = "sample-plugin"
name = "Sample"
version = "0.1.0"
description = "d"
entry = "plugin.wasm"
events = ["*"]

[[settings]]
key = "a"
label = "A"
type = "string"
default = ""

[[capabilities]]
kind = "http"
hosts = ["https://example.com"]
reason = "r"

[[sidecar]]
name = "worker"
reason = "r"
port = 51000

[[filesystem]]
name = "logs"
reason = "r"
mode = "read"

[[bus]]
driver = "ed-state"
subscribe = ["current-system"]
reason = "r"

[[dashboard]]
id = "w"
title = "W"
entry = "w.html"
size = "small"

[[schedule]]
name = "flush"
interval-seconds = 60
"#,
        MANIFEST_TOP_LEVEL_KEYS,
    );
    assert!(unknown.is_empty(), "unexpected unknown keys: {unknown:?}");
}

/// 知らないトップレベルキーは(前方互換のため)エラーにはせず、warn で
/// 報せるだけ — ロード自体は成功する。
#[test]
fn an_unknown_top_level_key_does_not_fail_the_load() {
    assert!(try_manifest_from("evens = [\"FSDJump\"]\n").is_ok());
}

#[test]
fn schedule_rejects_zero_interval() {
    let err = try_manifest_from(
        r#"
[[schedule]]
name = "zero"
interval-seconds = 0
"#,
    );
    assert!(matches!(err, Err(ManifestError::Parse(_))));
}

#[test]
fn select_accepts_options_from_a_driver_topic() {
    let m = manifest_from(
        r#"
[[settings]]
key = "speaker"
label = "話者"
type = "select"
default = ""
options-from = { driver = "coeiroink", topic = "speakers" }
"#,
    );

    assert_eq!(
        m.settings[0],
        SettingField::Select {
            key: "speaker".into(),
            label: "話者".into(),
            default: String::new(),
            options: None,
            options_from: Some(OptionsFrom {
                driver: "coeiroink".into(),
                topic: "speakers".into(),
            }),
        }
    );
}

/// 静的な候補は `"foo"` と `{ value, label }` を混ぜて書ける。
#[test]
fn static_options_accept_both_plain_strings_and_labeled_objects() {
    let m = manifest_from(
        r#"
[[settings]]
key = "mode"
label = "Mode"
type = "select"
default = "a"
options = ["a", { value = "b", label = "そのB" }]
"#,
    );

    let SettingField::Select { options, .. } = &m.settings[0] else {
        panic!("expected a select field");
    };
    assert_eq!(
        options.as_deref(),
        Some(
            [
                SelectOption {
                    value: "a".into(),
                    label: "a".into()
                },
                SelectOption {
                    value: "b".into(),
                    label: "そのB".into()
                },
            ]
            .as_slice()
        )
    );
}

#[test]
fn select_rejects_both_options_and_options_from() {
    let err = try_manifest_from(
        r#"
[[settings]]
key = "speaker"
label = "話者"
type = "select"
default = ""
options = ["a"]
options-from = { driver = "coeiroink", topic = "speakers" }
"#,
    );
    assert!(
        matches!(&err, Err(ManifestError::BadSetting(msg)) if msg.contains("not both")),
        "expected BadSetting, got {err:?}"
    );
}

#[test]
fn select_rejects_neither_options_nor_options_from() {
    let err = try_manifest_from(
        r#"
[[settings]]
key = "speaker"
label = "話者"
type = "select"
default = ""
"#,
    );
    assert!(
        matches!(&err, Err(ManifestError::BadSetting(msg)) if msg.contains("must specify one of")),
        "expected BadSetting, got {err:?}"
    );
}

#[test]
fn select_rejects_empty_static_options() {
    let err = try_manifest_from(
        r#"
[[settings]]
key = "mode"
label = "Mode"
type = "select"
default = ""
options = []
"#,
    );
    assert!(
        matches!(&err, Err(ManifestError::BadSetting(msg)) if msg.contains("must not be empty")),
        "expected BadSetting, got {err:?}"
    );
}

#[test]
fn slider_is_parsed_and_step_defaults_to_one() {
    let m = manifest_from(
        r#"
[[settings]]
key = "volume"
label = "音量"
type = "slider"
default = 50
min = 0
max = 100
"#,
    );
    assert_eq!(
        m.settings[0],
        SettingField::Slider {
            key: "volume".into(),
            label: "音量".into(),
            default: 50.0,
            min: 0.0,
            max: 100.0,
            step: 1.0,
        }
    );
    assert_eq!(m.settings[0].default_value(), serde_json::json!(50.0));
}

#[test]
fn slider_parses_an_explicit_step() {
    let m = manifest_from(
        r#"
[[settings]]
key = "volume"
label = "音量"
type = "slider"
default = 50
min = 0
max = 100
step = 5
"#,
    );
    let SettingField::Slider { step, .. } = &m.settings[0] else {
        panic!("expected a slider, got {:?}", m.settings[0]);
    };
    assert_eq!(*step, 5.0);
}

#[test]
fn slider_rejects_min_not_less_than_max() {
    let err = try_manifest_from(
        r#"
[[settings]]
key = "volume"
label = "音量"
type = "slider"
default = 50
min = 100
max = 100
"#,
    );
    assert!(
        matches!(&err, Err(ManifestError::BadSetting(msg)) if msg.contains("min must be less than max")),
        "expected BadSetting, got {err:?}"
    );
}

#[test]
fn slider_rejects_a_default_outside_the_range() {
    let err = try_manifest_from(
        r#"
[[settings]]
key = "volume"
label = "音量"
type = "slider"
default = 101
min = 0
max = 100
"#,
    );
    assert!(
        matches!(&err, Err(ManifestError::BadSetting(msg)) if msg.contains("default must be between min and max")),
        "expected BadSetting, got {err:?}"
    );
}

#[test]
fn slider_rejects_a_non_positive_step() {
    let err = try_manifest_from(
        r#"
[[settings]]
key = "volume"
label = "音量"
type = "slider"
default = 50
min = 0
max = 100
step = 0
"#,
    );
    assert!(
        matches!(&err, Err(ManifestError::BadSetting(msg)) if msg.contains("step must be greater than 0")),
        "expected BadSetting, got {err:?}"
    );
}

#[test]
fn select_rejects_options_from_with_a_bad_driver_id() {
    let err = try_manifest_from(
        r#"
[[settings]]
key = "speaker"
label = "話者"
type = "select"
default = ""
options-from = { driver = "COEIROINK", topic = "speakers" }
"#,
    );
    assert!(
        matches!(&err, Err(ManifestError::BadSetting(msg)) if msg.contains("driver must match")),
        "expected BadSetting, got {err:?}"
    );
}

#[test]
fn select_rejects_options_from_with_a_bad_topic() {
    let err = try_manifest_from(
        r#"
[[settings]]
key = "speaker"
label = "話者"
type = "select"
default = ""
options-from = { driver = "coeiroink", topic = "Speakers!" }
"#,
    );
    assert!(
        matches!(&err, Err(ManifestError::BadSetting(msg)) if msg.contains("topic must match")),
        "expected BadSetting, got {err:?}"
    );
}

/// RPC 応答は静的・動的のどちらでも `{value,label}` の配列に揃え、未解決は
/// `null` で出す(UI が 1 つの経路で描けるように)。
#[test]
fn select_serializes_options_as_value_label_pairs() {
    let field = SettingField::Select {
        key: "mode".into(),
        label: "Mode".into(),
        default: "a".into(),
        options: Some(vec!["a".into()]),
        options_from: None,
    };

    assert_eq!(
        serde_json::to_value(&field).unwrap(),
        serde_json::json!({
            "type": "select",
            "key": "mode",
            "label": "Mode",
            "default": "a",
            "options": [{ "value": "a", "label": "a" }],
        })
    );
}

#[test]
fn an_unresolved_select_serializes_options_as_null_and_keeps_options_from() {
    let field = SettingField::Select {
        key: "speaker".into(),
        label: "話者".into(),
        default: String::new(),
        options: None,
        options_from: Some(OptionsFrom {
            driver: "coeiroink".into(),
            topic: "speakers".into(),
        }),
    };

    assert_eq!(
        serde_json::to_value(&field).unwrap(),
        serde_json::json!({
            "type": "select",
            "key": "speaker",
            "label": "話者",
            "default": "",
            "options": null,
            "optionsFrom": { "driver": "coeiroink", "topic": "speakers" },
        })
    );
}
