use std::net::SocketAddr;

use swarm::spawn::Spawned;

pub struct TestRuntime {
    pub spawned: Spawned,
}

impl TestRuntime {
    pub async fn spawn(
        env_filter: &str,
        otel_addr: Option<SocketAddr>,
        db_retention: Option<&str>,
        gc_interval: &str,
    ) -> Self {
        let otel_export = otel_addr
            .map(|addr| format!("+ s.telemetry.opentelemetry_export('http://{addr}')"))
            .unwrap_or_default();
        let db_retention = db_retention
            .map(|retention| format!("+ {{ telemetry+: {{ db_retention: '{retention}' }} }}"))
            .unwrap_or_default();

        let config = format!(
            r"
                local z = import 'zenoh.libsonnet';
                local s = import 'swarm.libsonnet';

                z.peer()
                + z.plugins.dev({{ db: {{ gc_interval: '{gc_interval}', offload_escalation_timeout: '100ms' }}, embedded_log: {{}} }})
                + s.telemetry.logs.env_filter('{env_filter}')
                + {{ zenoh+: {{ scouting+: {{ multicast+: {{ enabled: false }}, gossip+: {{ enabled: false }} }} }} }}
                {db_retention}
                {otel_export}
            "
        );

        let swarm = swarm::Swarm::parse(&config).expect("unable to parse swarm config");
        let spawned = swarm.wait_in_place().await.expect("unable to spawn swarm");

        Self { spawned }
    }
}
