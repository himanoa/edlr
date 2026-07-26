//! `driver-http` の許可判定。プラグインごとの許可ホスト一覧(`granted_hosts`、
//! `https://api.example.com` のような origin 文字列のリスト)に対して、実際に
//! 呼び出そうとしている URL が許可されているかを判定する。
//!
//! 判定はスキーム + ホスト + ポートの完全一致。ポートを省略した場合はスキーム
//! の既定ポート(`http`=80、`https`=443)に正規化してから比較する。ホストの
//! 大文字小文字は無視する。サブドメインのワイルドカードは無い
//! (`api.example.com` の許可は `x.api.example.com` を許可しない)。パス・
//! クエリ・フラグメントは判定に使わない。

/// スキーム + 大文字小文字正規化済みホスト + 既定ポート正規化済みポートの組。
#[derive(Debug, PartialEq, Eq)]
struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    }
}

/// `url` をパースし、`http`/`https` の origin に正規化する。
///
/// 非 http(s) スキーム・パース失敗・ポート不明(既定ポートを持たないスキームで
/// 明示ポートも無い)の場合は `None`。
fn parse_origin(url: &str) -> Option<Origin> {
    let parsed = url::Url::parse(url).ok()?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_lowercase();
    let port = parsed.port().or_else(|| default_port(scheme))?;

    Some(Origin {
        scheme: scheme.to_string(),
        host,
        port,
    })
}

/// `granted_hosts` に対して `url` を判定する。許可されていれば `Ok(())`。
///
/// `granted_hosts` の各要素はマニフェストで検証済みの origin 文字列
/// (`https://host` または `https://host:port`)を想定するが、ここでも
/// 独立にパースするため不正な要素があっても panic しない(単に一致しない
/// ものとして扱われる)。
pub fn check_url(granted_hosts: &[String], url: &str) -> Result<(), String> {
    let requested =
        parse_origin(url).ok_or_else(|| format!("unparseable or non-http(s) url: {url}"))?;

    let allowed = granted_hosts
        .iter()
        .filter_map(|host| parse_origin(host))
        .any(|origin| origin == requested);

    if allowed {
        Ok(())
    } else {
        Err(format!(
            "url not in granted hosts allowlist: {}://{}:{}",
            requested.scheme, requested.host, requested.port
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn exact_match_is_allowed() {
        let granted = hosts(&["https://api.example.com"]);
        assert!(check_url(&granted, "https://api.example.com/v1/ping").is_ok());
    }

    #[test]
    fn scheme_mismatch_is_rejected() {
        let granted = hosts(&["https://api.example.com"]);
        assert!(check_url(&granted, "http://api.example.com").is_err());
    }

    #[test]
    fn port_mismatch_is_rejected() {
        let granted = hosts(&["https://api.example.com"]);
        assert!(check_url(&granted, "https://api.example.com:8443").is_err());
    }

    #[test]
    fn explicit_default_port_matches_implicit_form() {
        let granted = hosts(&["https://a.example.com"]);
        assert!(check_url(&granted, "https://a.example.com:443/x").is_ok());

        let granted = hosts(&["http://b.example.com:80"]);
        assert!(check_url(&granted, "http://b.example.com/x").is_ok());
    }

    #[test]
    fn host_case_is_ignored() {
        let granted = hosts(&["https://api.example.com"]);
        assert!(check_url(&granted, "https://API.Example.COM/ping").is_ok());
    }

    #[test]
    fn subdomains_are_not_wildcarded() {
        let granted = hosts(&["https://api.example.com"]);
        assert!(check_url(&granted, "https://x.api.example.com").is_err());
    }

    #[test]
    fn path_and_query_are_ignored_when_host_matches() {
        let granted = hosts(&["https://api.example.com"]);
        assert!(check_url(&granted, "https://api.example.com/v1/thing?x=1&y=2").is_ok());
    }

    #[test]
    fn unparseable_url_is_rejected() {
        let granted = hosts(&["https://api.example.com"]);
        assert!(check_url(&granted, "not a url at all").is_err());
    }

    #[test]
    fn non_http_scheme_is_rejected() {
        let granted = hosts(&["https://api.example.com"]);
        assert!(check_url(&granted, "ftp://api.example.com/file").is_err());
        assert!(check_url(&granted, "file:///etc/passwd").is_err());
    }

    #[test]
    fn empty_granted_list_rejects_everything() {
        let granted: Vec<String> = vec![];
        assert!(check_url(&granted, "https://api.example.com").is_err());
    }
}
