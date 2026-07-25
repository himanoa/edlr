use crate::event::Event;
use serde_json::Value;

/// Journal の JSON Lines 1 行をパースする。壊れた行や必須フィールド欠落は None。
pub fn parse_line(line: &str) -> Option<Event> {
    let raw: Value = serde_json::from_str(line.trim()).ok()?;
    let timestamp = raw.get("timestamp")?.as_str()?.to_string();
    let event = raw.get("event")?.as_str()?.to_string();
    Some(Event::Journal {
        timestamp,
        event,
        raw,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;

    #[test]
    fn parses_journal_line() {
        let line = r#"{"timestamp":"2026-07-25T12:00:00Z","event":"FSDJump","StarSystem":"Sol"}"#;
        let Some(Event::Journal {
            timestamp,
            event,
            raw,
        }) = parse_line(line)
        else {
            panic!("expected Journal event");
        };
        assert_eq!(timestamp, "2026-07-25T12:00:00Z");
        assert_eq!(event, "FSDJump");
        assert_eq!(raw["StarSystem"], "Sol");
    }

    #[test]
    fn rejects_broken_or_incomplete_lines() {
        assert_eq!(parse_line("{not json"), None);
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line(r#"{"timestamp":"t"}"#), None); // event 欠落
        assert_eq!(parse_line(r#"{"event":"e"}"#), None); // timestamp 欠落
    }
}
