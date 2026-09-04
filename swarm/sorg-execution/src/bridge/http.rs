use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::CONTENT_TYPE;
use sorg_common::{
    BodyTemplate, HttpBridgeApi, HttpBridgeRecord, OutgoingMessage, ResponseHeaderTemplate,
    WireHttpResponseTemplate, WireHttpResponseVariant, custom_err, status_variant_name,
};
use tokio::sync::{Barrier, Notify};
use tokio::time::timeout;
use zenoh::Session;

use cell_protocol::{MailboxCommand, Sri};
use myrmic_common::cells::Command;

use crate::bridge::consumer::spawn_bridge_command_consumer;
use crate::payload::{Marker, get_val, resolve_segments_as_string};
use crate::wasm::cell::state::DropHandle;

const BARRIER_TIMEOUT: Duration = Duration::from_secs(5);

pub struct HttpBridgeHandle {
    record: HttpBridgeRecord,
    session: Session,
    mailbox_poll_interval: Duration,

    state: EgressState,
}

enum EgressState {
    Uninit,
    Init(Inner),
}

struct Inner {
    barrier: Arc<Barrier>,
    kill_signal: Arc<Notify>,

    _egress_cells: Vec<DropHandle>,
}

impl HttpBridgeHandle {
    pub fn new(
        record: HttpBridgeRecord,
        session: &Session,
        mailbox_poll_interval: Duration,
    ) -> Self {
        Self {
            record,
            session: session.clone(),
            mailbox_poll_interval,
            state: EgressState::Uninit,
        }
    }

    pub async fn init(&mut self) -> crate::Result<()> {
        if !matches!(self.state, EgressState::Uninit) {
            Err(custom_err!("init was already called"))?;
        }

        let api = std::mem::take(&mut self.record.api);

        let barrier = Arc::new(Barrier::new(api.len() + 1));
        let kill_signal = Arc::new(Notify::new());

        // Each api acts as a single cell with a bunch of commands.
        let mut tasks = vec![];

        let db = db_client::v1::Client::new(&self.session);

        let client = reqwest::Client::new();

        for api in api {
            let api_sri = cell_protocol::Sri::from_target(&api.cell_name)
                .map_err(|e| custom_err!("bridge cell_name '{}' invalid: {e}", api.cell_name))?;
            let task = spawn_bridge_command_consumer(
                &db,
                barrier.clone(),
                kill_signal.clone(),
                api_sri,
                self.mailbox_poll_interval,
                {
                    let db = db.clone();
                    let client = client.clone();

                    move |command| {
                        let db = db.clone();
                        let client = client.clone();
                        let api = api.clone();

                        handle_message(db, client, api, command)
                    }
                },
            );

            tasks.push(task);
        }

        let _ = timeout(BARRIER_TIMEOUT, barrier.wait())
            .await
            .map_err(|_| custom_err!("unable to start egress tasks"))?;

        if core::pin::pin!(kill_signal.notified()).enable() {
            Err(custom_err!(
                "there was an error attempting to init the bridge"
            ))?;
        }

        self.state = EgressState::Init(Inner {
            barrier,
            kill_signal,
            _egress_cells: tasks,
        });

        Ok(())
    }

    pub async fn run(&mut self) -> crate::Result<()> {
        let EgressState::Init(inner) = &mut self.state else {
            return Err(custom_err!("init must be called before run"))?;
        };
        let _ = timeout(BARRIER_TIMEOUT, inner.barrier.wait())
            .await
            .map_err(|_| custom_err!("internal error: egress barrier failed"))?;

        let () = inner.kill_signal.notified().await;

        Err(custom_err!("egress failed"))?
    }
}

#[allow(clippy::too_many_lines)] // It's called art
async fn handle_message(
    db: db_client::v1::Client,
    client: reqwest::Client,
    api: HttpBridgeApi,
    command: MailboxCommand,
) -> crate::Result<()> {
    let MailboxCommand {
        cmd,
        payload,
        attachment,
    } = command;

    tracing::debug!("Attempting to process: {}", cmd.as_ref());

    let Some(egress) = api.endpoints.iter().find(|t| t.id == cmd.as_ref()) else {
        return Err(custom_err!("unknown command: {}", cmd.as_ref()))?;
    };

    tracing::debug!("found egress for {}", cmd.as_ref());

    let payload = payload.unwrap_or_default();

    tracing::debug!("payload length: {}", payload.len());

    let req = egress.request.clone();
    let response_template = egress.response.clone();

    let mut obj = crate::payload::parse_payload_object(payload.as_slice())?;

    // The generated client rides its callback command name in a reserved
    // `__callback` field; strip it before matching request placeholders so it
    // isn't rejected as unknown. Other callers omit it and stay fire-and-forget.
    let callback = match obj.remove("__callback") {
        Some(serde_json::Value::String(name)) => Some(name),
        Some(_) => return Err(custom_err!("`__callback` must be a string"))?,
        None => None,
    };

    let vals = {
        let mut markers = Marker::collect(&req.path);
        markers.extend(req.query.values().flat_map(Marker::collect));
        markers.extend(req.headers.values().flat_map(Marker::collect));
        if let Some(body) = req.body.as_ref() {
            markers.push(Marker::http_body(body));
        }

        tracing::debug!("expecting {} fields", markers.len());

        crate::payload::decode_vals(markers, obj)?
    };

    tracing::debug!("decoded payload correctly");

    let method = reqwest::Method::from_str(&req.method)
        .map_err(|err| custom_err!("invalid http method `{}`: {}", req.method, err))?;

    let path = resolve_segments_as_string(&db, req.path, &vals).await?;

    let base = api.base_url.trim_end_matches('/');
    let url = if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    };

    let mut builder = client.request(method, &url);

    for (k, segs) in req.query {
        let q = resolve_segments_as_string(&db, segs, &vals).await?;
        builder = builder.query(&[(k, q)]);
    }

    // Set it _before_ the request's headers are set, _just_ in case the server takes the latest header. (which is typically the case)
    if let Some(ref body) = req.body {
        builder = match body {
            BodyTemplate::String(_) => builder.header(CONTENT_TYPE, "text/plain"),
            BodyTemplate::Json(_) => builder.header(CONTENT_TYPE, "application/json"),
            BodyTemplate::Bytes(_) => builder.header(CONTENT_TYPE, "application/octet-stream"),
        };
    }

    for (k, segs) in req.headers {
        let header = resolve_segments_as_string(&db, segs, &vals).await?;
        builder = builder.header(k, header);
    }

    if let Some(seg) = req.body {
        let body = get_val(&vals, crate::payload::HTTP_BODY_FIELD)?;
        builder = match seg {
            BodyTemplate::String(_) => {
                let payload = body
                    .into_string()
                    .map_err(|err| custom_err!("unable to convert payload to string: {}", err))?;

                builder.body(payload)
            }
            BodyTemplate::Json(_) => {
                let payload = body
                    .into_json()
                    .map_err(|err| custom_err!("unable to convert payload to json: {}", err))?;

                builder.body(payload.to_string())
            }
            BodyTemplate::Bytes(_) => {
                let payload = body
                    .into_bytes()
                    .map_err(|err| custom_err!("unable to convert payload to bytes: {}", err))?;

                builder.body(payload)
            }
        };
    }

    if let Some(ms) = req.timeout_ms {
        builder = builder.timeout(Duration::from_millis(ms));
    }

    let request = builder
        .build()
        .map_err(|err| custom_err!("unable to build request: {}", err))?;

    tracing::debug!("built request: {} {}", request.method(), request.url());

    let response = client
        .execute(request)
        .await
        .map_err(|err| custom_err!("unable to send request: {}", err))?;

    let status_code = response.status();
    tracing::debug!("response status code: {}", status_code.as_u16());

    if status_code.is_success() {
        tracing::debug!("request was sent successfully");
    } else {
        tracing::warn!(
            "request returned a non-200 result: {}",
            status_code.as_str()
        );
    }

    // Deliver the response to the caller's callback when one was supplied and we
    // know who to reply to; otherwise the command stays fire-and-forget.
    let (Some(callback), Some(sender)) = (callback, attachment.sender()) else {
        return Ok(());
    };

    let reply = build_reply(&response_template, status_code.as_u16(), response).await?;

    let command =
        Command::new(callback).map_err(|err| custom_err!("invalid callback name: {}", err))?;
    let mut message = OutgoingMessage::command(&Sri::from_uuid(sender), &command, Some(reply))
        .map_err(|err| custom_err!("unable to build reply command: {}", err))?;
    if let Ok(bridge) = Sri::from_target(&api.cell_name) {
        message.attach_sender(Some(bridge.as_uuid()));
    }
    message
        .send_via_db(&db, None)
        .await
        .map_err(|err| custom_err!("unable to deliver reply: {}", err))?;

    tracing::debug!("delivered response to callback `{}`", command.as_ref());

    Ok(())
}

/// Builds the JSON reply the caller's generated `<Endpoint>Reply` enum decodes,
/// from the response template and the live HTTP response.
///
/// The reply is the externally-tagged serde form of the variant matching `status`:
/// `{"<Variant>": <body>}` for a bodied status, `{"<Variant>": {<headers>, body}}`
/// when it surfaces headers, or the bare string `"<Variant>"` for a status with
/// neither. A status the template doesn't list becomes `{"Unknown": <status>}`.
/// Variant names come from [`status_variant_name`] — the same helper the codegen
/// uses — so the tags line up. JSON is used (not postcard) because the runtime has
/// no schema for a typed body; the cell deserialises it from the value.
async fn build_reply(
    template: &WireHttpResponseTemplate,
    status: u16,
    response: reqwest::Response,
) -> crate::Result<Vec<u8>> {
    let value = match template.get(&status) {
        Some(variant) => {
            let name = status_variant_name(status).map_err(|err| custom_err!("{err}"))?;
            match build_variant_value(variant, response).await? {
                Some(inner) => {
                    let mut obj = serde_json::Map::new();
                    obj.insert(name, inner);
                    serde_json::Value::Object(obj)
                }
                None => serde_json::Value::String(name),
            }
        }
        None => serde_json::json!({ "Unknown": status }),
    };

    let payload = serde_json::to_vec(&value)
        .map_err(|err| custom_err!("unable to serialise reply: {}", err))?;

    Ok(payload)
}

/// The inner value of a matched status variant, or `None` for a unit variant (no
/// headers, no body). A body-only status returns the body value directly (the
/// tuple variant's payload); a status with headers returns an object of the header
/// fields plus a `body` field when one is templated (the struct variant's fields).
async fn build_variant_value(
    variant: &WireHttpResponseVariant,
    response: reqwest::Response,
) -> crate::Result<Option<serde_json::Value>> {
    // Read headers (a borrow) before the body consumes the response.
    let mut headers = serde_json::Map::new();
    for (header, ResponseHeaderTemplate::String(name)) in &variant.headers {
        let value = response
            .headers()
            .get(header)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        headers.insert(name.clone(), serde_json::Value::String(value));
    }

    let body = match &variant.body {
        Some(body) => {
            let bytes = response
                .bytes()
                .await
                .map_err(|err| custom_err!("unable to read response body: {}", err))?;
            Some(match body {
                BodyTemplate::Json(_) => {
                    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
                }
                BodyTemplate::String(_) => {
                    serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
                }
                BodyTemplate::Bytes(_) => serde_json::Value::Array(
                    bytes.iter().map(|b| serde_json::Value::from(*b)).collect(),
                ),
            })
        }
        None => None,
    };

    if variant.headers.is_empty() {
        // Tuple variant (`Variant(body)`) or unit variant (`Variant`).
        Ok(body)
    } else {
        // Struct variant: header fields, plus `body` when the status has one.
        if let Some(body) = body {
            headers.insert("body".to_string(), body);
        }
        Ok(Some(serde_json::Value::Object(headers)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A response variant with `${string:name}` headers (`(http-header, field)`)
    /// and an optional body.
    fn variant(headers: &[(&str, &str)], body: Option<BodyTemplate>) -> WireHttpResponseVariant {
        WireHttpResponseVariant {
            headers: headers
                .iter()
                .map(|(k, name)| {
                    (
                        (*k).to_owned(),
                        ResponseHeaderTemplate::String((*name).to_owned()),
                    )
                })
                .collect(),
            body,
        }
    }

    fn http_response(status: u16, headers: &[(&str, &str)], body: &str) -> reqwest::Response {
        let mut builder = http::Response::builder().status(status);
        for (k, v) in headers {
            builder = builder.header(*k, *v);
        }
        reqwest::Response::from(builder.body(body.to_owned()).unwrap())
    }

    async fn reply(
        template: &WireHttpResponseTemplate,
        status: u16,
        headers: &[(&str, &str)],
        body: &str,
    ) -> serde_json::Value {
        let resp = http_response(status, headers, body);
        let bytes = build_reply(template, status, resp).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The reply JSON must be the externally-tagged serde form of the generated
    /// `<Endpoint>Reply` enum, so the guest's derived `Deserialize` decodes it.
    #[tokio::test]
    async fn reply_matches_generated_enum_repr() {
        let template: WireHttpResponseTemplate = BTreeMap::from([
            // 200 -> body-only tuple variant `Ok(body)`
            (
                200,
                variant(&[], Some(BodyTemplate::String("body".to_owned()))),
            ),
            // 201 -> struct variant `Created { location, body }`
            (
                201,
                variant(
                    &[("Location", "location")],
                    Some(BodyTemplate::String("body".to_owned())),
                ),
            ),
            // 204 -> unit variant `NoContent`
            (204, variant(&[], None)),
        ]);

        // Tuple variant: `{"Ok": <body>}`.
        assert_eq!(
            reply(&template, 200, &[], "hello").await,
            serde_json::json!({ "Ok": "hello" })
        );

        // Struct variant: `{"Created": {<headers>, "body": ...}}`.
        assert_eq!(
            reply(&template, 201, &[("Location", "/ships/7")], "made").await,
            serde_json::json!({ "Created": { "location": "/ships/7", "body": "made" } })
        );

        // Unit variant: the bare tag string `"NoContent"`.
        assert_eq!(
            reply(&template, 204, &[], "").await,
            serde_json::json!("NoContent")
        );

        // A status the template doesn't list -> `{"Unknown": <code>}`.
        assert_eq!(
            reply(&template, 503, &[], "boom").await,
            serde_json::json!({ "Unknown": 503 })
        );
    }
}
