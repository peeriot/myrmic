use wiremock::http::Method;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub struct HttpMockHandle {
    server: MockServer,
    url: String,
}

impl HttpMockHandle {
    pub async fn start() -> Self {
        let server = MockServer::start().await;
        let url = server.uri();
        Self { server, url }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn expect_post(&self, endpoint_path: &str, response_body: impl Into<Vec<u8>>) {
        Mock::given(method("POST"))
            .and(path(endpoint_path))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(response_body, "application/octet-stream"),
            )
            .mount(&self.server)
            .await;
    }

    /// How many `POST {endpoint_path}` requests the mock server has received.
    ///
    /// This is how a bridge's outbound HTTP call is observed now that commands
    /// are fire-and-forget: the response no longer round-trips back to the
    /// caller, but the mock still records that the bridge actually called out.
    pub async fn received_post_count(&self, endpoint_path: &str) -> usize {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method == Method::POST && r.url.path() == endpoint_path)
            .count()
    }
}
