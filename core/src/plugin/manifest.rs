use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::Path;

/// マニフェストの `[[settings]]` テーブル 1 件。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
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
}

impl SettingField {
    pub fn key(&self) -> &str {
        match self {
            SettingField::Boolean { key, .. } => key,
            SettingField::String { key, .. } => key,
            SettingField::Number { key, .. } => key,
            SettingField::Select { key, .. } => key,
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
        }
    }
}

/// プラグインが要求する capability(実行時に許可が必要な外部リソースアクセス)。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CapabilityRequest {
    Http { hosts: Vec<String>, reason: String },
}

/// プラグインが要求するサイドカープロセス 1 件。
///
/// **実行ファイルのパス(`command`)はここに書けない** — 必ずユーザーが
/// UI で入力する。承認画面に出る内容と実際に走るプログラムを、ユーザー自身の
/// 明示的な指定によって一致させるため。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
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
pub struct BusRequest {
    pub driver: String,
    #[serde(default)]
    pub publish: Vec<String>,
    #[serde(default)]
    pub subscribe: Vec<String>,
    pub reason: String,
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
}
