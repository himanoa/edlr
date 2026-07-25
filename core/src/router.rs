use crate::event::Event;
use std::sync::Arc;
use tokio::sync::broadcast;

/// イベントを全購読者に配る pub/sub ルーター。
#[derive(Clone)]
pub struct Router {
    tx: broadcast::Sender<Arc<Event>>,
}

impl Router {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.tx.subscribe()
    }

    /// 購読者がいない場合の送信エラーは無視する。
    pub fn publish(&self, event: Event) {
        let _ = self.tx.send(Arc::new(event));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;

    #[tokio::test]
    async fn delivers_to_all_subscribers() {
        let router = Router::new(16);
        let mut a = router.subscribe();
        let mut b = router.subscribe();
        router.publish(Event::Status {
            raw: serde_json::json!({"Flags": 1}),
        });
        assert!(matches!(*a.recv().await.unwrap(), Event::Status { .. }));
        assert!(matches!(*b.recv().await.unwrap(), Event::Status { .. }));
    }

    #[test]
    fn publish_without_subscribers_does_not_panic() {
        let router = Router::new(16);
        router.publish(Event::Status {
            raw: serde_json::json!({}),
        });
    }
}
