//! helper functions for communicating with the test-sidecar over its HTTP API
use std::path::PathBuf;

use bollard::Docker;
use introspection_common::v1::NodeStatus;
use serde::de::DeserializeOwned;

use crate::docker::image::Image;

/// HTTP client for a running test-sidecar instance.
///
/// The sidecar is a helper process (usually inside a compose network) that opens zenoh
/// sessions on request, so tests can observe and inject traffic from a node's perspective.
pub struct Sidecar<'u> {
    client: reqwest::Client,
    url: &'u str,
}

/// Mode the sidecar uses when opening a zenoh session.
#[derive(Clone, Copy)]
pub enum ZenohMode {
    /// connect as a zenoh client
    Client,
    /// connect as a zenoh peer
    Peer,
}

impl ZenohMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Peer => "peer",
        }
    }
}

impl<'u> Sidecar<'u> {
    /// Create a client for the sidecar reachable at `sidecar_addr` (e.g. `http://127.0.0.1:8080`).
    pub fn new(sidecar_addr: &'u str) -> Self {
        Self {
            client: reqwest::Client::new(),
            url: sidecar_addr,
        }
    }

    /// Build the test-sidecar docker image with tag `tag` from `sidecar_dockerfile`, placing
    /// the sidecar binary (as `test-sidecar`) in the build context.
    pub async fn build(
        docker: &Docker,
        sidecar_dockerfile: impl Into<PathBuf>,
        sidecar_binary: impl Into<PathBuf>,
        tag: &str,
    ) {
        let _image = Image::build(
            docker,
            tag,
            sidecar_dockerfile.into().iter().as_path(),
            &[(sidecar_binary.into().as_path(), "test-sidecar")],
        )
        .await;
    }

    /// Poll [`Self::count_exec_runtimes`] every `sleep` until `break_condition` returns true,
    /// then return that response. Returns `None` if a request fails.
    pub async fn retry_count_exec_runtimes_until(
        &self,
        zenoh_endpoint: &str,
        break_condition: impl Fn(&get_exec_runtimes::Response) -> bool,
        sleep: tokio::time::Duration,
    ) -> Option<get_exec_runtimes::Response> {
        let response = loop {
            let response = self.count_exec_runtimes(zenoh_endpoint).await.ok()?;
            if break_condition(&response) {
                break response;
            }
            tokio::time::sleep(sleep).await;
        };

        Some(response)
    }

    /// [`Self::count_exec_runtimes_with_mode`] in [`ZenohMode::Client`] mode.
    pub async fn count_exec_runtimes(
        &self,
        zenoh_endpoint: &str,
    ) -> Result<get_exec_runtimes::Response, String> {
        self.count_exec_runtimes_with_mode(zenoh_endpoint, ZenohMode::Client)
            .await
    }

    /// Count the exec runtimes discoverable from `zenoh_endpoint`, as seen by the sidecar.
    pub async fn count_exec_runtimes_with_mode(
        &self,
        zenoh_endpoint: &str,
        mode: ZenohMode,
    ) -> Result<get_exec_runtimes::Response, String> {
        let query = get_exec_runtimes::Query {
            endpoint: zenoh_endpoint.into(),
            mode: mode.as_str().into(),
        };

        let response = self
            .client
            .get(format!("{}/exec-runtimes", self.url))
            .query(&query)
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;

        decode_response(response).await
    }

    /// [`Self::own_status_with_mode`] in [`ZenohMode::Client`] mode.
    pub async fn own_status(&self, zenoh_endpoint: &str) -> Result<NodeStatus, String> {
        self.own_status_with_mode(zenoh_endpoint, ZenohMode::Client)
            .await
    }

    /// Query the introspection status of the node at `zenoh_endpoint` itself.
    pub async fn own_status_with_mode(
        &self,
        zenoh_endpoint: &str,
        mode: ZenohMode,
    ) -> Result<NodeStatus, String> {
        let query = own_status::Query {
            endpoint: zenoh_endpoint.into(),
            mode: mode.as_str().into(),
        };

        let response = self
            .client
            .get(format!("{}/own-status", self.url))
            .query(&query)
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;

        decode_response(response).await
    }

    /// [`Self::swarm_status_with_mode`] in [`ZenohMode::Client`] mode.
    pub async fn swarm_status(&self, zenoh_endpoint: &str) -> Result<Vec<NodeStatus>, String> {
        self.swarm_status_with_mode(zenoh_endpoint, ZenohMode::Client)
            .await
    }

    /// Query the introspection status of every node reachable via `zenoh_endpoint`.
    pub async fn swarm_status_with_mode(
        &self,
        zenoh_endpoint: &str,
        mode: ZenohMode,
    ) -> Result<Vec<NodeStatus>, String> {
        let query = swarm_status::Query {
            endpoint: zenoh_endpoint.into(),
            mode: mode.as_str().into(),
        };

        let response = self
            .client
            .get(format!("{}/swarm-status", self.url))
            .query(&query)
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;

        decode_response(response).await
    }

    /// Register a persistent zenoh queryable via the sidecar.
    ///
    /// The sidecar opens a session to `zenoh_endpoint` and declares a queryable
    /// on `key_expr` that replies to every incoming query with `payload`.
    /// The queryable stays alive until the sidecar process exits.
    pub async fn start_queryable(
        &self,
        zenoh_endpoint: &str,
        mode: ZenohMode,
        key_expr: &str,
        payload: &str,
    ) -> Result<(), String> {
        let query = start_queryable::Query {
            endpoint: zenoh_endpoint.into(),
            mode: mode.as_str().into(),
            key_expr: key_expr.into(),
            payload: payload.into(),
        };

        let response = self
            .client
            .post(format!("{}/zenoh/start-queryable", self.url))
            .query(&query)
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| format!("failed to read response body: {err}"))?;

        if !status.is_success() {
            return Err(format!("sidecar returned {status}: {body}"));
        }

        Ok(())
    }

    /// Start a persistent zenoh subscriber via the sidecar. Returns an opaque subscriber ID.
    ///
    /// The sidecar opens a session to `zenoh_endpoint`, declares a subscriber on `key_expr`,
    /// and counts every received sample. Use `subscriber_count` to read the count. The
    /// subscriber stays alive until the sidecar process exits.
    pub async fn start_subscriber(
        &self,
        zenoh_endpoint: &str,
        mode: ZenohMode,
        key_expr: &str,
    ) -> Result<usize, String> {
        let query = start_subscriber::Query {
            endpoint: zenoh_endpoint.into(),
            mode: mode.as_str().into(),
            key_expr: key_expr.into(),
        };

        let response = self
            .client
            .post(format!("{}/zenoh/start-subscriber", self.url))
            .query(&query)
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;

        decode_response::<start_subscriber::Response>(response)
            .await
            .map(|r| r.sub_id)
    }

    /// Return the number of messages received by the subscriber with the given ID.
    pub async fn subscriber_count(&self, sub_id: usize) -> Result<usize, String> {
        let response = self
            .client
            .get(format!("{}/zenoh/subscriber/{sub_id}/count", self.url))
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;

        decode_response::<subscriber_count::Response>(response)
            .await
            .map(|r| r.count)
    }

    /// Publish `count` messages to `key_expr` via the sidecar.
    ///
    /// The sidecar opens a transient session to `zenoh_endpoint` and publishes `count` messages
    /// to `key_expr`. The session is closed after publishing.
    pub async fn publish(
        &self,
        zenoh_endpoint: &str,
        mode: ZenohMode,
        key_expr: &str,
        count: usize,
    ) -> Result<(), String> {
        let query = publish::Query {
            endpoint: zenoh_endpoint.into(),
            mode: mode.as_str().into(),
            key_expr: key_expr.into(),
            count,
        };

        let response = self
            .client
            .post(format!("{}/zenoh/publish", self.url))
            .query(&query)
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|err| format!("failed to read response body: {err}"))?;

        if !status.is_success() {
            return Err(format!("sidecar returned {status}: {body}"));
        }

        Ok(())
    }

    /// Create a test-control subscriber on the node with the given `zid`, subscribing to
    /// `key_expr` (optionally bounded by `max_samples` and mirroring to `stream_key`).
    pub async fn tc_create_subscriber(
        &self,
        zenoh_endpoint: &str,
        mode: ZenohMode,
        zid: &str,
        key_expr: &str,
        max_samples: Option<u32>,
        stream_key: Option<&str>,
    ) -> Result<tc_create_subscriber::Response, String> {
        let query = tc_create_subscriber::Query {
            endpoint: zenoh_endpoint.into(),
            mode: mode.as_str().into(),
            zid: zid.into(),
            key_expr: key_expr.into(),
            max_samples,
            stream_key: stream_key.map(Into::into),
        };

        let response = self
            .client
            .post(format!("{}/test-control/subscriber", self.url))
            .query(&query)
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;

        decode_response(response).await
    }

    /// Create a test-control publisher on the node with the given `zid`, publishing `payload`
    /// to `key_expr` (`count` times, with `delay_ms` between messages, if given).
    #[allow(clippy::too_many_arguments)]
    pub async fn tc_create_publisher(
        &self,
        zenoh_endpoint: &str,
        mode: ZenohMode,
        zid: &str,
        key_expr: &str,
        payload: &str,
        count: Option<u32>,
        delay_ms: Option<u64>,
    ) -> Result<tc_create_publisher::Response, String> {
        let query = tc_create_publisher::Query {
            endpoint: zenoh_endpoint.into(),
            mode: mode.as_str().into(),
            zid: zid.into(),
            key_expr: key_expr.into(),
            payload: payload.into(),
            count,
            delay_ms,
        };

        let response = self
            .client
            .post(format!("{}/test-control/publisher", self.url))
            .query(&query)
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;

        decode_response(response).await
    }

    /// Read the test-control traffic statistics for `key_expr` on the node with the given `zid`.
    pub async fn tc_stats(
        &self,
        zenoh_endpoint: &str,
        mode: ZenohMode,
        zid: &str,
        key_expr: &str,
    ) -> Result<tc_stats::Response, String> {
        let query = tc_stats::Query {
            endpoint: zenoh_endpoint.into(),
            mode: mode.as_str().into(),
            zid: zid.into(),
            key_expr: key_expr.into(),
        };

        let response = self
            .client
            .get(format!("{}/test-control/stats", self.url))
            .query(&query)
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;

        decode_response(response).await
    }

    /// Issue a zenoh get via the sidecar and return the number of replies received.
    ///
    /// The sidecar opens a client session to `zenoh_endpoint`, issues a get on
    /// `key_expr` with the given timeout, collects all replies, and returns their count.
    pub async fn zenoh_get(
        &self,
        zenoh_endpoint: &str,
        mode: ZenohMode,
        key_expr: &str,
        timeout_ms: u64,
    ) -> Result<usize, String> {
        let query = zenoh_get::Query {
            endpoint: zenoh_endpoint.into(),
            mode: mode.as_str().into(),
            key_expr: key_expr.into(),
            timeout_ms,
        };

        let response = self
            .client
            .get(format!("{}/zenoh/get", self.url))
            .query(&query)
            .send()
            .await
            .map_err(|err| format!("request failed: {err}"))?;

        decode_response::<zenoh_get::Response>(response)
            .await
            .map(|r| r.replies)
    }
}

async fn decode_response<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, String> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|err| format!("failed to read response body: {err}"))?;

    if !status.is_success() {
        return Err(format!("sidecar returned {status}: {body}"));
    }

    serde_json::from_str(&body)
        .map_err(|err| format!("failed to decode sidecar response: {err}; body: {body}"))
}

/// Wire format of the `GET /exec-runtimes` endpoint.
pub mod get_exec_runtimes {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize)]
    pub struct Query {
        pub endpoint: String,
        pub mode: String,
    }

    #[derive(Deserialize)]
    pub struct Response {
        pub count: usize,
    }
}

/// Wire format of the `GET /own-status` endpoint.
pub mod own_status {
    use serde::Serialize;

    #[derive(Serialize)]
    pub struct Query {
        pub endpoint: String,
        pub mode: String,
    }
}

/// Wire format of the `GET /swarm-status` endpoint.
pub mod swarm_status {
    use serde::Serialize;

    #[derive(Serialize)]
    pub struct Query {
        pub endpoint: String,
        pub mode: String,
    }
}

/// Wire format of the `POST /zenoh/start-queryable` endpoint.
pub mod start_queryable {
    use serde::Serialize;

    #[derive(Serialize)]
    pub struct Query {
        pub endpoint: String,
        pub mode: String,
        pub key_expr: String,
        pub payload: String,
    }
}

/// Wire format of the `POST /zenoh/start-subscriber` endpoint.
pub mod start_subscriber {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize)]
    pub struct Query {
        pub endpoint: String,
        pub mode: String,
        pub key_expr: String,
    }

    #[derive(Deserialize)]
    pub struct Response {
        pub sub_id: usize,
    }
}

/// Wire format of the `GET /zenoh/subscriber/{sub_id}/count` endpoint.
pub mod subscriber_count {
    use serde::Deserialize;

    #[derive(Deserialize)]
    pub struct Response {
        pub count: usize,
    }
}

/// Wire format of the `POST /zenoh/publish` endpoint.
pub mod publish {
    use serde::Serialize;

    #[derive(Serialize)]
    pub struct Query {
        pub endpoint: String,
        pub mode: String,
        pub key_expr: String,
        pub count: usize,
    }
}

/// Wire format of the `POST /test-control/subscriber` endpoint.
pub mod tc_create_subscriber {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize)]
    pub struct Query {
        pub endpoint: String,
        pub mode: String,
        pub zid: String,
        pub key_expr: String,
        pub max_samples: Option<u32>,
        pub stream_key: Option<String>,
    }

    #[derive(Deserialize)]
    pub struct Response {
        pub ok: bool,
        pub sub_id: String,
        pub key_expr: String,
    }
}

/// Wire format of the `POST /test-control/publisher` endpoint.
pub mod tc_create_publisher {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize)]
    pub struct Query {
        pub endpoint: String,
        pub mode: String,
        pub zid: String,
        pub key_expr: String,
        pub payload: String,
        pub count: Option<u32>,
        pub delay_ms: Option<u64>,
    }

    #[derive(Deserialize)]
    pub struct Response {
        pub ok: bool,
        pub pub_id: String,
        pub key_expr: String,
    }
}

/// Wire format of the `GET /test-control/stats` endpoint.
pub mod tc_stats {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize)]
    pub struct Query {
        pub endpoint: String,
        pub mode: String,
        pub zid: String,
        pub key_expr: String,
    }

    #[derive(Deserialize)]
    pub struct Response {
        pub ok: bool,
        pub key_expr: String,
        pub sent: u32,
        pub received: u32,
        pub gets: u32,
        pub queries: u32,
    }
}

/// Wire format of the `GET /zenoh/get` endpoint.
pub mod zenoh_get {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize)]
    pub struct Query {
        pub endpoint: String,
        pub mode: String,
        pub key_expr: String,
        pub timeout_ms: u64,
    }

    #[derive(Deserialize)]
    pub struct Response {
        pub replies: usize,
    }
}
