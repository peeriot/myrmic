impl crate::test_runtime::TestRuntime {
    pub async fn shutdown(&self) {
        self.spawned.kill_async().await;
    }

    pub async fn await_data<T, R, FA, FT>(
        &self,
        table: &'static str,
        timeout: std::time::Duration,
        transform: FT,
        any_fn: FA,
    ) -> Vec<R>
    where
        T: serde::de::DeserializeOwned + serde::Serialize + std::fmt::Debug + Send + 'static,
        R: Send + 'static,
        FT: Fn(&T) -> R,
        FA: Fn(&R) -> bool,
    {
        let interval = std::time::Duration::from_millis(50);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let entries = self.query_table::<T>(table).await;
            let results: Vec<R> = entries.iter().map(&transform).collect();

            if results.iter().any(&any_fn) {
                return results;
            }

            if tokio::time::Instant::now() + interval > deadline {
                return vec![];
            }
            tokio::time::sleep(interval).await;
        }
    }

    pub async fn query_table<T>(&self, table: &'static str) -> Vec<T>
    where
        T: serde::de::DeserializeOwned + serde::Serialize + std::fmt::Debug + Send + 'static,
    {
        use db_client::v1::models::tb_list;
        use swarm_telemetry::db::ScopedEntry;

        let db_client = db_client::v1::Client::new(self.spawned.session());
        db_client
            .read_tx_in(swarm_telemetry::db::scope(), async move |client, tx| {
                client
                    .send(tb_list::Request {
                        id: tx,
                        op: tb_list::Op {
                            cursor: None,
                            scope: swarm_telemetry::db::scope(),
                            table: table.into(),
                            limit: None,
                            order: None,
                        },
                    })
                    .await
            })
            .await
            .unwrap()
            .map_err(|err| err.message.clone())
            .unwrap()
            .entities
            .iter()
            .filter_map(|(_, bytes)| serde_json::from_slice::<ScopedEntry<T>>(bytes).ok())
            .map(|e| e.data)
            .collect()
    }
}
