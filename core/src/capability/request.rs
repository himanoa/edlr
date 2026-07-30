//! プラグインが manifest で宣言する capability 要求の型。
//!
//! パースは manifest 側(TOML → これらの型)が担い、ここは型と
//! それ自身の小さな振る舞い(`as_str` 等)だけを持つ。

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
