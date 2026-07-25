use serde_json::Value;

/// カーネルが配信するイベント。生 JSON を保持し、型付けは下流に委ねる。
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Journal {
        timestamp: String,
        event: String,
        raw: Value,
    },
    Status {
        raw: Value,
    },
}
