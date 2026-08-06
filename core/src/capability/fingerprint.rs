//! capability 要求のハッシュ材料の組み立て(安定フィンガープリント計算)。
//!
//! 該当 request を manifest から探す(`Manifest::sidecar` 等)のは呼び出し側
//! (`Manifest` のメソッド)の仕事。ここは値イン値アウトの計算だけを担う。

use super::request::{
    BusRequest, CapabilityRequest, DashboardWidget, FilesystemRequest, FilesystemTarget,
    SidecarRequest,
};

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
pub fn capabilities(requests: &[CapabilityRequest]) -> Option<String> {
    if requests.is_empty() {
        return None;
    }

    let mut canonical_requests: Vec<String> = requests
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

/// サイドカー 1 件の要求内容の安定フィンガープリント(grants の失効判定に使う)。
///
/// `capabilities` と同じ長さ接頭辞エンコード + SHA-256。
/// **ユーザーが入力する `command` は含めない** — パスの変更は再承認ではなく
/// 「設定変更 → 停止 → 次の ensure-started で新パスを起動」として扱うため
/// (設計書「付与(grants)」の節を参照)。
pub fn sidecar(request: &SidecarRequest) -> String {
    let mut canonical = encode_field("sidecar");
    canonical.push_str(&encode_field(&request.name));
    canonical.push_str(&encode_field(&request.reason));
    canonical.push_str(&encode_field(&request.args.len().to_string()));
    for arg in &request.args {
        canonical.push_str(&encode_field(arg));
    }
    canonical.push_str(&encode_field(&request.port.to_string()));
    canonical.push_str(&encode_field(if request.scalable { "1" } else { "0" }));

    sha256_hex(&canonical)
}

/// ファイルアクセス要求 1 件の安定フィンガープリント。
/// `capabilities` と同じ長さ接頭辞エンコード + SHA-256。
/// **ユーザーが選ぶ path は含めない**(パス変更は再承認を要さない)。
pub fn filesystem(request: &FilesystemRequest) -> String {
    let mut canonical = encode_field("filesystem");
    canonical.push_str(&encode_field(&request.name));
    canonical.push_str(&encode_field(&request.reason));
    canonical.push_str(&encode_field(request.mode.as_str()));
    // target は file のときだけ畳み込む。directory で無条件に足すと、この
    // フィールド導入前に承認された既存の grants が全プラグインで一斉に
    // 失効してしまう。4 フィールド形(旧 directory)と 5 フィールド形
    // (file)の衝突は起こらない: mode のエンコードは "4:read" か
    // "10:read-write" のどちらかで、"4:file" で終わる文字列を含み得ない。
    if request.target == FilesystemTarget::File {
        canonical.push_str(&encode_field(request.target.as_str()));
    }
    sha256_hex(&canonical)
}

/// バス接続 1 件の要求内容の安定フィンガープリント(grants の失効判定に使う)。
/// `capabilities` と同じ長さ接頭辞エンコード + SHA-256。
/// トピック順の違いは無視する(ソートしてから畳み込む)。
pub fn bus(request: &BusRequest) -> String {
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
    sha256_hex(&canonical)
}

/// ダッシュボードウィジェット 1 件の宣言内容の安定フィンガープリント
/// (grants の失効判定に使う)。宣言のどのフィールドが変わっても値が
/// 変わる(`bus` と同じ流儀)。
pub fn dashboard(widget: &DashboardWidget) -> String {
    let mut canonical = encode_field("dashboard");
    canonical.push_str(&encode_field(&widget.id));
    canonical.push_str(&encode_field(&widget.title));
    canonical.push_str(&encode_field(&widget.entry));
    canonical.push_str(&encode_field(widget.size.as_str()));
    sha256_hex(&canonical)
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

/// SHA-256 の16進表現。`capabilities` はプラグイン作者自身が
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
