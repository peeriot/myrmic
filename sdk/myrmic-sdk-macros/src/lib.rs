//! Proc-macro companion crate for [`myrmic-sdk`](https://crates.io/crates/myrmic-sdk),
//! the SDK for writing Myrmic cells. It supplies the attribute and derive
//! macros a cell's crate applies to its own functions and message types:
//!
//! - [`cmd`] and [`evt`] turn a free function into a command or event export.
//! - [`macro@init`] turns a function into the cell's `init_cell` export.
//! - [`monitor`] turns a function into the cell's `on_cell_lost` export.
//! - [`Message`] derives the `Decoder`/`Encoder` impls a command, event, or
//!   init payload type needs.
//! - `import!` reads a bridge spec file (MQTT or HTTP) and generates a typed
//!   client for it.
//!
//! `myrmic-sdk` re-exports every one of these under its own name, so a cell
//! depends on `myrmic-sdk` rather than on this crate directly:
//!
//! ```ignore
//! #[myrmic_sdk::cmd]
//! fn ping(md: myrmic_sdk::Metadata) -> myrmic_sdk::Result<()> {
//!     myrmic_sdk::info!("ping from {}", md.sender);
//!     Ok(())
//! }
//! ```

use quote::quote;

mod handler;
mod import;
mod init;
mod inputs;
mod message;

/// Turns a free function into a Wasm command export.
///
/// The function takes a leading `myrmic_sdk::Metadata` argument - the invocation
/// context (the cell's own identity and the sender's) - optionally followed by
/// a `Decoder` for the payload: a message type deriving `myrmic_sdk::Message`,
/// or `myrmic_sdk::Bytes` for a raw payload. When the payload argument is omitted
/// it defaults to `myrmic_sdk::Void`, which rejects any non-empty payload.
/// The macro emits a `command_<name>` FFI export that recombines the identity
/// halves the host passes into a `Metadata`, decodes the argument buffer via the
/// `Decoder` impl, and forwards both:
///
/// ```ignore
/// #[myrmic_sdk::cmd]
/// fn recv_message(md: myrmic_sdk::Metadata, msg: ServerMessage) -> myrmic_sdk::Result<()> {
///     myrmic_sdk::info!("from {}: {msg:?}", md.sender);
///     Ok(())
/// }
///
/// #[myrmic_sdk::cmd] // no payload - rejects any argument bytes
/// fn ping(md: myrmic_sdk::Metadata) -> myrmic_sdk::Result<()> {
///     myrmic_sdk::info!("ping from {}", md.sender);
///     Ok(())
/// }
/// ```
///
/// A `name = "..."` argument overrides the command name the export advertises,
/// decoupling it from the Rust function's identifier:
///
/// ```ignore
/// #[myrmic_sdk::cmd(name = "recv")]
/// fn recv_message(md: myrmic_sdk::Metadata) -> myrmic_sdk::Result<()> { Ok(()) }
/// ```
#[proc_macro_attribute]
pub fn cmd(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let root = quote! {::myrmic_sdk};
    handler::handler_impl(attr.into(), item.into(), &root, handler::Kind::Command).into()
}

/// Turns a free function into a Wasm event-handler export.
///
/// Identical in shape to [`cmd`], but emits an `event_<name>` export. The
/// function name selects the event subscribed to. It takes a leading
/// `myrmic_sdk::Metadata` carrying the publisher's identity, optionally followed
/// by a `Decoder` for the event payload (a message type deriving
/// `myrmic_sdk::Message`). When the payload argument is omitted it defaults to
/// `myrmic_sdk::Void`, which rejects any non-empty payload.
///
/// Unlike [`cmd`], `#[evt]` generates no `Callback` marker type: events are
/// pub/sub and can never be callback targets.
///
/// ```ignore
/// #[myrmic_sdk::evt]
/// fn temperature_changed(md: myrmic_sdk::Metadata, ev: TemperatureChanged) -> myrmic_sdk::Result<()> {
///     myrmic_sdk::info!("{} published {ev:?}", md.sender);
///     Ok(())
/// }
/// ```
///
/// Note: the standalone macros do not yet register the subscription - a cell
/// must call `myrmic_sdk::subscribe_to_event` itself (e.g. from `#[init]`).
///
/// A `name = "..."` argument overrides the event the handler subscribes to,
/// decoupling it from the Rust function's identifier:
///
/// ```ignore
/// #[myrmic_sdk::evt(name = "TemperatureChanged")]
/// fn temperature_changed(md: myrmic_sdk::Metadata, ev: TemperatureChanged) -> myrmic_sdk::Result<()> {
///     Ok(())
/// }
/// ```
#[proc_macro_attribute]
pub fn evt(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let root = quote! {::myrmic_sdk};
    handler::handler_impl(attr.into(), item.into(), &root, handler::Kind::Event).into()
}

/// Turns a free function into the `init_cell` Wasm export, run once per
/// incarnation: on first deploy, and again each time the cell is respawned or
/// restarted under the same SRI. Private db state survives incarnations, so
/// after a restart init runs over the previous life's data - seed it
/// idempotently rather than assuming a blank slate. Volatile host resources
/// (timers) die with the incarnation and are re-established here.
///
/// The function takes a leading `myrmic_sdk::Metadata` argument - carrying the
/// cell's own identity and the spawner's - optionally followed by a `Decoder`
/// for the spawn payload, and returns `myrmic_sdk::Result<_>`. It performs setup
/// only - durable state belongs in the data layer.
///
/// ```ignore
/// #[myrmic_sdk::init]
/// fn setup(md: myrmic_sdk::Metadata) -> myrmic_sdk::Result<()> {
///     myrmic_sdk::info!("initialising {}", md.id);
///     Ok(())
/// }
/// ```
#[proc_macro_attribute]
pub fn init(
    _attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let root = quote! {::myrmic_sdk};
    init::init_impl(item.into(), &root).into()
}

/// Binds a wire codec to a message type: generates `Decoder` + `Encoder`
/// (`myrmic_sdk`) that delegate to the `myrmic_sdk::Codec` named in an
/// optional `#[codec(...)]` attribute, defaulting to `myrmic_sdk::Json`
/// when omitted. The type should also derive `serde::{Serialize, Deserialize}`.
///
/// ```
/// #[derive(serde::Serialize, serde::Deserialize, myrmic_sdk::Message)]
/// #[codec(myrmic_sdk::Postcard)]   // any Codec type - built-in or your own; omit for Json
/// struct ServerMessage { /* ... */ }
/// #
/// # use myrmic_sdk::{Decoder, Encoder};
/// # let bytes = ServerMessage {}.to_bytes().unwrap();
/// # ServerMessage::from_bytes(bytes).unwrap();
/// ```
#[proc_macro_derive(Message, attributes(codec))]
pub fn derive_message(item: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let root = quote! {::myrmic_sdk};
    message::derive_message(item.into(), &root).into()
}

/// Generates a typed client for an external system from a YAML bridge spec.
///
/// Takes one or more string-literal paths, resolved relative to the crate's
/// `CARGO_MANIFEST_DIR`. Each file describes a *bridge* - a runtime-provided
/// cell fronting an HTTP API or an MQTT broker - and expands to everything
/// needed to talk to it type-safely:
///
/// - Rust structs for the payload types the spec declares, deriving
///   `serde::Serialize`/`Deserialize` and `myrmic_sdk::Message`. The derives
///   resolve through `myrmic_sdk`'s re-exports, so the cell crate needs no
///   direct `serde` dependency.
/// - A `<Name>Client` struct (the spec's `name`, converted to `UpperCamelCase`) with a
///   `const fn new(target: &'static str)` binding it to the bridge cell by
///   SRI or resolvable SRN string.
/// - One method per interaction:
///   - **HTTP endpoint** - `fn <id>(&self, <params...>, cb: Callback<<Id>Reply>) -> Result<()>`.
///     Path, query, and header templates may contain placeholders; each
///     becomes a typed parameter of the method. The bridge performs the
///     request and invokes the callback with `<Id>Reply`, an enum with one
///     variant per declared response status (named by its canonical reason,
///     e.g. `200` -> `Ok`) plus an `Unknown(u16)` catch-all.
///   - **MQTT egress** - `fn <id>(&self, value: <Id>) -> Result<()>`,
///     publishing the payload to the entry's topic.
///   - **MQTT ingress** - no method; each entry becomes an event payload type
///     implementing `CellEvent`, so an `#[evt]` handler receives messages
///     arriving from the broker.
///
/// The spec file is registered as a build input, so editing it re-runs the
/// code generation.
///
/// # Example
///
/// A spec `tracking.yml` next to the cell's `Cargo.toml`:
///
/// ```yaml
/// name: tracking
/// base_url: http://localhost:8080
/// endpoints:
///   - id: ping
///     request:
///       method: GET
///       path: /ping
///     response:
///       200: {}
/// ```
///
/// generates `TrackingClient` and `PingReply`; replies arrive on whichever
/// command handler the caller names in the `Callback`:
///
/// ```ignore
/// myrmic_sdk::import!("tracking.yml");
///
/// const TRACKING: TrackingClient = TrackingClient::new("bridge.tracking");
///
/// #[myrmic_sdk::cmd]
/// fn check(_md: myrmic_sdk::Metadata) -> myrmic_sdk::Result<()> {
///     TRACKING.ping(myrmic_sdk::Callback::of::<pong>())
/// }
///
/// #[myrmic_sdk::cmd]
/// fn pong(_md: myrmic_sdk::Metadata, reply: PingReply) -> myrmic_sdk::Result<()> {
///     match reply {
///         PingReply::Ok => myrmic_sdk::info!("bridge is up")?,
///         PingReply::Unknown(code) => myrmic_sdk::warn!("unexpected status {code}")?,
///     }
///     Ok(())
/// }
/// ```
///
/// Response bodies use the `${json:Type}` shorthand (e.g.
/// `200: "${json:Pong}"`) against type definitions from the spec's `types`
/// section, which is a JSON-Schema document.
#[proc_macro]
pub fn import(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let root = quote! {::myrmic_sdk};
    import::import_misc(input.into(), &root).into()
}

/// Turns a free function into the cell's `on_cell_lost` export - the reserved
/// handler the runtime invokes when one of this cell's children dies (crash,
/// deliberate stop, termination, or node loss).
///
/// Same shape as [`cmd`]: a leading `myrmic_sdk::Metadata`, then the
/// notification payload - `myrmic_sdk::monitor::CellLost`, carrying the
/// child's SRI, its spawn-time local name, and the reason it died. At most one
/// per cell; the export name is fixed and cannot be overridden.
///
/// ```ignore
/// use myrmic_sdk::monitor::{CellLost, LostReason};
///
/// #[myrmic_sdk::monitor]
/// fn lost(md: myrmic_sdk::Metadata, l: CellLost) -> myrmic_sdk::Result<()> {
///     myrmic_sdk::info!("child {} died: {:?}", l.child, l.reason);
///     Ok(())
/// }
/// ```
#[proc_macro_attribute]
pub fn monitor(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let root = quote! {::myrmic_sdk};
    handler::handler_impl(attr.into(), item.into(), &root, handler::Kind::Monitor).into()
}
