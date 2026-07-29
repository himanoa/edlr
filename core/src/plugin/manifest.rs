use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::Path;

/// マニフェストの `[[settings]]` テーブル 1 件。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum SettingField {
    Boolean {
        key: String,
        label: String,
        default: bool,
    },
    String {
        key: String,
        label: String,
        default: String,
    },
    Number {
        key: String,
        label: String,
        default: f64,
    },
    Select {
        key: String,
        label: String,
        default: String,
        options: Vec<String>,
    },
    /// API キーなどの秘密情報。**`default` を持たない**(マニフェストに
    /// 秘密情報を書けてしまう余地を作らないため)。値は常に空文字列から始まる。
    ///
    /// `string` との違いは扱いだけで、保存形式は同じ文字列:
    ///
    /// - UI ではマスク入力(`<input type="password">`)になる
    /// - **RPC の読み出し応答には含まれない**(write-only)。`plugins/list` /
    ///   `plugins/get-settings` は値の代わりに「設定済みかどうか」だけを返す
    /// - ログに出さない
    ///
    /// プラグイン自身は `host-settings.get-all` で通常どおり値を受け取る
    /// (受け取れなければ意味が無い)。ここで守っているのは「UI/RPC 越しに
    /// 秘密情報が読み出せてしまう」経路であって、プラグインからの秘匿では
    /// ない -- そもそも秘密情報を渡す相手がそのプラグインである。
    Secret { key: String, label: String },
    /// ユーザーが UI からキーと値のペアを動的に追加・削除できる設定項目。
    /// 値は **`string -> string` の JSON オブジェクト**に限る(値に number /
    /// bool / 入れ子は許さない -- 単純さを優先し、必要になってから広げる)。
    ///
    /// **`default` を持たない**: 何件のペアが要るかを決めるのはユーザーで
    /// あって、マニフェスト側が初期の行を用意する意味がないため。値は常に
    /// 空オブジェクト `{}` から始まる。
    Map { key: String, label: String },
}

impl SettingField {
    pub fn key(&self) -> &str {
        match self {
            SettingField::Boolean { key, .. } => key,
            SettingField::String { key, .. } => key,
            SettingField::Number { key, .. } => key,
            SettingField::Select { key, .. } => key,
            SettingField::Secret { key, .. } => key,
            SettingField::Map { key, .. } => key,
        }
    }

    pub fn default_value(&self) -> serde_json::Value {
        match self {
            SettingField::Boolean { default, .. } => serde_json::Value::Bool(*default),
            SettingField::String { default, .. } => serde_json::Value::String(default.clone()),
            SettingField::Number { default, .. } => {
                serde_json::json!(*default)
            }
            SettingField::Select { default, .. } => serde_json::Value::String(default.clone()),
            // 秘密情報にマニフェスト由来の初期値はない。
            SettingField::Secret { .. } => serde_json::Value::String(String::new()),
            // 行はユーザーが増やす。マニフェスト由来の初期値はない。
            SettingField::Map { .. } => serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    /// 秘密情報として扱うフィールドか(RPC 応答から除外する対象)。
    pub fn is_secret(&self) -> bool {
        matches!(self, SettingField::Secret { .. })
    }
}

/// プラグインが要求する capability(実行時に許可が必要な外部リソースアクセス)。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum CapabilityRequest {
    Http { hosts: Vec<String>, reason: String },
}

/// プラグインが要求するサイドカープロセス 1 件。
///
/// **実行ファイルのパス(`command`)はここに書けない** — 必ずユーザーが
/// UI で入力する。承認画面に出る内容と実際に走るプログラムを、ユーザー自身の
/// 明示的な指定によって一致させるため。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SidecarRequest {
    pub name: String,
    pub reason: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub port: u16,
    #[serde(default)]
    pub scalable: bool,
}

/// `[[filesystem]]` の `mode`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FilesystemMode {
    Read,
    ReadWrite,
}

impl FilesystemMode {
    /// フィンガープリント・RPC 応答で使う安定した文字列表現。
    pub fn as_str(&self) -> &'static str {
        match self {
            FilesystemMode::Read => "read",
            FilesystemMode::ReadWrite => "read-write",
        }
    }

    pub fn allows_write(&self) -> bool {
        matches!(self, FilesystemMode::ReadWrite)
    }
}

/// プラグインが要求するファイルアクセス 1 件。
///
/// **ディレクトリの実パスはここに書けない** -- 必ずユーザーが UI で選ぶ。
/// 承認画面に出る内容と実際にアクセスされる場所を、ユーザー自身の指定に
/// よって一致させるため(`[[sidecar]]` の `command` と同じ原則)。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemRequest {
    pub name: String,
    pub reason: String,
    pub mode: FilesystemMode,
}

/// プラグインが要求するバス接続 1 件。
///
/// **`get` は `subscribe` に宣言したトピックにのみ許される**(「配信は要らないが
/// 最新値は読みたい」という区別は設けない -- 承認画面に出す情報を増やさないため)。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BusRequest {
    pub driver: String,
    #[serde(default)]
    pub publish: Vec<String>,
    #[serde(default)]
    pub subscribe: Vec<String>,
    pub reason: String,
}

/// ダッシュボードウィジェットのサイズ(Dashboard 画面のグリッドの
/// カラムスパンに対応: small=1 / medium=2 / large=3)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WidgetSize {
    Small,
    Medium,
    Large,
}

impl WidgetSize {
    pub fn as_str(&self) -> &'static str {
        match self {
            WidgetSize::Small => "small",
            WidgetSize::Medium => "medium",
            WidgetSize::Large => "large",
        }
    }
}

/// プラグインが宣言するダッシュボードウィジェット 1 件(`[[dashboard]]`)。
///
/// `entry` はプラグインディレクトリからの相対パス。ディレクトリ外への
/// 脱出(`..`・絶対パス)はロード時に拒否するが、ファイルの実在は要求
/// しない -- 不在は Registry が `resolved: false` として UI バッジで報せる
/// (bus の未解決参照と同じセマンティクス)。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardWidget {
    pub id: String,
    pub title: String,
    pub entry: String,
    pub size: WidgetSize,
}

/// `[[schedule]]` 1 件の実行タイミング。`cron::Schedule` は `PartialEq` を
/// 持たず `Manifest` の derive を壊すため、検証済みの元の cron 式文字列を
/// そのまま保持する(実際の `cron::Schedule` への変換は利用側で都度行う)。
#[derive(Debug, Clone, PartialEq)]
pub enum ScheduleSpec {
    IntervalSeconds(u64),
    /// 検証済みの、正規化前(5 欄)の cron 式。
    Cron(String),
}

impl ScheduleSpec {
    /// RPC 応答(`plugins/list` の `schedules[].spec`)・UI 表示に使う安定した
    /// 文字列表現。`IntervalSeconds(n)` は `"every {n}s"`、`Cron(expr)` は
    /// `"cron: {expr}"`(元の 5 欄形式のまま、正規化前の文字列を使う --
    /// ユーザーが manifest.toml に書いた表現と一致させるため)。
    pub fn display_string(&self) -> String {
        match self {
            ScheduleSpec::IntervalSeconds(secs) => format!("every {secs}s"),
            ScheduleSpec::Cron(expr) => format!("cron: {expr}"),
        }
    }
}

/// プラグインが宣言する定期実行 1 件(`[[schedule]]`)。
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleRequest {
    pub name: String,
    pub spec: ScheduleSpec,
    /// 打ち漏らし(デーモンが動いていなかった間に過ぎた定刻)を、次回起動時に
    /// 1 回だけ追い掛けて実行するか。既定は `false`。
    ///
    /// **`cron` にのみ指定できる**。`interval-seconds` は「前回から N 秒後」と
    /// いう経過時間の宣言であって「何時に実行する」ではないため、追い掛ける
    /// べき定刻が存在しない(デーモンが止まっていた間の経過時間を後から
    /// 埋め合わせる意味も無い)。
    ///
    /// flush 系のスケジュールには不要だが、`cron = "0 9 * * *"` の日次レポート
    /// のような用途では、09:00 にデーモンが動いていなかった日が痕跡も無く
    /// スキップされるのは不適切なので、これを opt-in する。
    pub catch_up: bool,
}

/// `[[schedule]]` の生の serde 表現。`interval-seconds` と `cron` は排他
/// (どちらか片方だけが Some)。`ScheduleRequest` の `Deserialize` 実装が
/// これを中間表現として使い、排他性・`interval-seconds > 0`・cron 式の
/// パース可否をその場で検証する(name の字種・一意性は個別のテーブルの
/// 情報だけでは判定できないため、`validate_schedules` が変換後の一覧に対して
/// 別途行う)。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSchedule {
    name: String,
    #[serde(rename = "interval-seconds")]
    interval_seconds: Option<u64>,
    cron: Option<String>,
    #[serde(rename = "catch-up", default)]
    catch_up: bool,
}

impl<'de> serde::Deserialize<'de> for ScheduleRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        use std::str::FromStr;

        let raw = RawSchedule::deserialize(deserializer)?;

        let spec = match (raw.interval_seconds, raw.cron) {
            (Some(_), Some(_)) => {
                return Err(D::Error::custom(format!(
                    "schedule {} must specify exactly one of interval-seconds or cron, not both",
                    raw.name
                )));
            }
            (None, None) => {
                return Err(D::Error::custom(format!(
                    "schedule {} must specify one of interval-seconds or cron",
                    raw.name
                )));
            }
            (Some(interval), None) => {
                if interval == 0 {
                    return Err(D::Error::custom(format!(
                        "schedule {} interval-seconds must be greater than 0",
                        raw.name
                    )));
                }
                if raw.catch_up {
                    // interval には追い掛けるべき「定刻」が無い
                    // (`ScheduleRequest::catch_up` のドキュメント参照)。
                    // 黙って無視すると、書いた人は効いていると思い込むので拒否する。
                    return Err(D::Error::custom(format!(
                        "schedule {} cannot use catch-up with interval-seconds \
                         (catch-up only makes sense for cron, which has scheduled instants)",
                        raw.name
                    )));
                }
                ScheduleSpec::IntervalSeconds(interval)
            }
            (None, Some(cron_expr)) => {
                let normalized = normalize_cron(&cron_expr);
                cron::Schedule::from_str(&normalized).map_err(|e| {
                    D::Error::custom(format!(
                        "schedule {} has an invalid cron expression {cron_expr:?}: {e}",
                        raw.name
                    ))
                })?;
                ScheduleSpec::Cron(cron_expr)
            }
        };

        Ok(ScheduleRequest {
            name: raw.name,
            spec,
            catch_up: raw.catch_up,
        })
    }
}

/// `manifest.toml` のパース結果。
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub entry: String,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub settings: Vec<SettingField>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityRequest>,
    #[serde(default, rename = "sidecar")]
    pub sidecars: Vec<SidecarRequest>,
    #[serde(default)]
    pub filesystem: Vec<FilesystemRequest>,
    #[serde(default, rename = "bus")]
    pub bus: Vec<BusRequest>,
    #[serde(default)]
    pub dashboard: Vec<DashboardWidget>,
    /// プラグインが宣言する定期実行(`[[schedule]]`)。`interval-seconds`/
    /// `cron` の排他性・値の妥当性は `ScheduleRequest` の `Deserialize` で
    /// その場で検証される。name の字種・一意性は `load_manifest` が呼ぶ
    /// `validate_schedules` で検証する。
    #[serde(default, rename = "schedule")]
    pub schedules: Vec<ScheduleRequest>,
}

impl Manifest {
    /// capability 要求一式の安定フィンガープリント(grants の失効判定に使う)。
    ///
    /// - 同じ要求内容なら常に同じ値を返す(プロセスをまたいでも安定)。
    /// - `hosts` の順序は正規化(小文字化してソート)されるため、順序違いは無視される。
    /// - 要求内容が変われば異なる値になる。`reason` や `host` は検証済みとはいえ
    ///   自由記述のフィールドを含むため、区切り文字での結合ではなく長さ接頭辞で
    ///   エンコードして曖昧さ(衝突)を排除する(詳細は `encode_field` を参照)。
    /// - `capabilities` が空なら `None`。
    ///
    /// 戻り値は正規化文字列(`canonical`)の SHA-256 の16進表現。プラグイン作者は
    /// 自分自身のマニフェストの新旧両方を完全に制御できるため、ここに
    /// 非暗号学的ハッシュ(64bit FNV など)を使うと誕生日衝突が現実的な計算量
    /// (~2^32)で作れてしまい、host を追加した新バージョンを「差分なし」に
    /// 見せかけて再承認プロンプトを回避できてしまう。SHA-256 は原像・第二原像・
    /// 衝突のいずれも計算量的に不可能なので、`canonical` が異なれば
    /// フィンガープリントも(実務上)必ず異なる。
    pub fn capabilities_fingerprint(&self) -> Option<String> {
        if self.capabilities.is_empty() {
            return None;
        }

        let mut canonical_requests: Vec<String> = self
            .capabilities
            .iter()
            .map(|req| match req {
                CapabilityRequest::Http { hosts, reason } => {
                    let mut normalized_hosts: Vec<String> =
                        hosts.iter().map(|h| h.to_lowercase()).collect();
                    normalized_hosts.sort();

                    let mut encoded = encode_field("http");
                    encoded.push_str(&encode_field(&normalized_hosts.len().to_string()));
                    for host in &normalized_hosts {
                        encoded.push_str(&encode_field(host));
                    }
                    encoded.push_str(&encode_field(reason));
                    encoded
                }
            })
            .collect();
        canonical_requests.sort();

        let mut canonical = encode_field(&canonical_requests.len().to_string());
        for request in &canonical_requests {
            canonical.push_str(&encode_field(request));
        }

        Some(sha256_hex(&canonical))
    }

    /// `Http` capability 要求の host 一覧を平坦化して返す(重複を含みうる)。
    /// `driver-http` の許可判定に使う許可リストの元になる。
    ///
    /// 明示的に `Http` variant だけを `filter_map` で拾う書き方にしているのは、
    /// 将来 `CapabilityRequest` に別の kind(例えば filesystem)が追加された
    /// ときに、その kind の host らしきフィールドが黙って http 許可リストへ
    /// 混入するのを防ぐため。新しい kind を http 許可リストにも含めたい場合は
    /// このマッチ節を明示的に増やす必要がある。
    pub fn capability_hosts(&self) -> Vec<String> {
        self.capabilities
            .iter()
            .flat_map(|req| match req {
                // Exhaustive match, not a wildcard/`if let` -- adding a new
                // `CapabilityRequest` variant forces a compile error here
                // rather than silently falling through and (for a wildcard
                // arm) either joining or dropping its hosts by accident.
                CapabilityRequest::Http { hosts, .. } => hosts.clone(),
            })
            .collect()
    }

    pub fn sidecar(&self, name: &str) -> Option<&SidecarRequest> {
        self.sidecars.iter().find(|s| s.name == name)
    }

    /// サイドカー 1 件の要求内容の安定フィンガープリント(grants の失効判定に使う)。
    ///
    /// `capabilities_fingerprint` と同じ長さ接頭辞エンコード + SHA-256。
    /// **ユーザーが入力する `command` は含めない** — パスの変更は再承認ではなく
    /// 「設定変更 → 停止 → 次の ensure-started で新パスを起動」として扱うため
    /// (設計書「付与(grants)」の節を参照)。
    pub fn sidecar_fingerprint(&self, name: &str) -> Option<String> {
        let sidecar = self.sidecar(name)?;

        let mut canonical = encode_field("sidecar");
        canonical.push_str(&encode_field(&sidecar.name));
        canonical.push_str(&encode_field(&sidecar.reason));
        canonical.push_str(&encode_field(&sidecar.args.len().to_string()));
        for arg in &sidecar.args {
            canonical.push_str(&encode_field(arg));
        }
        canonical.push_str(&encode_field(&sidecar.port.to_string()));
        canonical.push_str(&encode_field(if sidecar.scalable { "1" } else { "0" }));

        Some(sha256_hex(&canonical))
    }

    pub fn filesystem_root(&self, name: &str) -> Option<&FilesystemRequest> {
        self.filesystem.iter().find(|r| r.name == name)
    }

    /// ファイルアクセス要求 1 件の安定フィンガープリント。
    /// `capabilities_fingerprint` と同じ長さ接頭辞エンコード + SHA-256。
    /// **ユーザーが選ぶ path は含めない**(パス変更は再承認を要さない)。
    pub fn filesystem_fingerprint(&self, name: &str) -> Option<String> {
        let request = self.filesystem_root(name)?;
        let mut canonical = encode_field("filesystem");
        canonical.push_str(&encode_field(&request.name));
        canonical.push_str(&encode_field(&request.reason));
        canonical.push_str(&encode_field(request.mode.as_str()));
        Some(sha256_hex(&canonical))
    }

    pub fn bus_request(&self, driver: &str) -> Option<&BusRequest> {
        self.bus.iter().find(|r| r.driver == driver)
    }

    /// バス接続 1 件の要求内容の安定フィンガープリント(grants の失効判定に使う)。
    /// `capabilities_fingerprint` と同じ長さ接頭辞エンコード + SHA-256。
    /// トピック順の違いは無視する(ソートしてから畳み込む)。
    pub fn bus_fingerprint(&self, driver: &str) -> Option<String> {
        let request = self.bus_request(driver)?;
        let mut publish = request.publish.clone();
        publish.sort();
        let mut subscribe = request.subscribe.clone();
        subscribe.sort();

        let mut canonical = encode_field("bus");
        canonical.push_str(&encode_field(&request.driver));
        canonical.push_str(&encode_field(&publish.len().to_string()));
        for topic in &publish {
            canonical.push_str(&encode_field(topic));
        }
        canonical.push_str(&encode_field(&subscribe.len().to_string()));
        for topic in &subscribe {
            canonical.push_str(&encode_field(topic));
        }
        canonical.push_str(&encode_field(&request.reason));
        Some(sha256_hex(&canonical))
    }

    pub fn dashboard_widget(&self, id: &str) -> Option<&DashboardWidget> {
        self.dashboard.iter().find(|w| w.id == id)
    }

    /// ダッシュボードウィジェット 1 件の宣言内容の安定フィンガープリント
    /// (grants の失効判定に使う)。宣言のどのフィールドが変わっても値が
    /// 変わる(`bus_fingerprint` と同じ流儀)。
    pub fn dashboard_fingerprint(&self, id: &str) -> Option<String> {
        let widget = self.dashboard_widget(id)?;
        let mut canonical = encode_field("dashboard");
        canonical.push_str(&encode_field(&widget.id));
        canonical.push_str(&encode_field(&widget.title));
        canonical.push_str(&encode_field(&widget.entry));
        canonical.push_str(&encode_field(widget.size.as_str()));
        Some(sha256_hex(&canonical))
    }
}

/// 可変長文字列フィールドを長さ接頭辞方式でエンコードする: `"{byte_len}:{content}"`。
///
/// 複数の可変長フィールドを区切り文字(`;` や `|` など)で単純結合すると、
/// フィールドの中身に区切り文字そのものが含まれる場合(例えば `reason` は
/// 検証されない自由記述フィールド)に異なる入力が同じ結合結果を生みうる
/// (例: `"a;b"` と `"a" + ";" + "b"` の衝突)。長さを前置しておけば、
/// `encode_field(f1) + encode_field(f2) + ... + encode_field(fn)` は
/// `(f1, f2, ..., fn)` に対して単射になる — 先頭から「長さを読む→その
/// バイト数だけ読む」を繰り返せば一意に読み戻せるため、内容にどんな文字列が
/// 含まれていても後続フィールドとの衝突が起こらない。
fn encode_field(s: &str) -> String {
    format!("{}:{}", s.len(), s)
}

/// SHA-256 の16進表現。`capabilities_fingerprint` はプラグイン作者自身が
/// 完全に制御できる入力を衝突困難性つきで比較する必要があるため、暗号学的
/// ハッシュ関数が必須(非暗号学的ハッシュだと誕生日衝突が現実的な計算量で
/// 作れてしまう)。
fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// `load_manifest` が返しうるエラー。
#[derive(Debug)]
pub enum ManifestError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    /// `id` がディレクトリ名と不一致。
    IdMismatch,
    /// `id` が `[a-z0-9-]+` にマッチしない。
    BadId,
    /// `entry` が指すファイルが存在しない。
    MissingEntry,
    /// `settings` 内で `key` が重複している。
    DuplicateKey,
    /// `capabilities` の内容が不正(host の形式・空リストなど)。
    BadCapability(String),
    /// `sidecar` の内容が不正(name の形式・重複・reason 空など)。
    BadSidecar(String),
    /// `filesystem` の内容が不正(name の形式・重複・reason 空など)。
    BadFilesystem(String),
    /// `bus` の内容が不正(driver の形式・重複・空の publish/subscribe・
    /// トピック名・reason 空など)。
    BadBus(String),
    /// `topics`(ドライバの `[[topics]]`)の内容が不正(名前の形式・重複など)。
    BadTopic(String),
    /// `dashboard` の内容が不正(id の形式・重複・title 空・entry の脱出など)。
    BadDashboard(String),
    /// `schedule` の `name` が不正(字種違反・重複)。`interval-seconds`/
    /// `cron` の排他違反・値の不正・cron 式のパース失敗は `Parse` として
    /// 現れる(`ScheduleRequest` の `Deserialize` 実装内で検出するため)。
    BadSchedule(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Io(e) => write!(f, "failed to read manifest.toml: {e}"),
            ManifestError::Parse(e) => write!(f, "failed to parse manifest.toml: {e}"),
            ManifestError::IdMismatch => {
                write!(f, "manifest id does not match plugin directory name")
            }
            ManifestError::BadId => write!(f, "manifest id must match [a-z0-9-]+"),
            ManifestError::MissingEntry => write!(f, "entry file does not exist"),
            ManifestError::DuplicateKey => write!(f, "duplicate settings key"),
            ManifestError::BadCapability(msg) => write!(f, "invalid capability request: {msg}"),
            ManifestError::BadSidecar(msg) => write!(f, "invalid sidecar request: {msg}"),
            ManifestError::BadFilesystem(msg) => write!(f, "invalid filesystem request: {msg}"),
            ManifestError::BadBus(msg) => write!(f, "invalid bus request: {msg}"),
            ManifestError::BadTopic(msg) => write!(f, "invalid topic: {msg}"),
            ManifestError::BadDashboard(msg) => write!(f, "invalid dashboard widget: {msg}"),
            ManifestError::BadSchedule(msg) => write!(f, "invalid schedule request: {msg}"),
        }
    }
}

impl std::error::Error for ManifestError {}

pub(crate) fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// capability の host エントリを検証する。
///
/// - `http://` または `https://` で始まること
/// - URL としてパース可能で、host を持つこと
/// - path・query・fragment を持たないこと(bare origin のみ)。ただし末尾の
///   `/` 一つだけの path (`https://example.com/`) は origin と等価なので許可する。
/// - userinfo(`user:pass@host` の形式)を含まないこと。人間がレビューする
///   capability 宣言に認証情報が紛れ込むのを防ぐ。
fn validate_host(host: &str) -> Result<(), String> {
    if !host.starts_with("http://") && !host.starts_with("https://") {
        return Err(format!("host must start with http:// or https://: {host}"));
    }

    let parsed =
        url::Url::parse(host).map_err(|e| format!("host is not a valid URL: {host} ({e})"))?;

    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(format!("host must have a non-empty hostname: {host}"));
    }

    if !matches!(parsed.path(), "" | "/") {
        return Err(format!("host must not contain a path: {host}"));
    }

    if parsed.query().is_some() {
        return Err(format!("host must not contain a query: {host}"));
    }

    if parsed.fragment().is_some() {
        return Err(format!("host must not contain a fragment: {host}"));
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!("host must not contain userinfo: {host}"));
    }

    Ok(())
}

/// `reason`(および必要なら `host`)に、ユーザーには見えない/見分けが
/// つかない文字が紛れ込んでいないか検証する。
///
/// `capabilities_fingerprint` が承認対象のテキストをそのままハッシュに
/// 含める以上、承認画面に描画されるテキストとフィンガープリントの元に
/// なるテキストが byte-for-byte 一致していなければならない。制御文字
/// (改行・タブなど)や幅を持たない文字(zero-width space/joiner, BOM)は
/// UI 上ただの空白や何もない箇所に見えるため、これらを承認前に拒否して
/// 「見た目には同じだが実際には異なる文字列」を承認させられる余地を潰す。
fn reject_invisible_chars(field: &str, s: &str) -> Result<(), String> {
    for c in s.chars() {
        if c.is_control() {
            return Err(format!(
                "{field} must not contain control characters: {s:?}"
            ));
        }
        // U+200B..U+200D (zero width space/non-joiner/joiner), U+FEFF (BOM /
        // zero width no-break space), U+2060 (word joiner). Not an
        // exhaustive Cf-category sweep, but covers the characters that are
        // actually invisible in a rendered UI and cheap to type.
        if matches!(c, '\u{200B}'..='\u{200D}' | '\u{FEFF}' | '\u{2060}') {
            return Err(format!(
                "{field} must not contain zero-width characters: {s:?}"
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_capabilities(
    capabilities: &mut [CapabilityRequest],
) -> Result<(), ManifestError> {
    for capability in capabilities.iter_mut() {
        match capability {
            CapabilityRequest::Http { hosts, reason } => {
                if hosts.is_empty() {
                    return Err(ManifestError::BadCapability(
                        "http capability requires at least one host".to_string(),
                    ));
                }

                // Normalize before validating/hashing so the text a user
                // approves in the UI is byte-identical to the text that
                // gates the grant fingerprint (see `reject_invisible_chars`).
                let trimmed = reason.trim().to_string();
                if trimmed.is_empty() {
                    return Err(ManifestError::BadCapability(
                        "http capability requires a non-empty reason".to_string(),
                    ));
                }
                reject_invisible_chars("reason", &trimmed).map_err(ManifestError::BadCapability)?;
                *reason = trimmed;

                for host in hosts.iter() {
                    reject_invisible_chars("host", host).map_err(ManifestError::BadCapability)?;
                    validate_host(host).map_err(ManifestError::BadCapability)?;
                }
            }
        }
    }
    Ok(())
}

/// `[[sidecar]]` を検証・正規化する。`reason` は `capabilities` と同じく
/// trim して不可視文字を拒否する(承認画面に出る文字列とフィンガープリントの
/// 元になる文字列を byte 単位で一致させるため)。
pub(crate) fn validate_sidecars(sidecars: &mut [SidecarRequest]) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for sidecar in sidecars.iter_mut() {
        if !is_valid_id(&sidecar.name) {
            return Err(ManifestError::BadSidecar(format!(
                "sidecar name must match [a-z0-9-]+: {}",
                sidecar.name
            )));
        }
        if !seen.insert(sidecar.name.clone()) {
            return Err(ManifestError::BadSidecar(format!(
                "duplicate sidecar name: {}",
                sidecar.name
            )));
        }
        if sidecar.port == 0 {
            return Err(ManifestError::BadSidecar(
                "sidecar port must be 1..=65535".to_string(),
            ));
        }

        let trimmed = sidecar.reason.trim().to_string();
        if trimmed.is_empty() {
            return Err(ManifestError::BadSidecar(
                "sidecar requires a non-empty reason".to_string(),
            ));
        }
        reject_invisible_chars("reason", &trimmed).map_err(ManifestError::BadSidecar)?;
        sidecar.reason = trimmed;

        for arg in &sidecar.args {
            reject_invisible_chars("args", arg).map_err(ManifestError::BadSidecar)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_filesystem(requests: &mut [FilesystemRequest]) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for request in requests.iter_mut() {
        if !is_valid_id(&request.name) {
            return Err(ManifestError::BadFilesystem(format!(
                "filesystem name must match [a-z0-9-]+: {}",
                request.name
            )));
        }
        if !seen.insert(request.name.clone()) {
            return Err(ManifestError::BadFilesystem(format!(
                "duplicate filesystem name: {}",
                request.name
            )));
        }

        let trimmed = request.reason.trim().to_string();
        if trimmed.is_empty() {
            return Err(ManifestError::BadFilesystem(
                "filesystem request requires a non-empty reason".to_string(),
            ));
        }
        reject_invisible_chars("reason", &trimmed).map_err(ManifestError::BadFilesystem)?;
        request.reason = trimmed;
    }
    Ok(())
}

/// `[[bus]]` を検証・正規化する。`reason` は `capabilities` と同じく trim
/// して不可視文字を拒否する(承認画面に出る文字列とフィンガープリントの
/// 元になる文字列を byte 単位で一致させるため)。
fn validate_bus(requests: &mut [BusRequest]) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for request in requests.iter_mut() {
        if !is_valid_id(&request.driver) {
            return Err(ManifestError::BadBus(format!(
                "bus driver must match [a-z0-9-]+: {}",
                request.driver
            )));
        }
        if !seen.insert(request.driver.clone()) {
            return Err(ManifestError::BadBus(format!(
                "duplicate bus driver: {}",
                request.driver
            )));
        }
        if request.publish.is_empty() && request.subscribe.is_empty() {
            return Err(ManifestError::BadBus(format!(
                "bus request must declare at least one publish or subscribe topic: {}",
                request.driver
            )));
        }

        for topic in request.publish.iter().chain(request.subscribe.iter()) {
            edlr_driver_channel::topic::validate_name(topic).map_err(ManifestError::BadBus)?;
        }
        // `[[topics]]`(`crate::driver::manifest::validate_topics`)と同じ
        // 規律: `publish`/`subscribe` それぞれの中で同じトピック名が重複して
        // 宣言されているのを許すと、`subscribe = ["a", "a"]` が実際には 2 件の
        // 購読を作ってしまい、`emit` のたびに `on-event` が 2 回呼ばれ、
        // プラグインの作業キューを無駄に 2 スロット消費する(Minor: 最終
        // レビューで見つかった取りこぼし)。
        for (field, topics) in [
            ("publish", &request.publish),
            ("subscribe", &request.subscribe),
        ] {
            let mut seen_topics = HashSet::new();
            for topic in topics {
                if !seen_topics.insert(topic.as_str()) {
                    return Err(ManifestError::BadBus(format!(
                        "duplicate {field} topic for driver {}: {topic}",
                        request.driver
                    )));
                }
            }
        }

        let trimmed = request.reason.trim().to_string();
        if trimmed.is_empty() {
            return Err(ManifestError::BadBus(
                "bus request requires a non-empty reason".to_string(),
            ));
        }
        reject_invisible_chars("reason", &trimmed).map_err(ManifestError::BadBus)?;
        request.reason = trimmed;
    }
    Ok(())
}

pub(crate) fn validate_dashboard(widgets: &mut [DashboardWidget]) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for widget in widgets.iter_mut() {
        if !is_valid_id(&widget.id) {
            return Err(ManifestError::BadDashboard(format!(
                "dashboard id must match [a-z0-9-]+: {}",
                widget.id
            )));
        }
        if !seen.insert(widget.id.clone()) {
            return Err(ManifestError::BadDashboard(format!(
                "duplicate dashboard id: {}",
                widget.id
            )));
        }
        let title = widget.title.trim().to_string();
        if title.is_empty() {
            return Err(ManifestError::BadDashboard(
                "dashboard widget requires a non-empty title".to_string(),
            ));
        }
        reject_invisible_chars("title", &title).map_err(ManifestError::BadDashboard)?;
        widget.title = title;
        validate_widget_entry(&widget.entry)?;
    }
    Ok(())
}

/// 標準 5 欄の cron 式(分 時 日 月 曜日)を、`cron` クレートが要求する
/// 7 欄形式(秒 分 時 日 月 曜日 年)に正規化する。秒は常に `0`、年は常に `*`
/// を補う。
pub(crate) fn normalize_cron(expr: &str) -> String {
    format!("0 {expr} *")
}

/// `[[schedule]]` の name を検証する(字種・一意性)。
///
/// `interval-seconds`/`cron` の排他性・値の妥当性(0 より大きいこと、
/// cron 式が `cron::Schedule::from_str` でパース可能であること)は
/// `ScheduleRequest` の `Deserialize` 実装がテーブル単体の情報だけで
/// その場で検証済み。ここでは一覧全体の情報が要る一意性チェックのみ行う。
fn validate_schedules(schedules: &[ScheduleRequest]) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for schedule in schedules {
        if !is_valid_id(&schedule.name) {
            return Err(ManifestError::BadSchedule(format!(
                "schedule name must match [a-z0-9-]+: {}",
                schedule.name
            )));
        }
        if !seen.insert(schedule.name.clone()) {
            return Err(ManifestError::BadSchedule(format!(
                "duplicate schedule name: {}",
                schedule.name
            )));
        }
    }
    Ok(())
}

/// `entry` がプラグインディレクトリ内に収まる相対パスであることの検証。
/// 絶対パス・`..`・空・ルート/プレフィックス成分を拒否する(配信ハンドラの
/// トラバーサル防御の一段目。二段目は `Registry::dashboard_asset_path`)。
fn validate_widget_entry(entry: &str) -> Result<(), ManifestError> {
    use std::path::Component;
    let path = Path::new(entry);
    if entry.is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(ManifestError::BadDashboard(format!(
            "dashboard entry must be a relative path inside the plugin directory: {entry}"
        )));
    }
    Ok(())
}

/// `Manifest` が知っているトップレベルキー(serde の `rename` 後の名前)。
/// `unknown_top_level_keys` の既知リストとして使う。
pub(crate) const MANIFEST_TOP_LEVEL_KEYS: &[&str] = &[
    "id",
    "name",
    "version",
    "description",
    "entry",
    "events",
    "settings",
    "capabilities",
    "sidecar",
    "filesystem",
    "bus",
    "dashboard",
    "schedule",
];

/// マニフェスト本文のトップレベルにある、`known` に無いキーの一覧を返す。
///
/// トップレベルの構造体には `deny_unknown_fields` を付けていない — 付けると
/// 新しいフィールドを増やしたときに古い edlr が新しいマニフェストを読めなく
/// なるため。代わりに、綴り違いや古い/新しいキーを warn で報せる
/// (issue manifest-rjoa の提案 2)。
///
/// パースできない本文は空を返す。呼び出し側は `toml::from_str` の結果として
/// 既に `ManifestError::Parse` を扱っており、ここで別のエラーを重ねる意味が
/// ないため。
pub(crate) fn unknown_top_level_keys(content: &str, known: &[&str]) -> Vec<String> {
    let Ok(table) = content.parse::<toml::Table>() else {
        return Vec::new();
    };
    table
        .keys()
        .filter(|key| !known.contains(&key.as_str()))
        .cloned()
        .collect()
}

/// 知らないトップレベルキーを warn ログに出す。
pub(crate) fn warn_unknown_top_level_keys(file: &str, id: &str, unknown: &[String]) {
    for key in unknown {
        tracing::warn!(
            manifest = file,
            id,
            key = key.as_str(),
            "unknown top-level key in {file} is ignored — 綴り違い、または \
             テーブルヘッダ([[settings]] など)より後ろに書いてしまった \
             トップレベルキーの可能性があります"
        );
    }
}

/// `dir/manifest.toml` を読み込み、検証して返す。
///
/// 検証エラーは `Err` として返す(panic しない)。呼び出し側は当該プラグインのみ
/// ロードスキップして warn するなど、エラーを握りつぶさずに扱うこと。
pub fn load_manifest(dir: &Path) -> Result<Manifest, ManifestError> {
    let manifest_path = dir.join("manifest.toml");
    let content = fs::read_to_string(&manifest_path).map_err(ManifestError::Io)?;
    let mut manifest: Manifest = toml::from_str(&content).map_err(ManifestError::Parse)?;

    if !is_valid_id(&manifest.id) {
        return Err(ManifestError::BadId);
    }

    let dir_name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if manifest.id != dir_name {
        return Err(ManifestError::IdMismatch);
    }

    let entry_path = dir.join(&manifest.entry);
    if !entry_path.is_file() {
        return Err(ManifestError::MissingEntry);
    }

    let mut seen = HashSet::new();
    for setting in &manifest.settings {
        if !seen.insert(setting.key()) {
            return Err(ManifestError::DuplicateKey);
        }
    }

    validate_capabilities(&mut manifest.capabilities)?;
    validate_sidecars(&mut manifest.sidecars)?;
    validate_filesystem(&mut manifest.filesystem)?;
    validate_bus(&mut manifest.bus)?;
    validate_dashboard(&mut manifest.dashboard)?;
    validate_schedules(&manifest.schedules)?;

    warn_unknown_top_level_keys(
        "manifest.toml",
        &manifest.id,
        &unknown_top_level_keys(&content, MANIFEST_TOP_LEVEL_KEYS),
    );

    // 宣言と実際の読み取り結果が一致しているかを目視できるようにする
    // (issue manifest-rjoa の提案 3)。`settings=0` のような行が出ていれば、
    // 「宣言したはずの設定が丸ごと消えている」ことにログだけで気づける。
    tracing::info!(
        id = manifest.id.as_str(),
        events = manifest.events.len(),
        settings = manifest.settings.len(),
        capabilities = manifest.capabilities.len(),
        sidecars = manifest.sidecars.len(),
        filesystem = manifest.filesystem.len(),
        bus = manifest.bus.len(),
        dashboard = manifest.dashboard.len(),
        schedules = manifest.schedules.len(),
        "plugin manifest loaded"
    );

    Ok(manifest)
}

/// `events` フィルタが `event` にマッチするかどうか。
///
/// - `"*"` は全ての journal イベントにマッチ(status には false)
/// - `"status"` は Status イベントにのみマッチ
/// - それ以外は journal イベント名の完全一致
/// - 空リストは常に false
pub fn matches_event(events: &[String], event: &crate::event::Event) -> bool {
    match event {
        crate::event::Event::Journal { event: name, .. } => {
            events.iter().any(|e| e == "*" || e == name)
        }
        crate::event::Event::Status { .. } => events.iter().any(|e| e == "status"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use std::fs;

    fn write_manifest(dir: &Path, contents: &str) {
        fs::write(dir.join("manifest.toml"), contents).unwrap();
    }

    fn write_entry(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"\0asm").unwrap();
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
                options: vec!["a".into(), "b".into()],
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

        let manifest =
            load_manifest(&plugin_dir).expect("manifest with integer default should parse");

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

        let err = load_manifest(&plugin_dir)
            .expect_err("catch-up with interval-seconds should be rejected");
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
                options: vec!["a".into()],
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

        let err = load_manifest(&plugin_dir)
            .expect_err("zero-width character in reason must be rejected");
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

        let err =
            load_manifest(&plugin_dir).expect_err("control character in reason must be rejected");
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

        let manifest =
            load_manifest(&plugin_dir).expect("bare trailing slash host should be accepted");
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
            parse_fs_manifest(
                "[[filesystem]]\nname = \"Exports\"\nreason = \"r\"\nmode = \"read\"\n"
            )
            .expect_err("uppercase name"),
            ManifestError::BadFilesystem(_)
        ));
    }

    #[test]
    fn filesystem_fingerprint_is_stable_and_changes_with_the_request() {
        let manifest = parse_fs_manifest(
            "[[filesystem]]\nname = \"exports\"\nreason = \"r\"\nmode = \"read\"\n",
        )
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
    /// subscriptions (`crate::driver::manifest::validate_topics` already
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
}
