use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::Path;

/// `select` の候補 1 件。
///
/// TOML でも JSON でも **`"foo"` と `{ value = "...", label = "..." }` の両方**を
/// 受ける。前者は `value` と `label` が同じ値になる。候補がただの文字列でしか
/// ない場合(大半)にドライバ側もマニフェスト側も冗長に書かずに済ませつつ、
/// 表示名と保存値を分けたい場合(話者一覧なら「アメノちゃん/ギャル」を見せて
/// `uuid:styleId` を保存する)に対応するため。
///
/// シリアライズは常に `{ value, label }` の形で行う -- UI が 2 つの形を
/// 場合分けせずに済むように。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

/// 表示名と保存値が同じ候補を短く作る。TOML の `"foo"` 形式と同じ意味。
impl From<&str> for SelectOption {
    fn from(s: &str) -> Self {
        SelectOption {
            value: s.to_string(),
            label: s.to_string(),
        }
    }
}

/// `SelectOption` の生の serde 表現(文字列形式と object 形式の両受け)。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLabeledOption {
    value: String,
    label: String,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum RawSelectOption {
    Plain(String),
    Labeled(RawLabeledOption),
}

impl<'de> serde::Deserialize<'de> for SelectOption {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(match RawSelectOption::deserialize(deserializer)? {
            RawSelectOption::Plain(s) => SelectOption {
                value: s.clone(),
                label: s,
            },
            RawSelectOption::Labeled(o) => SelectOption {
                value: o.value,
                label: o.label,
            },
        })
    }
}

/// `select` の候補源として指すドライバの retain トピック。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct OptionsFrom {
    pub driver: String,
    pub topic: String,
}

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
    /// ドロップダウン。候補は **マニフェストに直接書く(`options`)か、ドライバの
    /// retain トピックから引く(`options-from`)かのどちらか一方**で、両方指定・
    /// 両方省略はいずれも `validate_settings` が弾く。
    ///
    /// `options-from` は「候補がインストール環境で決まる」設定のためにある
    /// (COEIROINK の話者一覧のように、何が選べるかは動かしてみないと分から
    /// ない)。解決は RPC 応答を組み立てる時点で行われ、`options` に埋められる
    /// (`crate::plugin::select_options::resolve`)。解決できなければ `None` の
    /// まま返り、UI が「候補を取得できません」を出す。
    Select {
        key: String,
        label: String,
        default: String,
        /// 静的な候補、または `options-from` から解決済みの候補。
        ///
        /// **`skip_serializing_if` を付けていない**のは意図的で、未解決を
        /// `null` として RPC に出すため。フィールドごと消すと、UI 側で
        /// 「未解決」と「そもそも select ではない」が同じ `undefined` になる。
        #[serde(default)]
        options: Option<Vec<SelectOption>>,
        /// 候補源となるドライバの retain トピック。
        ///
        /// TOML では `options-from`、RPC 応答では `optionsFrom`
        /// (他の RPC フィールドが camelCase なのに合わせる)。
        #[serde(
            rename = "optionsFrom",
            alias = "options-from",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        options_from: Option<OptionsFrom>,
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

// Phase 1 で capability/request.rs へ移動した型の旧パス互換(削除は Phase 6)。
pub use crate::capability::request::{
    BusRequest, CapabilityRequest, DashboardWidget, FilesystemMode, FilesystemRequest,
    SidecarRequest, WidgetSize,
};

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
        crate::capability::fingerprint::capabilities(&self.capabilities)
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
        self.sidecar(name)
            .map(crate::capability::fingerprint::sidecar)
    }

    pub fn filesystem_root(&self, name: &str) -> Option<&FilesystemRequest> {
        self.filesystem.iter().find(|r| r.name == name)
    }

    /// ファイルアクセス要求 1 件の安定フィンガープリント。
    /// `capabilities_fingerprint` と同じ長さ接頭辞エンコード + SHA-256。
    /// **ユーザーが選ぶ path は含めない**(パス変更は再承認を要さない)。
    pub fn filesystem_fingerprint(&self, name: &str) -> Option<String> {
        self.filesystem_root(name)
            .map(crate::capability::fingerprint::filesystem)
    }

    pub fn bus_request(&self, driver: &str) -> Option<&BusRequest> {
        self.bus.iter().find(|r| r.driver == driver)
    }

    /// バス接続 1 件の要求内容の安定フィンガープリント(grants の失効判定に使う)。
    /// `capabilities_fingerprint` と同じ長さ接頭辞エンコード + SHA-256。
    /// トピック順の違いは無視する(ソートしてから畳み込む)。
    pub fn bus_fingerprint(&self, driver: &str) -> Option<String> {
        self.bus_request(driver)
            .map(crate::capability::fingerprint::bus)
    }

    pub fn dashboard_widget(&self, id: &str) -> Option<&DashboardWidget> {
        self.dashboard.iter().find(|w| w.id == id)
    }

    /// ダッシュボードウィジェット 1 件の宣言内容の安定フィンガープリント
    /// (grants の失効判定に使う)。宣言のどのフィールドが変わっても値が
    /// 変わる(`bus_fingerprint` と同じ流儀)。
    pub fn dashboard_fingerprint(&self, id: &str) -> Option<String> {
        self.dashboard_widget(id)
            .map(crate::capability::fingerprint::dashboard)
    }
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
    /// `id` がホスト予約語(`edlr_driver_channel::HOST_SENDER` = "host")。
    /// `host` は字種 `[a-z0-9-]+` として合法だが、ホストが合成する
    /// `sidecar-ready` メッセージの `from` に使うため、なりすまし余地を
    /// 塞ぐ目的で予約する(設計書 sidecar-ready 参照)。
    ReservedId,
    /// `entry` が指すファイルが存在しない。
    MissingEntry,
    /// `settings` 内で `key` が重複している。
    DuplicateKey,
    /// `settings` の内容が不正(`select` の候補指定の排他違反・空の
    /// `options`・`options-from` の字種違反など)。
    BadSetting(String),
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
            ManifestError::ReservedId => write!(
                f,
                "manifest id \"{}\" is reserved for host-synthesized messages",
                edlr_driver_channel::HOST_SENDER
            ),
            ManifestError::MissingEntry => write!(f, "entry file does not exist"),
            ManifestError::DuplicateKey => write!(f, "duplicate settings key"),
            ManifestError::BadSetting(msg) => write!(f, "invalid setting: {msg}"),
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

/// `[[settings]]` を検証する。`key` の一意性と、`select` の候補指定を見る。
///
/// プラグインとドライバの両方のローダから呼ばれる(`[[settings]]` の書式は
/// 両者で共通のため)。
pub(crate) fn validate_settings(settings: &[SettingField]) -> Result<(), ManifestError> {
    let mut seen = HashSet::new();
    for setting in settings {
        if !seen.insert(setting.key()) {
            return Err(ManifestError::DuplicateKey);
        }

        let SettingField::Select {
            key,
            options,
            options_from,
            ..
        } = setting
        else {
            continue;
        };

        match (options, options_from) {
            (Some(_), Some(_)) => {
                return Err(ManifestError::BadSetting(format!(
                    "select {key} must specify exactly one of options or options-from, not both"
                )));
            }
            (None, None) => {
                return Err(ManifestError::BadSetting(format!(
                    "select {key} must specify one of options or options-from"
                )));
            }
            // 空の候補は「選べる値が無いドロップダウン」でしかなく、書いた人の
            // 意図(まだ埋めていない)と結果(常に何も選べない)がずれる。
            (Some(list), None) if list.is_empty() => {
                return Err(ManifestError::BadSetting(format!(
                    "select {key} options must not be empty"
                )));
            }
            (Some(_), None) => {}
            (None, Some(from)) => {
                if !is_valid_id(&from.driver) {
                    return Err(ManifestError::BadSetting(format!(
                        "select {key} options-from driver must match [a-z0-9-]+"
                    )));
                }
                if !is_valid_id(&from.topic) {
                    return Err(ManifestError::BadSetting(format!(
                        "select {key} options-from topic must match [a-z0-9-]+"
                    )));
                }
            }
        }
    }
    Ok(())
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

    if manifest.id == edlr_driver_channel::HOST_SENDER {
        return Err(ManifestError::ReservedId);
    }

    let dir_name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if manifest.id != dir_name {
        return Err(ManifestError::IdMismatch);
    }

    let entry_path = dir.join(&manifest.entry);
    if !entry_path.is_file() {
        return Err(ManifestError::MissingEntry);
    }

    validate_settings(&manifest.settings)?;

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
mod tests;
