//! Code generation for `import!`-ed bridge specs.
//!
//! Two entry points, one per bridge kind:
//!
//! * [`http_bridge`] — the request/response payload types come from the
//!   bridge's embedded **JSON Schema** and are generated with [`typify`]. Each
//!   endpoint becomes one fire-and-forget client method.
//! * [`mqtt_bridge`] — MQTT specs carry no schema; their payload types are
//!   still derived from the `${kind:name}` template placeholders, via the
//!   [`crate::codegen::cell_api`] model.
//!
//! Both emit a `<Name>Client` whose methods dispatch commands with
//! `myrmic_sdk::send`, plus the payload/event types those methods reference.
//!
//! [`typify`]: typify_impl

use std::collections::{BTreeMap, HashMap};

use heck::ToUpperCamelCase;
use proc_macro2::TokenStream as Ts;
use quote::{format_ident, quote};

use crate::codegen::bridge_api::{UserHttpBridgeApi, UserHttpEndpoint, UserMqttBridge};
use crate::codegen::cell_api::{ApiCommand, ApiEvent, ApiField, ApiType, CellApi};
use crate::codegen::status::status_variant_name;
use crate::codegen::template::{RawSeg, Seg, Segments};

mod schema;

/// Generates the client + payload/reply types for an HTTP bridge.
///
/// The bridge's `types` JSON Schema is run through `typify` to produce the Rust
/// payload types. Each endpoint becomes one client method whose positional
/// arguments are the endpoint's request placeholders (`${kind:name}` in the
/// path/query/headers, then the body), followed by a typed `Callback` naming
/// the handler the HTTP reply is delivered to. `db:` placeholders resolve
/// runtime-side and take no argument.
///
/// Each endpoint also gets a generated `<Endpoint>Reply` enum built from its
/// response template — one variant per listed status code plus `Unknown(u16)`:
/// it is the payload the callback handler decodes, and the `T` in `Callback<T>`.
///
/// The wire payload is a JSON object keyed by placeholder name (the runtime
/// decodes it by name, so field order is irrelevant) plus a reserved
/// `__callback` key carrying the callback command name; a private
/// `__<Endpoint>Payload` struct encodes exactly that.
pub fn http_bridge(root: &Ts, api: UserHttpBridgeApi) -> Result<Ts, String> {
    let UserHttpBridgeApi {
        name,
        base_url: _, // resolved on the runtime side, not the client
        types,
        endpoints,
    } = api;

    // Turn the JSON Schema into Rust types (and a name -> ident lookup so the
    // placeholders can refer to them).
    let (type_defs, type_idents) = match types {
        Some(schema) => schema::generate(root, schema)?,
        None => (Ts::new(), BTreeMap::new()),
    };

    let mut defs = Vec::new();
    let mut methods = Vec::new();
    for endpoint in &endpoints {
        let (reply_def, payload_def, method) = http_endpoint(root, endpoint, &type_idents)?;
        defs.push(reply_def);
        defs.push(payload_def);
        methods.push(method);
    }

    let client = client_struct(&name, &methods);

    Ok(quote! {
        #type_defs
        #(#defs)*
        #client
    })
}

/// Generates one endpoint's `<Endpoint>Reply` type, its private wire-payload
/// struct, and the client method that ties them together.
fn http_endpoint(
    root: &Ts,
    endpoint: &UserHttpEndpoint,
    type_idents: &BTreeMap<String, Ts>,
) -> Result<(Ts, Ts, Ts), String> {
    let camel = endpoint.id.to_upper_camel_case();
    let reply_ident = format_ident!("{camel}Reply");
    let payload_ident = format_ident!("__{camel}Payload");

    // A generated Reply must not clash with a schema type of the same name.
    if type_idents.contains_key(&format!("{camel}Reply")) {
        return Err(format!(
            "endpoint `{}` generates `{camel}Reply`, which collides with a `types` definition of the same name",
            endpoint.id
        ));
    }

    let params = request_params(endpoint)?;

    // `__callback` is the reserved wire key carrying the caller's callback command
    // name; a request placeholder of that name would collide with it.
    if params.iter().any(|(_, n)| n == "__callback") {
        return Err(format!(
            "endpoint `{}` has a request placeholder named `__callback`, which is reserved",
            endpoint.id
        ));
    }

    let mut sig = Vec::new();
    let mut field_decls = Vec::new();
    let mut inits = Vec::new();
    for (kind, name) in &params {
        let (s, f, i) = request_param_tokens(root, kind, name, type_idents);
        sig.push(s);
        field_decls.push(f);
        inits.push(i);
    }
    if let Some(body) = &endpoint.request.body
        && let Some((kind, name)) = body.0.split()
    {
        if params.iter().any(|(_, n)| n == "body") {
            return Err(format!(
                "endpoint `{}` has both a `body` and a request parameter named `body`",
                endpoint.id
            ));
        }
        let (s, f, i) = body_param_tokens(root, kind, name, type_idents);
        sig.push(s);
        field_decls.push(f);
        inits.push(i);
    }

    let reply_def = reply_enum(root, &reply_ident, endpoint, type_idents)?;

    let serde_crate = format!("{root}::codegen::exports::serde");

    // Private: only this module's methods build it. JSON-encoding the struct (the
    // `Message` default) is the wire object the runtime's `decode_vals` reads back
    // by field name; the reserved `__callback` key carries the caller's callback
    // command name, which the runtime strips before matching request placeholders.
    let payload_def = quote! {
        #[derive(
            #root::codegen::exports::serde::Serialize,
            #root::codegen::exports::serde::Deserialize,
            #root::Message,
        )]
        #[serde(crate = #serde_crate)]
        struct #payload_ident {
            #(#field_decls,)*
            __callback: #root::String,
        }
    };

    let method_ident = format_ident!("{}", endpoint.id);
    let command_name = &endpoint.id;
    let method = quote! {
        pub fn #method_ident(&self, #(#sig,)* cb: #root::Callback<#reply_ident>) -> #root::Result<()> {
            let sri = #root::Sri::from_target(self.target)
                .map_err(|_| "invalid bridge target")?;
            let __cb: #root::Command = cb.into();
            let __payload = #payload_ident {
                #(#inits,)*
                __callback: __cb.as_ref().into(),
            };
            #root::send(sri, #command_name, &__payload)
        }
    };

    Ok((reply_def, payload_def, method))
}

/// The request placeholders as `(kind, name)` in the runtime's decode order
/// (path, then query, then headers), rejecting a name used more than once
/// (the wire expects one value per placeholder occurrence). `db:`/raw segments
/// carry no value and are skipped.
fn request_params(endpoint: &UserHttpEndpoint) -> Result<Vec<(String, String)>, String> {
    let req = &endpoint.request;
    let mut params: Vec<(String, String)> = Vec::new();
    let mut push = |kind: &str, name: &str| -> Result<(), String> {
        if params.iter().any(|(_, n)| n == name) {
            return Err(format!(
                "request placeholder `{name}` appears more than once in endpoint `{}`; each parameter must be unique",
                endpoint.id
            ));
        }
        params.push((kind.to_string(), name.to_string()));
        Ok(())
    };
    for seg in req.path.0.iter() {
        if let Some((kind, name)) = seg.split() {
            push(kind, name)?;
        }
    }
    for segs in req.query.values() {
        for seg in segs.0.iter() {
            if let Some((kind, name)) = seg.split() {
                push(kind, name)?;
            }
        }
    }
    for segs in req.headers.values() {
        for seg in segs.0.iter() {
            if let Some((kind, name)) = seg.split() {
                push(kind, name)?;
            }
        }
    }
    Ok(params)
}

/// A request placeholder's `(signature arg, payload field, payload init)`
/// tokens. `json` args are handed by reference and stored as a nested JSON value
/// (`to_value`) so the runtime reads them back as an object; scalars are taken by
/// value, `string` by `&str`.
fn request_param_tokens(
    root: &Ts,
    kind: &str,
    name: &str,
    type_idents: &BTreeMap<String, Ts>,
) -> (Ts, Ts, Ts) {
    let ident = format_ident!("{name}");
    match kind {
        "bool" => scalar(&ident, &quote! { bool }),
        "u8" => scalar(&ident, &quote! { u8 }),
        "u16" => scalar(&ident, &quote! { u16 }),
        "u32" => scalar(&ident, &quote! { u32 }),
        "u64" => scalar(&ident, &quote! { u64 }),
        "i8" => scalar(&ident, &quote! { i8 }),
        "i16" => scalar(&ident, &quote! { i16 }),
        "i32" => scalar(&ident, &quote! { i32 }),
        "i64" => scalar(&ident, &quote! { i64 }),
        "f32" => scalar(&ident, &quote! { f32 }),
        "f64" => scalar(&ident, &quote! { f64 }),
        "json" => {
            let jt = json_ref_type(root, name, type_idents);
            (
                quote! { #ident: &#jt },
                quote! { #ident: #root::JsonValue },
                quote! { #ident: #root::codegen::exports::serde_json::to_value(#ident)
                .map_err(|_| "unable to serialise json argument")? },
            )
        }
        // "string" and anything unknown fall back to the SDK string type.
        _ => (
            quote! { #ident: &str },
            quote! { #ident: #root::String },
            quote! { #ident: #ident.into() },
        ),
    }
}

/// The body placeholder's `(signature arg, payload field, payload init)`, always
/// named `body`. `json` is stored as a nested JSON value, `bytes` as a byte vector.
fn body_param_tokens(
    root: &Ts,
    kind: &str,
    name: &str,
    type_idents: &BTreeMap<String, Ts>,
) -> (Ts, Ts, Ts) {
    match kind {
        "json" => {
            let jt = json_ref_type(root, name, type_idents);
            (
                quote! { body: &#jt },
                quote! { body: #root::JsonValue },
                quote! { body: #root::codegen::exports::serde_json::to_value(body)
                .map_err(|_| "unable to serialise json body")? },
            )
        }
        "bytes" => (
            quote! { body: &[u8] },
            quote! { body: #root::Vec<u8> },
            quote! { body: body.to_vec() },
        ),
        _ => (
            quote! { body: &str },
            quote! { body: #root::String },
            quote! { body: body.into() },
        ),
    }
}

/// A scalar arg is taken and stored by value; its payload init is a field
/// shorthand.
fn scalar(ident: &syn::Ident, ty: &Ts) -> (Ts, Ts, Ts) {
    (
        quote! { #ident: #ty },
        quote! { #ident: #ty },
        quote! { #ident },
    )
}

/// The `<Endpoint>Reply` enum, built from the response template: one variant per
/// listed status code (named by its canonical reason — `200` -> `Ok`, `404` ->
/// `NotFound`), plus `Unknown(u16)` for any status the bridge sees that the spec
/// doesn't list.
///
/// A status entry that is only a body becomes a tuple variant carrying that body;
/// one with headers becomes a struct variant (a `String` field per header, plus a
/// `body` field when a body is templated); one with neither becomes a unit variant.
///
/// JSON codec (the `Message` default): the runtime builds the reply from the
/// response template without a schema, emitting the externally-tagged serde form
/// of the matching variant; a typed `body` then deserialises from the nested value.
fn reply_enum(
    root: &Ts,
    reply_ident: &syn::Ident,
    endpoint: &UserHttpEndpoint,
    type_idents: &BTreeMap<String, Ts>,
) -> Result<Ts, String> {
    let mut variants = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    for (code, variant) in &endpoint.response {
        let name = status_variant_name(*code)
            .map_err(|err| format!("endpoint `{}`: {err}", endpoint.id))?;
        if seen.iter().any(|s| s == &name) {
            return Err(format!(
                "endpoint `{}` lists two statuses that map to variant `{name}`",
                endpoint.id
            ));
        }
        seen.push(name.clone());
        let ident = format_ident!("{name}");
        let variant = &variant.0; // unwrap the `ParseInto` shorthand wrapper

        let mut header_fields = Vec::new();
        let mut header_names: Vec<String> = Vec::new();
        for hdr in variant.headers.values() {
            if let Some((_, hname)) = hdr.0.split() {
                if header_names.iter().any(|n| n == hname) {
                    return Err(format!(
                        "endpoint `{}` status `{code}` uses response placeholder `{hname}` more than once",
                        endpoint.id
                    ));
                }
                header_names.push(hname.to_string());
                let field = format_ident!("{hname}");
                header_fields.push(quote! { #field: #root::String });
            }
        }

        let body_ty =
            variant
                .body
                .as_ref()
                .and_then(|b| b.0.split())
                .map(|(kind, bname)| match kind {
                    "json" => json_ref_type(root, bname, type_idents),
                    "bytes" => quote! { #root::Vec<u8> },
                    _ => quote! { #root::String },
                });

        if !header_fields.is_empty() {
            if let Some(body_ty) = body_ty {
                header_fields.push(quote! { body: #body_ty });
            }
            variants.push(quote! { #ident { #(#header_fields),* } });
        } else if let Some(body_ty) = body_ty {
            variants.push(quote! { #ident(#body_ty) });
        } else {
            variants.push(quote! { #ident });
        }
    }

    variants.push(quote! { Unknown(u16) });

    let serde_crate = format!("{root}::codegen::exports::serde");
    Ok(quote! {
        #[derive(
            Debug,
            Clone,
            #root::codegen::exports::serde::Serialize,
            #root::codegen::exports::serde::Deserialize,
            #root::Message,
        )]
        #[serde(crate = #serde_crate)]
        pub enum #reply_ident {
            #(#variants,)*
        }
    })
}

/// A `json` placeholder's Rust type: the schema definition it names, or an
/// untyped `myrmic_sdk::JsonValue` when no such definition exists.
fn json_ref_type(root: &Ts, name: &str, type_idents: &BTreeMap<String, Ts>) -> Ts {
    type_idents
        .get(name)
        .cloned()
        .unwrap_or_else(|| quote! { #root::JsonValue })
}

/// Generates the client + payload/event types for an MQTT bridge.
///
/// MQTT egresses become fire-and-forget `<id>` client methods; ingresses
/// become event payload types (implementing `myrmic_sdk::CellEvent`).
pub fn mqtt_bridge(root: &Ts, api: UserMqttBridge) -> Result<Ts, String> {
    let cell_api = convert_mqtt(api)?;
    Ok(cell_api_tokens(root, &cell_api))
}

// ---------------------------------------------------------------------------
// Shared client / method generation
// ---------------------------------------------------------------------------

/// Emits a single fire-and-forget client method that sends the command named
/// `name` to the bridge's target SRI, optionally carrying a typed payload.
/// `by_value` selects the payload calling convention: `false` takes `&Ty`
/// (HTTP), `true` takes `Ty` by value (MQTT).
fn command_method(root: &Ts, name: &str, arg_ty: Option<&Ts>, by_value: bool) -> Ts {
    let method = format_ident!("{}", name);
    let command_name = name;

    let resolve_sri = quote! {
        let sri = #root::Sri::from_target(self.target)
            .map_err(|_| "invalid bridge target")?;
    };

    match arg_ty {
        Some(ty) if by_value => quote! {
            pub fn #method(&self, value: #ty) -> #root::Result<()> {
                #resolve_sri
                #root::send(sri, #command_name, &value)
            }
        },
        Some(ty) => quote! {
            pub fn #method(&self, value: &#ty) -> #root::Result<()> {
                #resolve_sri
                #root::send(sri, #command_name, value)
            }
        },
        None => quote! {
            pub fn #method(&self) -> #root::Result<()> {
                #resolve_sri
                #root::send(sri, #command_name, &#root::Void)
            }
        },
    }
}

/// Wraps the generated methods in a `<Name>Client` that stores the bridge's
/// target (an SRI or resolvable SRN string).
fn client_struct(bridge_name: &str, methods: &[Ts]) -> Ts {
    let client = format_ident!("{}Client", bridge_name.to_upper_camel_case());
    quote! {
        pub struct #client {
            target: &'static str,
        }

        impl #client {
            /// Binds the client to a bridge cell, named by SRI or SRN string.
            pub const fn new(target: &'static str) -> Self {
                Self { target }
            }

            #(#methods)*
        }
    }
}

// ---------------------------------------------------------------------------
// MQTT: template-placeholder -> cell_api -> tokens
// ---------------------------------------------------------------------------

/// Generates the struct/event types and client for a `CellApi` (MQTT path).
fn cell_api_tokens(root: &Ts, api: &CellApi) -> Ts {
    let mut type_defs = Vec::new();
    if let Some(types) = &api.types {
        for (name, ty) in types {
            type_defs.push(struct_def(root, name, ty, /* event */ None));
        }
    }

    let mut event_defs = Vec::new();
    for (name, event) in &api.events {
        event_defs.push(struct_def(root, name, event.as_ref(), Some(name)));
    }

    let mut methods = Vec::new();
    for (name, cmd) in &api.commands {
        methods.extend(command_methods(root, name, cmd));
    }

    let client = client_struct(&api.cell, &methods);

    quote! {
        #(#type_defs)*
        #(#event_defs)*
        #client
    }
}

/// A struct type from an [`ApiType`]. When `event_name` is `Some`, also emits a
/// `myrmic_sdk::CellEvent` impl so the type can be used as an event payload.
///
/// serde is reached through `myrmic_sdk::codegen::exports` (both the derive path and
/// the `#[serde(crate = …)]` container attr) so the cell needs no direct serde
/// dependency — mirroring what the typify path does for schema types.
fn struct_def(root: &Ts, name: &str, ty: &ApiType, event_name: Option<&str>) -> Ts {
    let ident = format_ident!("{}", name);
    let serde_crate = format!("{root}::codegen::exports::serde");

    let fields = ty.fields.iter().map(|f| {
        let field_ident = format_ident!("{}", f.name);
        let field_ty: syn::Type =
            syn::parse_str(&f.field_type).unwrap_or_else(|_| syn::parse_quote!(#root::String));
        quote! { pub #field_ident: #field_ty }
    });

    let event_impl = event_name.map(|ev| {
        quote! {
            impl #root::CellEvent for #ident {
                fn event_name() -> &'static str { #ev }
            }
        }
    });

    quote! {
        #[derive(
            Debug,
            Clone,
            #root::codegen::exports::serde::Serialize,
            #root::codegen::exports::serde::Deserialize,
            #root::Message,
        )]
        #[serde(crate = #serde_crate)]
        pub struct #ident {
            #(#fields,)*
        }
        #event_impl
    }
}

/// Generates the client method for one command. Commands are fire-and-forget,
/// so there is a single dispatch shape, named after the command itself.
fn command_methods(root: &Ts, name: &str, cmd: &ApiCommand) -> Vec<Ts> {
    let arg_ty = cmd
        .args
        .as_deref()
        .filter(|s| *s != "None")
        .map(|s| format_ident!("{}", s));

    // MQTT call sites pass the payload by value.
    let arg = arg_ty.as_ref().map(|ty| quote! { #ty });
    let method = command_method(root, name, arg.as_ref(), /* by_value */ true);

    vec![method]
}

/// Converts a `UserMqttBridge` into a `CellApi`: egresses -> commands (+ arg
/// types), ingresses -> events. Ported from the original `import!` macro.
fn convert_mqtt(api: UserMqttBridge) -> Result<CellApi, String> {
    let UserMqttBridge {
        name,
        broker_url: _,
        ingress,
        egress,
    } = api;

    let name = name.to_upper_camel_case();

    let mut types: HashMap<String, ApiType> = HashMap::new();
    let mut events: HashMap<String, ApiEvent> = HashMap::new();
    let mut commands: HashMap<String, ApiCommand> = HashMap::new();

    for egress in egress {
        let cmd_name = egress.id.to_upper_camel_case();

        let mut fields = FieldCollector::default();
        fields.scan_segments(&egress.topic.0)?;
        fields.push(&egress.payload.0)?;

        types.insert(
            cmd_name.clone(),
            ApiType {
                description: None,
                fields: fields.into_fields(),
            },
        );

        commands.insert(
            egress.id,
            ApiCommand {
                description: None,
                args: Some(cmd_name),
            },
        );
    }

    for ingress in ingress {
        let event_name = ingress.id.to_upper_camel_case();

        let mut fields = FieldCollector::default();
        fields.push(&ingress.payload.0)?;

        events.insert(
            event_name,
            ApiEvent(ApiType {
                description: None,
                fields: fields.into_fields(),
            }),
        );
    }

    Ok(CellApi {
        cell: name,
        types: if types.is_empty() { None } else { Some(types) },
        commands,
        events,
    })
}

/// Collects distinct `${kind:name}` placeholders into typed fields.
#[derive(Default)]
struct FieldCollector {
    fields: Vec<ApiField>,
    seen: HashMap<String, String>,
}

impl FieldCollector {
    fn scan_segments<S: RawSeg>(&mut self, segments: &Segments<S>) -> Result<(), String> {
        for seg in segments.iter() {
            self.push(seg)?;
        }
        Ok(())
    }

    fn push<S: Seg>(&mut self, seg: &S) -> Result<(), String> {
        let Some((ty, name)) = seg.split() else {
            return Ok(());
        };
        if ty == "db" {
            return Ok(());
        }

        if let Some(prev) = self.seen.insert(name.to_string(), ty.to_string())
            && prev != ty
        {
            return Err(format!("placeholder `{name}` used with conflicting types"));
        }

        self.fields.push(ApiField {
            name: name.to_string(),
            serde_with: None,
            field_type: map_placeholder_type(ty).to_string(),
            description: None,
        });

        Ok(())
    }

    fn into_fields(self) -> Vec<ApiField> {
        self.fields
    }
}

fn map_placeholder_type(ty: &str) -> &'static str {
    match ty {
        "bool" => "bool",
        "u8" => "u8",
        "u16" => "u16",
        "u32" => "u32",
        "u64" => "u64",
        "i8" => "i8",
        "i16" => "i16",
        "i32" => "i32",
        "i64" => "i64",
        "f32" => "f32",
        "f64" => "f64",
        "bytes" => "::myrmic_sdk::Bytes",
        "json" => "::myrmic_sdk::JsonValue",
        // "string" and anything unknown fall back to the SDK string type.
        _ => "::myrmic_sdk::String",
    }
}

#[cfg(test)]
mod tests;
