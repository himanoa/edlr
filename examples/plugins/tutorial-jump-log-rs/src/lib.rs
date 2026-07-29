//! `docs/plugin-tutorial-rust.md` で育てるプラグインの完成形。
//!
//! FSDJump を拾って、
//!
//! - 設定(`enabled` / `minDistance`)でふるいに掛け
//! - `tutorial-tracker-rs` ドライバへ訪問先を publish し
//! - キューに積んで、定期実行のたびに 1 件だけ EDSM へ問い合わせる
//!
//! HTTP をイベント処理ではなく定期実行の側でやるのは意図的。理由は
//! チュートリアルの 5 章を参照。

wit_bindgen::generate!({
    path: "../../../core/wit",
    world: "plugin-guest",
    generate_all,
});

use std::cell::RefCell;

use edlr::plugin::{bus, driver_http, host_log, host_settings};

/// 連携するドライバの id(`manifest.toml` の `[[bus]]` と一致させる)。
const TRACKER: &str = "tutorial-tracker-rs";

/// EDSM の問い合わせ先。`manifest.toml` の `[[capabilities]]` で
/// `https://www.edsm.net` を要求し、ユーザーが承認したときだけ通る。
const EDSM: &str = "https://www.edsm.net/api-v1/system";

/// 未処理のジャンプを溜めておく上限。EDSM への問い合わせは 1 回の
/// `on-schedule` につき 1 件しか流さないので、際限なく溜めない。
const QUEUE_CAP: usize = 50;

thread_local! {
    static PENDING: RefCell<Vec<Jump>> = const { RefCell::new(Vec::new()) };
}

struct Jump {
    system: String,
    distance: f64,
}

struct Settings {
    enabled: bool,
    min_distance: f64,
}

struct Component;

impl Guest for Component {
    fn init() {
        host_log::log(host_log::Level::Info, "tutorial-jump-log started");
    }

    fn on_event(ev: Event) {
        // `manifest.toml` の `events` で絞ってはいるが、購読を増やしたときに
        // 壊れないよう、ここでも名前を確かめる。
        if ev.name.as_deref() != Some("FSDJump") {
            return;
        }

        let settings = settings();
        if !settings.enabled {
            return;
        }

        let payload: serde_json::Value = match serde_json::from_str(&ev.payload_json) {
            Ok(v) => v,
            Err(e) => {
                host_log::log(host_log::Level::Warn, &format!("broken payload: {e}"));
                return;
            }
        };
        let system = payload["StarSystem"].as_str().unwrap_or("").to_string();
        let distance = payload["JumpDist"].as_f64().unwrap_or(0.0);
        if system.is_empty() {
            return;
        }
        if distance < settings.min_distance {
            // debug ではなく info。デーモンの既定ログレベルは info なので、
            // debug にすると `RUST_LOG=debug` 無しでは見えない
            // (チュートリアル 2 章の「確認する」参照)。
            host_log::log(
                host_log::Level::Info,
                &format!("skipping {system} ({distance:.2} ly < {:.2} ly)", settings.min_distance),
            );
            return;
        }

        host_log::log(
            host_log::Level::Info,
            &format!("jumped to {system} ({distance:.2} ly)"),
        );

        // ドライバへ渡す。未承認のうちは permission-denied になるが、
        // プラグイン自体は動き続けてよいので、ログに出して先へ進む。
        if let Err(e) = bus::publish(TRACKER, "visit", system.as_bytes()) {
            host_log::log(host_log::Level::Warn, &format!("publish failed: {e:?}"));
        }

        PENDING.with(|q| {
            let mut q = q.borrow_mut();
            if q.len() >= QUEUE_CAP {
                q.remove(0);
            }
            q.push(Jump { system, distance });
        });
    }

    /// ドライバが `last-system` へ流し直した値がここへ届く
    /// (`manifest.toml` の `[[bus]]` の `subscribe` に書いたトピックだけ)。
    fn on_message(driver: String, topic: String, payload: Vec<u8>) {
        host_log::log(
            host_log::Level::Info,
            &format!("{driver}/{topic} = {}", String::from_utf8_lossy(&payload)),
        );
    }

    fn on_schedule(name: String) {
        if name != "flush" {
            host_log::log(host_log::Level::Warn, &format!("unknown schedule: {name}"));
            return;
        }

        let Some(jump) = PENDING.with(|q| {
            let mut q = q.borrow_mut();
            if q.is_empty() {
                None
            } else {
                Some(q.remove(0))
            }
        }) else {
            return;
        };

        // 呼び出し期限は 2 秒、driver-http のタイムアウトは 1.5 秒。
        // 1 回の呼び出しで HTTP を叩けるのは実質 1 回だけ。
        lookup(&jump);

        let remaining = PENDING.with(|q| q.borrow().len());
        if remaining > 0 {
            host_log::log(
                host_log::Level::Info,
                &format!("{remaining} jump(s) still queued"),
            );
        }

        // retain されている最新値は、配信を待たずいつでも読める。
        match bus::get(TRACKER, "last-system") {
            Ok(Some(v)) => host_log::log(
                host_log::Level::Info,
                &format!("tracker says: {}", String::from_utf8_lossy(&v)),
            ),
            Ok(None) => {}
            Err(e) => host_log::log(host_log::Level::Warn, &format!("get failed: {e:?}")),
        }
    }

    /// デーモンの graceful shutdown で一度だけ呼ばれる。猶予は 5 秒しかなく、
    /// ここで HTTP を叩くと間に合わないことがあるので、残りはログへ落とすだけ。
    fn on_stop() {
        let pending = PENDING.with(|q| {
            q.borrow()
                .iter()
                .map(|j| format!("{} ({:.2} ly)", j.system, j.distance))
                .collect::<Vec<_>>()
        });
        if pending.is_empty() {
            return;
        }
        host_log::log(
            host_log::Level::Info,
            &format!("stopping with {} unflushed: {}", pending.len(), pending.join(", ")),
        );
    }
}

/// 設定は毎回読み直す。UI で変えた値が次のイベントから効くのはこのため。
fn settings() -> Settings {
    let raw = host_settings::get_all();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
    Settings {
        enabled: v["enabled"].as_bool().unwrap_or(true),
        min_distance: v["minDistance"].as_f64().unwrap_or(0.0),
    }
}

/// EDSM に星系を問い合わせて、結果をログへ落とす。
fn lookup(jump: &Jump) {
    let url = format!(
        "{EDSM}?systemName={}&showId=1",
        urlencode(&jump.system)
    );
    let req = driver_http::Request {
        method: "GET".to_string(),
        url,
        headers: vec![("accept".to_string(), "application/json".to_string())],
        body: None,
    };

    match driver_http::send(&req) {
        Ok(resp) if resp.status == 200 => {
            let body = String::from_utf8_lossy(&resp.body);
            // 未知の星系では EDSM は空配列 `[]` を返す。
            if body.trim() == "[]" {
                host_log::log(
                    host_log::Level::Info,
                    &format!("{}: not known to EDSM", jump.system),
                );
            } else {
                host_log::log(
                    host_log::Level::Info,
                    &format!("{}: EDSM says {}", jump.system, body.trim()),
                );
            }
        }
        Ok(resp) => host_log::log(
            host_log::Level::Warn,
            &format!("{}: EDSM returned HTTP {}", jump.system, resp.status),
        ),
        Err(e) => host_log::log(
            host_log::Level::Warn,
            &format!("{}: lookup failed: {e:?}", jump.system),
        ),
    }
}

/// 星系名にはスペースやハイフンが入る(`Col 285 Sector AA-A a1`)。
/// 依存を増やさないための最小限のパーセントエンコード。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

export!(Component);
