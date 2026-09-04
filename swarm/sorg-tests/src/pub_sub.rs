//! Functionality to test component behavior related to subscribing to/publishing on topics

use std::time::Duration;

use zenoh::key_expr::OwnedKeyExpr;

use crate::{TestApp, WAIT_TIME};

impl TestApp {
    pub async fn subscribe_to_topic(&mut self, topic: OwnedKeyExpr) {
        let subscriber = self
            .session()
            .declare_subscriber(topic.clone())
            .await
            .unwrap();
        {
            let received_msgs = self.received_msgs.clone();
            tokio::spawn(async move {
                while let Ok(sample) = subscriber.recv_async().await {
                    let bytes = sample.payload().to_bytes();
                    received_msgs
                        .lock()
                        .await
                        .entry(topic.clone())
                        .or_default()
                        .push(bytes.to_vec());
                }
            });
        }
        tokio::time::sleep(WAIT_TIME).await;
    }

    /// Returns the messages received by the testapp on the provided topic (it should have subscribed to it earlier)
    /// this method drains the message buffer, so that a subsequent call will not find the same messages there
    pub async fn received_msgs_on_topic(&mut self, topic: OwnedKeyExpr) -> Vec<Vec<u8>> {
        let mut guard = self.received_msgs.lock().await;
        let msgs = guard.get_mut(&topic);
        let Some(msgs) = msgs else {
            return vec![];
        };
        std::mem::take(msgs)
    }

    pub async fn wait_for_messages(
        &mut self,
        topic: OwnedKeyExpr,
        expected: usize,
        timeout: Duration,
    ) -> Vec<Vec<u8>> {
        let start = tokio::time::Instant::now();
        loop {
            {
                let guard = self.received_msgs.lock().await;
                if guard.get(&topic).map_or(0, Vec::len) >= expected {
                    break;
                }
            }
            if start.elapsed() > timeout {
                let guard = self.received_msgs.lock().await;
                let actual = guard.get(&topic).map_or(0, Vec::len);
                panic!(
                    "timed out waiting for {expected} message(s) on {topic} \
                     (got {actual} after {timeout:?})"
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut guard = self.received_msgs.lock().await;
        std::mem::take(guard.get_mut(&topic).unwrap())
    }

    pub async fn publish_payload(&self, topic: OwnedKeyExpr, payload: Vec<u8>) {
        self.session().put(topic, payload).await.unwrap();
        tokio::time::sleep(WAIT_TIME).await;
    }
}
