use std::ops::Deref;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bollard::{
    Docker,
    container::LogOutput,
    exec::{CreateExecOptions, StartExecResults},
    models::{EndpointSettings, NetworkConnectRequest},
    query_parameters::LogsOptionsBuilder,
};
use futures::{StreamExt, TryStreamExt};

use crate::docker::CommandOutput;

#[derive(Clone)]
/// a thin wrapper for interacting with a running docker container (exec, logs, network shaping)
pub struct ConnectedContainer {
    docker: Docker,
    container_id: String,
}

impl Deref for ConnectedContainer {
    type Target = Docker;

    fn deref(&self) -> &Self::Target {
        &self.docker
    }
}

impl ConnectedContainer {
    /// a [`ConnectedContainer`] is created by attaching to a `container_id`
    pub fn attach(docker: Docker, container_id: impl Into<String>) -> Self {
        Self {
            docker,
            container_id: container_id.into(),
        }
    }

    /// the docker container id
    pub fn id(&self) -> &str {
        &self.container_id
    }

    /// return the IP of this container in the specified network
    pub async fn container_ip(&self, network: &str) -> String {
        let inspect = self
            .docker
            .inspect_container(&self.container_id, None)
            .await
            .unwrap();
        inspect
            .network_settings
            .unwrap()
            .networks
            .unwrap()
            .get(network)
            .unwrap()
            .ip_address
            .clone()
            .unwrap()
    }

    /// tries to find the zenoh tcp port that is usually printed in logs
    pub async fn zenoh_tcp_port(&self) -> u16 {
        self.find_in_logs(
            "zenoh tcp port",
            Duration::from_mins(1),
            Duration::from_secs(30),
            |line| {
                let prefix = line.split("Zenoh can be reached at:").nth(1)?;
                let (_, port) = prefix.trim().rsplit_once(':')?;
                port.parse::<u16>().ok()
            },
        )
        .await
    }

    /// tries to find the zenoh id that is usually printed in logs
    pub async fn zenoh_zid(&self) -> String {
        self.find_in_logs(
            "zenoh zid",
            Duration::from_mins(1),
            Duration::from_secs(30),
            |line| {
                if !line.contains("region: Local") {
                    return None;
                }
                let zid: String = line
                    .split("zid: ")
                    .nth(1)?
                    .chars()
                    .take_while(char::is_ascii_hexdigit)
                    .collect();
                (!zid.is_empty()).then_some(zid)
            },
        )
        .await
    }

    /// Streams container logs starting `lookback` before now, following new output as it
    /// arrives, and returns the first value for which `extract` returns `Some`.
    /// Panics if `timeout` elapses or the log stream ends first.
    async fn find_in_logs<T>(
        &self,
        what: &str,
        timeout: Duration,
        lookback: Duration,
        mut extract: impl FnMut(&str) -> Option<T>,
    ) -> T {
        let since = SystemTime::now()
            .checked_sub(lookback)
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .and_then(|d| i32::try_from(d.as_secs()).ok())
            .expect("since timestamp fits in i32 until 2038");

        let mut logs = self.docker.logs(
            &self.container_id,
            Some(
                LogsOptionsBuilder::new()
                    .stdout(true)
                    .stderr(true)
                    .timestamps(true)
                    .since(since)
                    .follow(true)
                    .build(),
            ),
        );

        tokio::time::timeout(timeout, async {
            let mut buf = String::new();
            loop {
                let chunk = logs
                    .try_next()
                    .await
                    .expect("failed to read container logs")
                    .expect("log stream ended before a match was found");
                buf.push_str(&String::from_utf8_lossy(chunk.as_ref()));
                while let Some(idx) = buf.find('\n') {
                    let line = buf[..idx].to_owned();
                    buf.drain(..=idx);
                    if let Some(value) = extract(&line) {
                        return value;
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{what} not found in logs for container `{}` after {timeout:?}",
                self.container_id
            )
        })
    }

    /// execute a command in this container
    pub async fn exec(&self, cmd: &[&str]) -> CommandOutput {
        let exec = self
            .docker
            .create_exec(
                &self.container_id,
                CreateExecOptions {
                    cmd: Some(cmd.to_vec()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        if let StartExecResults::Attached { mut output, .. } =
            self.docker.start_exec(&exec.id, None).await.unwrap()
        {
            while let Some(msg) = output.next().await {
                match msg.unwrap() {
                    LogOutput::StdOut { message } => stdout.extend_from_slice(&message),
                    LogOutput::StdErr { message } => stderr.extend_from_slice(&message),
                    _ => {}
                }
            }
        }

        let inspect = self.docker.inspect_exec(&exec.id).await.unwrap();

        CommandOutput {
            success: inspect.exit_code == Some(0),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }
    }

    /// run a shell script inside the container
    pub async fn shell(&self, script: &str) -> CommandOutput {
        self.exec(&["sh", "-lc", script]).await
    }

    /// connect container to a network
    pub async fn connect_network(&self, network: &str) {
        self.docker
            .connect_network(
                network,
                NetworkConnectRequest {
                    container: self.container_id.clone(),
                    endpoint_config: Some(EndpointSettings::default()),
                },
            )
            .await
            .unwrap();
    }

    /// return the network interface that is used by the container for the specified network
    pub async fn network_interface(&self, network: &str) -> String {
        let ip = self.container_ip(network).await;
        let output = self
            .shell(&format!(
                "ip -o addr show | awk '$4 ~ /^{}\\// {{ print $2; exit }}'",
                ip
            ))
            .await;
        assert_command_success(
            &output,
            format_args!("failed to resolve interface for network `{network}`"),
        );

        output.stdout.trim().to_owned()
    }

    /// run iptables command with arguments
    pub async fn iptables(&self, args: &[&str]) -> CommandOutput {
        let mut cmd = vec!["iptables"];
        cmd.extend_from_slice(args);
        self.exec(&cmd).await
    }

    /// run tc command with arguments
    pub async fn tc(&self, args: &[&str]) -> CommandOutput {
        let mut cmd = vec!["tc"];
        cmd.extend_from_slice(args);
        self.exec(&cmd).await
    }

    /// clear any network rules
    pub async fn clear_network_rules(&self, network: &str) {
        let iface = self.network_interface(network).await;
        let ingress_chain = format!("TFW-IN-{iface}");
        let egress_chain = format!("TFW-OUT-{iface}");

        let output = self
            .shell(&format!(
                "iptables -w -F {ingress_chain} 2>/dev/null || true; \
                 iptables -w -F {egress_chain} 2>/dev/null || true"
            ))
            .await;
        assert_command_success(
            &output,
            format_args!("failed to clear iptables rules for network `{network}`"),
        );
    }

    /// allow all traffic
    pub async fn allow_all_traffic(&self, network: &str) {
        self.clear_network_rules(network).await;
    }

    /// setup rules to reject all traffic
    pub async fn reject_all_traffic(&self, network: &str) {
        let (_, ingress_chain, egress_chain) = self.network_chains(network).await;
        let output = self
            .shell(&format!(
                "iptables -w -F {ingress_chain}; \
                 iptables -w -F {egress_chain}; \
                 iptables -w -A {ingress_chain} -j REJECT; \
                 iptables -w -A {egress_chain} -j REJECT"
            ))
            .await;
        assert_command_success(
            &output,
            format_args!("failed to reject all traffic for network `{network}`"),
        );
    }

    /// allow multicast on network
    pub async fn allow_multicast(&self, network: &str) {
        let (_, ingress_chain, egress_chain) = self.network_chains(network).await;
        let output = self
            .shell(&format!(
                "iptables -w -I {ingress_chain} 1 -s 224.0.0.0/4 -j ACCEPT; \
                 iptables -w -I {egress_chain} 1 -d 224.0.0.0/4 -j ACCEPT"
            ))
            .await;
        assert_command_success(
            &output,
            format_args!("failed to allow multicast for network `{network}`"),
        );
    }

    /// reject connections from/to a specific remote on network
    pub async fn reject_remote(&self, network: &str, remote: &str) {
        let (_, ingress_chain, egress_chain) = self.network_chains(network).await;
        let output = self
            .shell(&format!(
                "iptables -w -A {ingress_chain} -s {remote} -j REJECT; \
                 iptables -w -A {egress_chain} -d {remote} -j REJECT"
            ))
            .await;
        assert_command_success(
            &output,
            format_args!("failed to reject remote `{remote}` on network `{network}`"),
        );
    }

    /// allow connections from/to a specific remote on network
    pub async fn allow_remote(&self, network: &str, remote: &str) {
        let (_, ingress_chain, egress_chain) = self.network_chains(network).await;
        let output = self
            .shell(&format!(
                "iptables -w -I {ingress_chain} 1 -s {remote} -j ACCEPT; \
                 iptables -w -I {egress_chain} 1 -d {remote} -j ACCEPT"
            ))
            .await;
        assert_command_success(
            &output,
            format_args!("failed to allow remote `{remote}` on network `{network}`"),
        );
    }

    async fn network_chains(&self, network: &str) -> (String, String, String) {
        let iface = self.network_interface(network).await;
        self.ensure_network_chains(&iface).await;
        let ingress_chain = format!("TFW-IN-{iface}");
        let egress_chain = format!("TFW-OUT-{iface}");
        (iface, ingress_chain, egress_chain)
    }

    /// clear network emulation (removes the tc root qdisc)
    pub async fn clear_netem(&self, network: &str) {
        let iface = self.network_interface(network).await;
        let output = self
            .shell(&format!(
                "tc qdisc del dev {iface} root 2>/dev/null || true"
            ))
            .await;
        assert_command_success(
            &output,
            format_args!("failed to clear tc netem on network `{network}`"),
        );
    }

    /// set network emulation with a tc netem `spec` (e.g. `delay 100ms loss 5%`)
    pub async fn set_netem(&self, network: &str, spec: &str) {
        let iface = self.network_interface(network).await;
        let output = self
            .shell(&format!(
                "tc qdisc del dev {iface} root 2>/dev/null || true; tc qdisc add dev {iface} root netem {spec}"
            ))
            .await;
        assert_command_success(
            &output,
            format_args!("failed to set tc netem `{spec}` on network `{network}`"),
        );
    }

    async fn ensure_network_chains(&self, iface: &str) {
        let ingress_chain = format!("TFW-IN-{iface}");
        let egress_chain = format!("TFW-OUT-{iface}");

        let output = self
            .shell(&format!(
                "iptables -w -N {ingress_chain} 2>/dev/null || true; \
                 iptables -w -N {egress_chain} 2>/dev/null || true; \
                 iptables -w -C INPUT -i {iface} -j {ingress_chain} 2>/dev/null || iptables -w -I INPUT 1 -i {iface} -j {ingress_chain}; \
                 iptables -w -C OUTPUT -o {iface} -j {egress_chain} 2>/dev/null || iptables -w -I OUTPUT 1 -o {iface} -j {egress_chain}"
            ))
            .await;
        assert_command_success(
            &output,
            format_args!("failed to prepare iptables chains for interface `{iface}`"),
        );
    }
}

fn assert_command_success(output: &CommandOutput, context: std::fmt::Arguments<'_>) {
    assert!(output.success, "{context}: {}", output.stderr);
}
