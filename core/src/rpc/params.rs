/// `params` から `key` の文字列値を取り出す。無い・文字列でない場合は
/// `Err`(RPC 層の流儀どおり panic しない)。
pub fn param_str<'a>(params: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("params.{key} must be a string"))
}

/// `params` から `key` の bool 値を取り出す。無い・bool でない場合は `Err`。
pub fn param_bool(params: &serde_json::Value, key: &str) -> Result<bool, String> {
    params
        .get(key)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| format!("params.{key} must be a bool"))
}

/// `params` から `key` のオブジェクト値を取り出す。無い・オブジェクトでない場合は `Err`。
pub fn param_object<'a>(
    params: &'a serde_json::Value,
    key: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, String> {
    params
        .get(key)
        .and_then(|v| v.as_object())
        .ok_or_else(|| format!("params.{key} must be an object"))
}
