use std::collections::HashSet;
use std::path::Path;

use crate::capability::request::BusRequest;
use crate::manifest::{is_valid_id, ManifestError};

/// capability の host エントリを検証する。
///
/// - `http://` または `https://` で始まること
/// - URL としてパース可能で、host を持つこと
/// - path・query・fragment を持たないこと(bare origin のみ)。ただし末尾の
///   `/` 一つだけの path (`https://example.com/`) は origin と等価なので許可する。
/// - userinfo(`user:pass@host` の形式)を含まないこと。人間がレビューする
///   capability 宣言に認証情報が紛れ込むのを防ぐ。
pub fn validate_host(host: &str) -> Result<(), String> {
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
pub fn reject_invisible_chars(field: &str, s: &str) -> Result<(), String> {
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

/// `[[bus]]` を検証・正規化する。`reason` は `capabilities` と同じく trim
/// して不可視文字を拒否する(承認画面に出る文字列とフィンガープリントの
/// 元になる文字列を byte 単位で一致させるため)。
pub fn validate_bus(requests: &mut [BusRequest]) -> Result<(), ManifestError> {
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

/// `entry` がプラグインディレクトリ内に収まる相対パスであることの検証。
/// 絶対パス・`..`・空・ルート/プレフィックス成分を拒否する(配信ハンドラの
/// トラバーサル防御の一段目。二段目は `Registry::dashboard_asset_path`)。
pub fn validate_widget_entry(entry: &str) -> Result<(), ManifestError> {
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
