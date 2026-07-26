use crate::event::Event;
use serde_json::Value;

/// Journal の JSON Lines 1 行をパースする。壊れた行や必須フィールド欠落は None。
///
/// `replay` はデーモンが動き出す前に既に Journal へ書かれていた行かどうかを
/// 呼び出し元(`monitor`)から受け取ってそのまま `Event::Journal` に載せる。
pub fn parse_line(line: &str, replay: bool) -> Option<Event> {
    let raw: Value = serde_json::from_str(line.trim()).ok()?;
    let timestamp = raw.get("timestamp")?.as_str()?.to_string();
    let event = raw.get("event")?.as_str()?.to_string();
    Some(Event::Journal {
        timestamp,
        event,
        raw,
        replay,
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
            replay,
        }) = parse_line(line, false)
        else {
            panic!("expected Journal event");
        };
        assert!(!replay);
        assert_eq!(timestamp, "2026-07-25T12:00:00Z");
        assert_eq!(event, "FSDJump");
        assert_eq!(raw["StarSystem"], "Sol");
    }

    #[test]
    fn rejects_broken_or_incomplete_lines() {
        assert_eq!(parse_line("{not json", false), None);
        assert_eq!(parse_line("", false), None);
        assert_eq!(parse_line(r#"{"timestamp":"t"}"#, false), None); // event 欠落
        assert_eq!(parse_line(r#"{"event":"e"}"#, false), None); // timestamp 欠落
    }

    #[test]
    fn carries_the_replay_flag_through() {
        let line = r#"{"timestamp":"2026-07-27T12:00:00Z","event":"FSDJump"}"#;
        let Some(Event::Journal { replay, .. }) = parse_line(line, true) else {
            panic!("expected Journal event");
        };
        assert!(replay);

        let Some(Event::Journal { replay, .. }) = parse_line(line, false) else {
            panic!("expected Journal event");
        };
        assert!(!replay);
    }
}
