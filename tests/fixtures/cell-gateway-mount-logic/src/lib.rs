//! Cell that declares gateway resources in `#[init]`: one asset in its own
//! store and a mount serving it. The orchestration tests check both are
//! released when the cell goes away, and drive each kind of reply through a
//! real gateway.

#![no_std]

use myrmic_sdk::gateway::{self, Asset, HttpRequest, HttpResponse, StatusCode, WsEvent};
use myrmic_sdk::{Callback, Metadata, Result};

/// Mirrored by the orchestration gateway tests.
const MOUNT: &str = "/gateway-mount-test";
const INDEX: &str = "/index.html";

#[myrmic_sdk::init]
fn init(md: Metadata) -> Result<()> {
    gateway::assets(md.id).put(INDEX, b"<h1>gateway mount test</h1>")?;
    gateway::mount(MOUNT, Callback::of::<on_http>())?;
    Ok(())
}

/// `/echo` answers with the request body, `/ws` is an echo socket, and
/// everything else is an asset.
#[myrmic_sdk::cmd]
fn on_http(_md: Metadata, req: HttpRequest) -> Result<()> {
    match req.path.as_str() {
        "/echo" => {
            let response = HttpResponse::new(StatusCode::OK).body(req.body.clone());
            req.respond(response)
        }
        "/ws" => req.upgrade(Callback::of::<on_ws>()),
        _ => {
            let asset = Asset::new(req.path.clone()).index(INDEX);
            req.asset(asset)
        }
    }
}

#[myrmic_sdk::cmd]
fn on_ws(_md: Metadata, event: WsEvent) -> Result<()> {
    match event {
        WsEvent::Message(socket, message) => socket.send(message),
        WsEvent::Open(_) | WsEvent::Closed(_) => Ok(()),
    }
}
