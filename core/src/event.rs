use serde_json::Value;

/// カーネルが配信するイベント。生 JSON を保持し、型付けは下流に委ねる。
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Journal {
        timestamp: String,
        event: String,
        raw: Value,
        /// デーモンが動き出す前に既に Journal へ書かれていたイベント。
        /// 通知・読み上げ系のプラグインはこれを無視し、アップローダ・集計系は
        /// 処理する、という使い分けを想定している。
        replay: bool,
    },
    Status {
        raw: Value,
    },
}
