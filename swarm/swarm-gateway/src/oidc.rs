use crate::OidcConfig;
use anyhow::Context as _;
use axum::extract::Request;
use axum::http::uri::{InvalidUri, PathAndQuery};
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Redirect, Response};
use openidconnect::core::{
    CoreAuthDisplay, CoreAuthPrompt, CoreAuthenticationFlow, CoreClaimName, CoreClaimType,
    CoreClientAuthMethod, CoreErrorResponseType, CoreGenderClaim, CoreGrantType, CoreJsonWebKey,
    CoreJweContentEncryptionAlgorithm, CoreJweKeyManagementAlgorithm, CoreJwsSigningAlgorithm,
    CoreResponseMode, CoreResponseType, CoreRevocableToken, CoreRevocationErrorResponse,
    CoreSubjectIdentifierType, CoreTokenIntrospectionResponse, CoreTokenType,
};
use openidconnect::url::Url;
use openidconnect::{
    AccessToken, AccessTokenHash, AdditionalClaims, CsrfToken, EmptyAdditionalClaims,
    EmptyAdditionalProviderMetadata, EmptyExtraTokenFields, EndpointMaybeSet, EndpointNotSet,
    EndpointSet, Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl,
    StandardErrorResponse, StandardTokenResponse, TokenResponse as _,
};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

const SESSION_KEY: &str = "_oidc";

type IdTokenFields<AC = EmptyAdditionalClaims, TF = EmptyExtraTokenFields> =
    openidconnect::IdTokenFields<
        AC,
        TF,
        CoreGenderClaim,
        CoreJweContentEncryptionAlgorithm,
        CoreJwsSigningAlgorithm,
    >;

type TokenResponse<AC = EmptyAdditionalClaims, TF = EmptyExtraTokenFields> =
    StandardTokenResponse<IdTokenFields<AC, TF>, CoreTokenType>;

type IdToken<AZ = EmptyAdditionalClaims> = openidconnect::IdToken<
    AZ,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJwsSigningAlgorithm,
>;

type IdTokenClaims<AC = EmptyAdditionalClaims> = openidconnect::IdTokenClaims<AC, CoreGenderClaim>;

type Client<AC = EmptyAdditionalClaims, TF = EmptyExtraTokenFields> = openidconnect::Client<
    AC,
    CoreAuthDisplay,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJsonWebKey,
    CoreAuthPrompt,
    StandardErrorResponse<CoreErrorResponseType>,
    TokenResponse<AC, TF>,
    CoreTokenIntrospectionResponse,
    CoreRevocableToken,
    CoreRevocationErrorResponse,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

type ProviderMetadata = openidconnect::ProviderMetadata<
    EmptyAdditionalProviderMetadata,
    CoreAuthDisplay,
    CoreClientAuthMethod,
    CoreClaimName,
    CoreClaimType,
    CoreGrantType,
    CoreJweContentEncryptionAlgorithm,
    CoreJweKeyManagementAlgorithm,
    CoreJsonWebKey,
    CoreResponseMode,
    CoreResponseType,
    CoreSubjectIdentifierType,
>;

#[derive(Serialize, Deserialize, Debug)]
#[serde(bound = "AC: Serialize + serde::de::DeserializeOwned")]
struct Session<AC: AdditionalClaims = EmptyAdditionalClaims> {
    nonce: Nonce,
    csrf_token: CsrfToken,
    pkce_verifier: PkceCodeVerifier,
    authenticated: Option<AuthenticatedSession<AC>>,
    refresh_token: Option<openidconnect::RefreshToken>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(bound = "AC: Serialize + serde::de::DeserializeOwned")]
struct AuthenticatedSession<AC: AdditionalClaims = EmptyAdditionalClaims> {
    id_token: IdToken<AC>,
    access_token: AccessToken,
}

#[derive(Debug, Deserialize)]
struct Query {
    code: String,
    state: String,
}

/// Guards one request with the route's OIDC configuration.
///
/// `Ok` carries the request on to the application: the caller's session holds a
/// verified ID token. `Err` carries the response to send instead — a redirect
/// into (or back out of) the provider's login flow, or a status and a reason for
/// any step that failed.
pub async fn apply_oidc(
    http_client: &reqwest::Client,
    req: Request,
    config: OidcConfig,
) -> Result<Request, Response> {
    let (parts, body) = req.into_parts();

    let OidcConfig {
        application_base_url,
        issuer,
        client_id,
        client_secret,
        scopes,
    } = config;

    let session = parts
        .extensions
        .get::<tower_sessions::Session>()
        .ok_or_else(|| {
            reject(
                StatusCode::INTERNAL_SERVER_ERROR,
                "no session store on the request",
            )
        })?;

    let mut oidc_session = session.get::<Session>(SESSION_KEY).await.map_err(|err| {
        reject(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("unable to read the session: {err}"),
        )
    })?;

    let Some(application_base_url) = application_base_url else {
        return Err(reject(
            StatusCode::INTERNAL_SERVER_ERROR,
            "route has no `application_base_url` configured",
        ));
    };

    // Where the user ends up once the flow completes: what they asked for,
    // resolved against the application's base url, minus the provider's
    // callback parameters.
    let redirect_uri = clean_redirect_uri(&parts.uri, &application_base_url)?;

    let oidc_client = {
        let client = discover(http_client, &issuer, client_id, client_secret).await?;
        let uri = RedirectUrl::new(redirect_uri.to_string()).map_err(|err| {
            reject(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("`{redirect_uri}` is not a valid redirect uri: {err}"),
            )
        })?;
        client.set_redirect_uri(uri)
    };

    let Some(oidc_session) = oidc_session.as_mut() else {
        tracing::debug!("No session, setting up challenge");

        return Err(challenge(session, &oidc_client, &scopes).await);
    };

    let authenticated_session = oidc_session.authenticated.as_ref().and_then(|session| {
        let verifier = oidc_client.id_token_verifier();

        session
            .id_token
            .claims(&verifier, &oidc_session.nonce)
            .ok()
            .cloned()
            .map(|claims| (session, claims))
    });

    let (_access_token, _claims) = if let Some((session, claims)) = authenticated_session {
        tracing::debug!("has authenticated session");
        let access_token = session.access_token.secret().clone();
        (access_token, claims.clone())
    } else if let Some(refresh_token) = oidc_session.refresh_token.clone() {
        tracing::debug!("needs to refresh");

        refresh(
            &oidc_client,
            http_client,
            session,
            oidc_session,
            &refresh_token,
            &scopes,
        )
        .await?
    } else if let Ok(query) = axum::extract::Query::<Query>::try_from_uri(&parts.uri) {
        tracing::debug!("parsed query parameters from OIDC provider");

        exchange_code(&oidc_client, http_client, session, oidc_session, &query).await?;

        tracing::debug!("redirecting to {}", redirect_uri);

        return Err(Redirect::temporary(&redirect_uri.to_string()).into_response());
    } else {
        tracing::debug!("No query parameters provided, redirected to login");

        return Err(challenge(session, &oidc_client, &scopes).await);
    };

    Ok(Request::from_parts(parts, body))
}

/// Ends the request with a status and a one-line reason. The request never
/// reaches the application, so the client gets this instead of the page it
/// asked for.
fn reject(status: StatusCode, message: impl std::fmt::Display) -> Response {
    tracing::warn!("oidc: {message}");
    (status, format!("oidc: {message}\n")).into_response()
}

/// Builds the client for a route by discovering the provider's metadata.
async fn discover(
    http_client: &reqwest::Client,
    issuer: &str,
    client_id: String,
    client_secret: Option<String>,
) -> Result<Client, Response> {
    let url = openidconnect::IssuerUrl::new(issuer.to_owned()).map_err(|err| {
        reject(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("configured issuer `{issuer}` is not a valid url: {err}"),
        )
    })?;

    let metadata = ProviderMetadata::discover_async(url, http_client)
        .await
        .map_err(|err| {
            reject(
                StatusCode::BAD_GATEWAY,
                format!("unable to discover the provider at `{issuer}`: {err}"),
            )
        })?;

    Ok(Client::from_provider_metadata(
        metadata,
        openidconnect::ClientId::new(client_id),
        client_secret.map(openidconnect::ClientSecret::new),
    ))
}

/// Trades the session's refresh token for a fresh access token, returning the
/// new access token and its verified claims.
async fn refresh(
    client: &Client,
    http_client: &reqwest::Client,
    session: &tower_sessions::Session,
    oidc_session: &mut Session,
    refresh_token: &openidconnect::RefreshToken,
    scopes: &[String],
) -> Result<(String, IdTokenClaims), Response> {
    let mut request = client
        .exchange_refresh_token(refresh_token)
        .map_err(|err| {
            reject(
                StatusCode::BAD_GATEWAY,
                format!("provider advertises no token endpoint: {err}"),
            )
        })?;

    for scope in scopes.iter() {
        request = request.add_scope(openidconnect::Scope::new(scope.clone()));
    }

    let response: TokenResponse = request.request_async(http_client).await.map_err(|err| {
        reject(
            StatusCode::BAD_GATEWAY,
            format!("unable to refresh the access token: {err}"),
        )
    })?;

    let claims = store_tokens(client, session, oidc_session, &response).await?;

    tracing::debug!("refreshed");

    Ok((response.access_token().secret().clone(), claims))
}

/// Completes the authorization-code flow: checks the callback against the
/// challenge we issued, then trades the code for tokens.
async fn exchange_code(
    client: &Client,
    http_client: &reqwest::Client,
    session: &tower_sessions::Session,
    oidc_session: &mut Session,
    query: &Query,
) -> Result<(), Response> {
    if oidc_session.csrf_token.secret() != &query.state {
        return Err(reject(
            StatusCode::BAD_REQUEST,
            "the callback's `state` does not match the session's csrf token",
        ));
    }

    let request = client
        .exchange_code(openidconnect::AuthorizationCode::new(query.code.clone()))
        .map_err(|err| {
            reject(
                StatusCode::BAD_GATEWAY,
                format!("provider advertises no token endpoint: {err}"),
            )
        })?;

    let response = request
        .set_pkce_verifier(PkceCodeVerifier::new(
            oidc_session.pkce_verifier.secret().clone(),
        ))
        .request_async(http_client)
        .await
        .map_err(|err| {
            reject(
                StatusCode::BAD_GATEWAY,
                format!("unable to exchange the authorization code: {err}"),
            )
        })?;

    store_tokens(client, session, oidc_session, &response).await?;

    Ok(())
}

/// Starts a fresh authorization-code flow: records the challenge on the session
/// and sends the user to the provider to log in.
async fn challenge(
    session: &tower_sessions::Session,
    client: &Client,
    scopes: &[String],
) -> Response {
    let (oidc_session, auth_url) = setup_challenge(client, scopes);

    if let Err(err) = session.insert(SESSION_KEY, oidc_session).await {
        return reject(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("unable to store the auth challenge: {err}"),
        );
    }

    tracing::debug!("redirecting to {}", auth_url.as_str());

    Redirect::temporary(auth_url.as_str()).into_response()
}

/// Verifies a freshly issued token response and records it on the session,
/// returning the verified claims.
///
/// The response must carry an ID token, its claims must verify against the
/// challenge's nonce, and — when the ID token names one — the access token must
/// match the hash it commits to.
async fn store_tokens(
    client: &Client,
    session: &tower_sessions::Session,
    oidc_session: &mut Session,
    response: &TokenResponse,
) -> Result<IdTokenClaims, Response> {
    let Some(id_token) = response.id_token() else {
        return Err(reject(
            StatusCode::BAD_GATEWAY,
            "token response contained no id token",
        ));
    };

    let verifier = client.id_token_verifier();
    let claims = id_token
        .claims(&verifier, &oidc_session.nonce)
        .map_err(|err| {
            reject(
                StatusCode::UNAUTHORIZED,
                format!("unable to verify the id token's claims: {err}"),
            )
        })?
        .clone();

    validate_access_token_hash(id_token, response.access_token(), &claims, client)
        .map_err(|err| reject(StatusCode::UNAUTHORIZED, err))?;

    oidc_session.authenticated = Some(AuthenticatedSession {
        id_token: id_token.clone(),
        access_token: response.access_token().clone(),
    });
    if let Some(refresh_token) = response.refresh_token() {
        oidc_session.refresh_token = Some(refresh_token.clone());
    }

    session
        .insert(SESSION_KEY, &*oidc_session)
        .await
        .map_err(|err| {
            reject(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("unable to store the authenticated session: {err}"),
            )
        })?;

    Ok(claims)
}

fn setup_challenge(client: &Client, scopes: &[String]) -> (Session, Url) {
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    let (auth_url, csrf_token, nonce) = {
        let mut auth = client.authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        );

        for scope in scopes.iter() {
            auth = auth.add_scope(openidconnect::Scope::new(scope.clone()));
        }

        auth.set_pkce_challenge(pkce_challenge).url()
    };

    let oidc_session = Session {
        nonce,
        csrf_token,
        pkce_verifier,
        authenticated: None,
        refresh_token: None,
    };

    (oidc_session, auth_url)
}

/// Double checks the validity of the AT hash.
/// This catches things like someone swapping out the token from a different user, etc.
fn validate_access_token_hash<AC: AdditionalClaims>(
    id_token: &IdToken<AC>,
    access_token: &AccessToken,
    claims: &IdTokenClaims<AC>,
    client: &Client,
) -> anyhow::Result<()> {
    let Some(expected_hash) = claims.access_token_hash() else {
        return Ok(());
    };

    let verifier = client.id_token_verifier();
    let signing_key = id_token
        .signing_key(&verifier)
        .context("Unable to build signing key")?;
    let signing_algo = id_token
        .signing_alg()
        .context("Unable to build signing algo")?;
    let hash = AccessTokenHash::from_token(access_token, signing_algo, signing_key)
        .context("Unable to build hash from token")?;

    anyhow::ensure!(&hash == expected_hash, "invalid hash detected");

    Ok(())
}

/// Resolves the request's path against the application's base url, with the
/// provider's callback parameters stripped — the url to send the user back to
/// once the flow completes.
fn clean_redirect_uri(uri: &Uri, application_base_url: &str) -> Result<Uri, Response> {
    let cleaned = strip_oidc_from_path(uri)
        .map_err(|err| {
            reject(
                StatusCode::BAD_REQUEST,
                format!("unable to rebuild the request path: {err}"),
            )
        })?
        .ok_or_else(|| reject(StatusCode::BAD_REQUEST, "request carries no path"))?;

    let base = Uri::from_str(application_base_url).map_err(|err| {
        reject(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("`{application_base_url}` is not a valid application base url: {err}"),
        )
    })?;

    let mut parts = base.into_parts();
    parts.path_and_query = Some(cleaned);

    Uri::from_parts(parts).map_err(|err| {
        reject(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("unable to build a redirect uri from `{application_base_url}`: {err}"),
        )
    })
}

/// This cleans the uri, as we're passing quite a lot of information via the url.
fn strip_oidc_from_path(uri: &Uri) -> Result<Option<PathAndQuery>, InvalidUri> {
    uri.path_and_query()
        .map(|path_and_query| {
            let query = path_and_query
                .query()
                .and_then(|uri| {
                    uri.split('&')
                        .filter(|x| {
                            !x.starts_with("code")
                                && !x.starts_with("state")
                                && !x.starts_with("session_state")
                                && !x.starts_with("iss")
                        })
                        .map(ToString::to_string)
                        .reduce(|acc, x| acc + "&" + &x)
                })
                .map(|x| format!("?{x}"))
                .unwrap_or_default();

            PathAndQuery::from_maybe_shared(format!("{}{}", path_and_query.path(), query))
        })
        .transpose()
}
