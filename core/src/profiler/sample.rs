use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Subject {
    Plugin,
    Driver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CallKind {
    OnEvent,
    OnMessage,
    OnSchedule,
    OnJobComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    Error,
    Timeout,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallSample {
    pub ts: f64,
    pub subject: Subject,
    pub id: String,
    pub call: CallKind,
    pub detail: String,
    pub duration_us: u64,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Serialize)]
pub struct GaugeSample {
    pub ts: f64,
    pub subject: Subject,
    pub id: String,
    pub queue_len: usize,
    pub dropped_events: u64,
    pub dropped_bus: u64,
    pub memory_bytes: u64,
}

#[derive(Debug, Clone)]
pub enum Sample {
    Call(CallSample),
    Gauge(GaugeSample),
    /// sink 満杯で捨てた計測サンプル数(累計)。毎秒 1 行。
    SinkLost { ts: f64, lost: u64 },
}

impl Sample {
    /// JSONL 1 行(改行なし)。シリアライズ失敗はあり得ない構造だが、
    /// 万一に備えて空オブジェクトへフォールバック。
    pub fn to_jsonl_line(&self) -> String {
        match self {
            Sample::Call(c) => serde_json::to_string(c),
            Sample::Gauge(g) => serde_json::to_string(g),
            Sample::SinkLost { ts, lost } => serde_json::to_string(&serde_json::json!({
                "ts": ts, "subject": "profiler", "id": "sink", "lost": lost,
            })),
        }
        .unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_sample_serializes_to_a_flat_jsonl_line() {
        let s = Sample::Call(CallSample {
            ts: 1755500000.123,
            subject: Subject::Plugin,
            id: "inara-uploader".into(),
            call: CallKind::OnEvent,
            detail: "FSDJump".into(),
            duration_us: 1800,
            outcome: Outcome::Ok,
        });
        let line = s.to_jsonl_line();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["subject"], "plugin");
        assert_eq!(v["call"], "on-event");
        assert_eq!(v["outcome"], "ok");
        assert_eq!(v["duration_us"], 1800);
        assert!(!line.contains('\n'));
    }

    #[test]
    fn gauge_and_sink_lost_serialize_with_their_own_shapes() {
        let g = Sample::Gauge(GaugeSample {
            ts: 1755500000.0,
            subject: Subject::Driver,
            id: "ed-state".into(),
            queue_len: 3,
            dropped_events: 0,
            dropped_bus: 1,
            memory_bytes: 4194304,
        });
        let v: serde_json::Value = serde_json::from_str(&g.to_jsonl_line()).unwrap();
        assert_eq!(v["subject"], "driver");
        assert_eq!(v["queue_len"], 3);

        let l = Sample::SinkLost { ts: 1.0, lost: 5 };
        let v: serde_json::Value = serde_json::from_str(&l.to_jsonl_line()).unwrap();
        assert_eq!(v["subject"], "profiler");
        assert_eq!(v["id"], "sink");
        assert_eq!(v["lost"], 5);
    }
}
