//! End-to-end checks that a stream on an OIDC-guarded mount ends when the
//! login's ID token expires.
//!
//! A unit-test module rather than `tests/`: it drives the crate-private
//! `handler` and `AppState`, behind a mock OIDC provider and a loopback
//! zenoh session.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use axum::http::header;
use cell_protocol::Sri;
use chrono::{TimeZone as _, Utc};
use futures_util::StreamExt as _;
use openidconnect::core::{
    CoreIdToken, CoreIdTokenClaims, CoreJwsSigningAlgorithm, CoreRsaPrivateSigningKey,
};
use openidconnect::url::Url;
use openidconnect::{
    Audience, EmptyAdditionalClaims, IssuerUrl, JsonWebKeyId, Nonce, PrivateSigningKey as _,
    StandardClaims, SubjectIdentifier,
};
use reqwest::StatusCode;
use rsa::RsaPrivateKey;
use rsa::pkcs1::{EncodeRsaPrivateKey as _, LineEnding};
use sorg_common::gateway_config::GatewayRoute;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::{AppState, OidcConfig, RouteConfig, RoutingConfig, handler, sessions};

const MOUNT: &str = "/app";
const CLIENT_ID: &str = "gateway-test";
/// Guaranteed lifetime of the ID tokens the mock provider issues. `exp` is
/// whole seconds, so it is set past the next second boundary: a token lives
/// between this and this plus 1 second after it is issued.
const TOKEN_LIFETIME_SECS: i64 = 3;
/// How long past `exp` a stream may take to end.
const SLACK: Duration = Duration::from_secs(3);

/// The mock provider's signing key as PKCS#1 PEM, generated once per test
/// binary so no private key lives in the repository.
static IDP_KEY_PEM: LazyLock<String> = LazyLock::new(|| {
    RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
        .expect("generate the test RSA key")
        .to_pkcs1_pem(LineEnding::LF)
        .expect("encode the test RSA key")
        .to_string()
});

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guarded_ws_closes_with_revoked_at_token_expiry() -> anyhow::Result<()> {
    let idp = MockIdp::start().await?;
    let gateway = Gateway::spawn(&idp).await?;
    let login = gateway.login(&idp).await?;

    let mut request = format!("ws://{}{MOUNT}/ws", gateway.addr).into_client_request()?;
    request
        .headers_mut()
        .insert(header::COOKIE, login.cookie.parse()?);
    let (mut ws, _) = tokio_tungstenite::connect_async(request).await?;

    // The guard let the request through and the session started.
    let first = tokio::time::timeout(SLACK, ws.next())
        .await?
        .context("socket ended before Ready")??;
    let WsMessage::Text(ready) = first else {
        anyhow::bail!("expected Ready, got {first:?}");
    };
    assert!(ready.as_str().contains(r#""type":"ready""#), "{ready}");

    let wait = until(login.expires_at) + SLACK;
    let frame = tokio::time::timeout(wait, async {
        while let Some(message) = ws.next().await {
            if let WsMessage::Close(frame) = message? {
                return anyhow::Ok(frame);
            }
        }
        anyhow::bail!("socket ended without a close frame")
    })
    .await
    .context("socket still open after the token expired")??
    .context("close frame carries no code")?;

    assert!(
        SystemTime::now() >= login.expires_at,
        "closed before the token expired"
    );
    assert_eq!(u16::from(frame.code), 1008);
    assert_eq!(frame.reason.as_str(), "revoked");

    // The server closes the connection after the close frame.
    let rest = tokio::time::timeout(Duration::from_secs(1), ws.next()).await?;
    assert!(
        matches!(rest, None | Some(Err(_))),
        "socket still open after the close: {rest:?}"
    );
    Ok(())
}

/// Time left until `at`, zero once it has passed.
fn until(at: SystemTime) -> Duration {
    at.duration_since(SystemTime::now()).unwrap_or_default()
}

/// A mock OIDC provider: discovery, JWKS, and a token endpoint answering
/// the code exchange with a short-lived RS256 ID token.
struct MockIdp {
    server: MockServer,
    key: CoreRsaPrivateSigningKey,
}

impl MockIdp {
    async fn start() -> anyhow::Result<Self> {
        let server = MockServer::start().await;
        let key = CoreRsaPrivateSigningKey::from_pem(
            &IDP_KEY_PEM,
            Some(JsonWebKeyId::new("test".to_owned())),
        )
        .map_err(anyhow::Error::msg)?;

        let issuer = server.uri();
        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}/token"),
                "jwks_uri": format!("{issuer}/jwks"),
                "response_types_supported": ["code"],
                "subject_types_supported": ["public"],
                "id_token_signing_alg_values_supported": ["RS256"],
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/jwks"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "keys": [key.as_verification_key()] })),
            )
            .mount(&server)
            .await;

        Ok(Self { server, key })
    }

    /// Answers the code exchange with an ID token for `nonce` that expires
    /// `TOKEN_LIFETIME_SECS` from now, and returns that expiry.
    async fn issue(&self, nonce: &str) -> anyhow::Result<SystemTime> {
        let now = Utc::now().timestamp();
        let iat = Utc
            .timestamp_opt(now, 0)
            .single()
            .context("iat out of range")?;
        let exp = Utc
            .timestamp_opt(now + 1 + TOKEN_LIFETIME_SECS, 0)
            .single()
            .context("exp out of range")?;

        let claims = CoreIdTokenClaims::new(
            IssuerUrl::new(self.server.uri())?,
            vec![Audience::new(CLIENT_ID.to_owned())],
            exp,
            iat,
            StandardClaims::new(SubjectIdentifier::new("user".to_owned())),
            EmptyAdditionalClaims {},
        )
        .set_nonce(Some(Nonce::new(nonce.to_owned())));
        let id_token = CoreIdToken::new(
            claims,
            &self.key,
            CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
            None,
            None,
        )?;

        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access",
                "token_type": "Bearer",
                "id_token": id_token,
            })))
            .mount(&self.server)
            .await;

        Ok(SystemTime::from(exp))
    }
}

/// The gateway's real `handler`, served on a loopback port with one mount
/// (`/app`, `ws` at `/ws`, `api` at `/api`) guarded by the mock provider.
struct Gateway {
    addr: SocketAddr,
    client: reqwest::Client,
}

/// A logged-in browser session.
struct Login {
    /// `name=value` of the gateway's session cookie.
    cookie: String,
    /// When the session's ID token expires.
    expires_at: SystemTime,
}

impl Gateway {
    async fn spawn(idp: &MockIdp) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        let mut config = zenoh::Config::default();
        config
            .insert_json5("timestamping/enabled", "{ peer: true }")
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        config
            .insert_json5("scouting/multicast/enabled", "false")
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let session = zenoh::open(config)
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;

        let route = GatewayRoute {
            owner: Sri::from_uuid(Uuid::new_v4()),
            mount: MOUNT.to_owned(),
            assets: None,
            api: Some("/api".to_owned()),
            ws: Some("/ws".to_owned()),
        };
        let oidc = OidcConfig {
            application_base_url: Some(format!("http://{addr}")),
            issuer: idp.server.uri(),
            client_id: CLIENT_ID.to_owned(),
            client_secret: None,
            scopes: Vec::new(),
        };
        let routing = RoutingConfig {
            base_url: None,
            routes: Some(HashMap::from([(
                MOUNT.to_owned(),
                RouteConfig {
                    srn: None,
                    oidc: Some(oidc),
                },
            )])),
        };
        let client = reqwest::ClientBuilder::new()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        let state = AppState {
            session,
            routes: Arc::new(RwLock::new(vec![route])),
            sessions: sessions::Sessions::default(),
            routing: Some(Arc::new(RwLock::new(routing))),
            http_client: client.clone(),
        };
        let app = axum::Router::new()
            .fallback(handler)
            .with_state(state)
            .layer(
                tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default())
                    .with_secure(false),
            );
        tokio::spawn(async move { axum::serve(listener, app).await });

        Ok(Self { addr, client })
    }

    /// Runs the authorization-code flow against `idp` and returns the
    /// logged-in session.
    async fn login(&self, idp: &MockIdp) -> anyhow::Result<Login> {
        let start = self
            .client
            .get(format!("http://{}{MOUNT}/ws", self.addr))
            .send()
            .await?;
        anyhow::ensure!(
            start.status() == StatusCode::TEMPORARY_REDIRECT,
            "expected a login redirect, got {}",
            start.status()
        );
        let cookie = session_cookie(&start)?;

        let authorize = Url::parse(
            start
                .headers()
                .get(header::LOCATION)
                .context("login redirect has no location")?
                .to_str()?,
        )?;
        let param = |name: &str| {
            authorize
                .query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
                .with_context(|| format!("authorize url has no `{name}`"))
        };
        let (state, nonce) = (param("state")?, param("nonce")?);

        let expires_at = idp.issue(&nonce).await?;

        let callback = self
            .client
            .get(format!(
                "http://{}{MOUNT}/ws?code=test-code&state={state}",
                self.addr
            ))
            .header(header::COOKIE, &cookie)
            .send()
            .await?;
        let status = callback.status();
        anyhow::ensure!(
            status == StatusCode::TEMPORARY_REDIRECT,
            "code exchange failed: {status} {}",
            callback.text().await?
        );

        Ok(Login { cookie, expires_at })
    }
}

/// The `name=value` part of a response's session cookie.
fn session_cookie(response: &reqwest::Response) -> anyhow::Result<String> {
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .context("response sets no session cookie")?
        .to_str()?;
    Ok(set_cookie.split(';').next().unwrap_or_default().to_owned())
}
