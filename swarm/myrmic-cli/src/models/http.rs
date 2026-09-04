use sorg_common::HttpBridgeApi;

use myrmic_common::types::web::{Scheme, Url};

pub use myrmic_common::codegen::bridge_api::{
    UserHttpBridgeApi, UserHttpEndpoint, WireHttpEndpoint, WireHttpRequestTemplate,
    WireHttpResponseTemplate, WireHttpResponseVariant,
};

pub fn convert(cell_name: String, api: UserHttpBridgeApi) -> anyhow::Result<HttpBridgeApi> {
    let UserHttpBridgeApi {
        name: _,
        base_url,
        types: _,
        endpoints,
    } = api;

    let url = Url::parse(&base_url)
        .map_err(|err| anyhow::anyhow!("unable to parse `base_url`: {}", err.to_text()))?;

    if !matches!(url.scheme(), Scheme::Http | Scheme::Https) {
        anyhow::bail!("`base_url` only supports http(s)");
    }

    let mut converted = vec![];

    for endpoint in endpoints {
        let UserHttpEndpoint {
            id,
            request,
            response,
        } = endpoint;

        let request = WireHttpRequestTemplate {
            method: request.method,
            path: request.path.0,
            query: { request.query.into_iter().map(|(k, v)| (k, v.0)).collect() },
            headers: { request.headers.into_iter().map(|(k, v)| (k, v.0)).collect() },
            body: request.body.map(|b| b.0),
            timeout_ms: request.timeout_ms,
        };

        let response: WireHttpResponseTemplate = response
            .into_iter()
            .map(|(code, variant)| {
                let variant = variant.0; // unwrap the `ParseInto` shorthand wrapper
                let wire = WireHttpResponseVariant {
                    headers: variant.headers.into_iter().map(|(k, v)| (k, v.0)).collect(),
                    body: variant.body.map(|b| b.0),
                };
                (code, wire)
            })
            .collect();

        converted.push(WireHttpEndpoint {
            id,
            request,
            response,
        });
    }

    Ok(HttpBridgeApi {
        cell_name,
        base_url,
        endpoints: converted,
    })
}
