use anyhow::{Context as _, bail};

#[cfg(test)]
mod tests;

/// Encode a `send`/`publish` payload for the wire.
///
/// Default (`raw == false`): parse the literal as JSON and re-serialise it. A
/// value that isn't valid JSON is sent as a JSON string, so `hello` becomes
/// `"hello"` on the wire. This matches the JSON codec that `#[derive(Message)]`
/// payload types use by default.
///
/// `raw == true`: decode the literal as a hex string (optional `0x` prefix) and
/// send those bytes as-is — no JSON, for handlers on a non-JSON wire format.
pub(super) fn encode(payload: String, raw: bool) -> anyhow::Result<Vec<u8>> {
    if raw {
        return decode_hex(&payload);
    }

    // A value that isn't valid JSON is wrapped as a JSON string.
    let value = match serde_json::from_str::<serde_json::Value>(&payload) {
        Ok(value) => value,
        Err(_) => serde_json::Value::String(payload),
    };
    serde_json::to_vec(&value).context("unable to serialise json payload")
}

/// Decode a hex string into raw bytes. An optional `0x`/`0X` prefix and
/// surrounding whitespace are stripped first.
fn decode_hex(payload: &str) -> anyhow::Result<Vec<u8>> {
    let trimmed = payload.trim();
    let hex = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed)
        .as_bytes();
    if !hex.len().is_multiple_of(2) {
        bail!(
            "--raw payload must be a hex string with an even number of digits; got {}",
            hex.len()
        );
    }
    hex.chunks_exact(2)
        .map(|pair| match (hex_digit(pair[0]), hex_digit(pair[1])) {
            (Some(hi), Some(lo)) => Ok((hi << 4) | lo),
            _ => Err(anyhow::anyhow!("invalid hex in --raw payload: {payload:?}")),
        })
        .collect()
}

/// Value of a single ASCII hex digit, or `None` if the byte isn't one.
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
