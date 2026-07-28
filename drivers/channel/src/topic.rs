//! トピック名の検証と、ドライバが宣言するトピック仕様。

/// ドライバが `driver.toml` の `[[topics]]` で宣言する 1 件。
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TopicSpec {
    pub name: String,
    #[serde(default)]
    pub retain: bool,
    #[serde(default)]
    pub description: String,
}

/// トピック名は `[a-z0-9-]+` の 1..=64 バイト。
/// プラグイン ID / ドライバ ID と同じ字種に揃えてある(UI とログで同じ
/// 扱いができ、パス片やクエリに埋めても曖昧さが出ないため)。
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("topic name must not be empty".to_string());
    }
    if name.len() > 64 {
        return Err(format!("topic name must be at most 64 bytes: {name}"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(format!("topic name must match [a-z0-9-]+: {name}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_lowercase_kebab() {
        assert!(validate_name("current-system").is_ok());
    }

    #[test]
    fn rejects_empty() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn rejects_uppercase_and_symbols() {
        assert!(validate_name("Current").is_err());
        assert!(validate_name("a_b").is_err());
        assert!(validate_name("a/b").is_err());
    }

    #[test]
    fn rejects_over_64_bytes() {
        assert!(validate_name(&"a".repeat(65)).is_err());
        assert!(validate_name(&"a".repeat(64)).is_ok());
    }
}
