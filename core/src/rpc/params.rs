/// `params` から `key` の文字列値を取り出す。無い・文字列でない場合は
/// `Err`(RPC 層の流儀どおり panic しない)。
pub fn param_str<'a>(params: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("params.{key} must be a string"))
}
