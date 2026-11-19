//! Module defining types representing concepts from the MQTT domain.

use core::net::Ipv6Addr;

use myrmic_common::types::web::{Scheme, Url};
use serde::{Deserialize, Serialize};

use crate::{Result, bail_validation, validation_err};

pub use myrmic_common::codegen::bridge_api::Qos;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct MqttConnection {
    pub broker_address: BrokerAddress,
    pub broker_port: u16,
    pub keep_alive_period: core::time::Duration,
    pub client_id: String,
    pub channel_cap: u16,
    /// Connect over TLS, verifying the broker certificate and host name; no plaintext fallback.
    pub tls: bool,
}

/// A parsed `mqtt://` or `mqtts://` broker URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerEndpoint {
    pub address: BrokerAddress,
    pub port: u16,
    pub tls: bool,
}

/// Parses a `mqtt://` or `mqtts://` broker URL (scheme case-insensitive, `IPv6` hosts in
/// brackets). The CLI and the deploy both use this, so they accept the same URLs.
pub fn parse_broker_url(url: &str) -> Result<BrokerEndpoint> {
    // The messages below never echo the url: it may carry credentials or a token.
    if url.chars().any(char::is_whitespace) {
        bail_validation!("broker url must not contain whitespace");
    }
    let authority = url
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .unwrap_or_default();
    if authority.contains('@') {
        bail_validation!("broker url must not contain user info");
    }
    if authority.contains('?') {
        bail_validation!("broker url must not contain a query");
    }

    let parsed =
        Url::parse(url).map_err(|err| validation_err!("invalid broker url: {}", err.as_text()))?;
    let tls = match parsed.scheme() {
        Scheme::Mqtt => false,
        Scheme::Mqtts => true,
        other => bail_validation!(
            "unsupported broker scheme '{}', expected 'mqtt' or 'mqtts'",
            other.as_str()
        ),
    };
    if parsed.path() != "/" {
        bail_validation!("broker url must not contain a path");
    }
    if parsed.query().is_some() {
        bail_validation!("broker url must not contain a query");
    }
    if parsed.host().is_empty() {
        bail_validation!("broker url has an empty host");
    }
    if authority.starts_with('[') && parsed.host_ip().is_none() {
        bail_validation!("broker url brackets must hold an IPv6 address");
    }
    if parsed.port_or_default() == 0 {
        bail_validation!("broker url port must not be 0");
    }

    let host = match parsed.scope_id() {
        Some(_) if tls => bail_validation!("broker url zone ids cannot be verified over TLS"),
        Some(scope) => format!("{}%{scope}", parsed.host()),
        None => parsed.host().to_owned(),
    };

    Ok(BrokerEndpoint {
        address: BrokerAddress::new(&host)?,
        port: parsed.port_or_default(),
        tls,
    })
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct BrokerAddress(String);

impl BrokerAddress {
    pub fn new(address_str: &str) -> Result<Self> {
        let trimmed = address_str.trim();

        if trimmed.is_empty() {
            bail_validation!("the MQTT broker address must not be empty");
        }

        if trimmed.contains("://") {
            bail_validation!(
                "invalid format of broker address: URL scheme detected. Please provide only a hostname or IP address without '://' (e.g., provide just 'localhost' instead of 'http://localhost')."
            );
        }

        if trimmed.contains(' ') {
            bail_validation!("the MQTT broker address must not contain spaces");
        }

        if trimmed
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '%')))
        {
            bail_validation!(
                "the provided MQTT broker address contains disallowed special characters"
            )
        }

        let (ip, zone) = trimmed
            .split_once('%')
            .map_or((trimmed, None), |(ip, zone)| (ip, Some(zone)));
        if zone.is_some_and(|zone| zone.is_empty() || !zone.chars().all(|c| c.is_ascii_digit())) {
            bail_validation!("the zone id of an IPv6 broker address must be a number");
        }
        if (zone.is_some() || ip.contains(':')) && ip.parse::<Ipv6Addr>().is_err() {
            bail_validation!("':' and a zone id ('%') are only allowed in an IPv6 broker address");
        }

        Ok(Self(trimmed.to_owned()))
    }
}

impl AsRef<str> for BrokerAddress {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Represents a valid MQTT topic for subscriptions.
/// According to the MQTT standard, subscription topics:
/// - MUST NOT be empty,
/// - MUST NOT contain null characters,
/// - MAY contain wildcard characters, but with the following restrictions:
///   - The multi-level wildcard '#' must occupy an entire level and can only appear once, and only as the last level.
///   - The single-level wildcard '+' must occupy an entire level.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct SubTopic(String);

impl AsRef<str> for SubTopic {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl SubTopic {
    pub fn new(topic_str: &str) -> Result<Self> {
        let trimmed = topic_str.trim();
        if trimmed.is_empty() {
            bail_validation!("An MQTT sub topic must not be empty");
        }

        if trimmed.contains('\0') {
            bail_validation!("An MQTT sub topic must not contain null characters");
        }

        let mut tokens = trimmed.split('/').peekable();
        let mut multi_count = 0;
        while let Some(token) = tokens.next() {
            if token.contains('#') {
                if token != "#" {
                    bail_validation!("Multi-level wildcard '#' must occupy an entire level");
                }

                multi_count += 1;
                if tokens.peek().is_some() {
                    bail_validation!("Multi-level wildcard '#' must be the last level");
                }
                if multi_count > 1 {
                    bail_validation!("Only one multi-level wildcard '#' is allowed");
                }
            }
            if token.contains('+') && token != "+" {
                bail_validation!("Single-level wildcard '+' must occupy an entire level");
            }
        }
        Ok(Self(trimmed.to_owned()))
    }
}

/// Represents a valid MQTT topic for publishing messages.
/// According to the MQTT standard, publish topics:
/// - MUST NOT be empty,
/// - MUST NOT contain null characters,
/// - MUST NOT contain wildcard characters '#' or '+'.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct PubTopic(String);

impl AsRef<str> for PubTopic {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl PubTopic {
    pub fn new(topic_str: &str) -> Result<Self> {
        let trimmed = topic_str.trim();
        if trimmed.is_empty() {
            bail_validation!("An MQTT pub topic must not be empty");
        }
        if trimmed.contains('\0') {
            bail_validation!("An MQTT pub topic must not contain null characters");
        }
        if trimmed.contains('#') {
            bail_validation!("MQTT pub topic must not contain the multi-level wildcard '#'");
        }
        if trimmed.contains('+') {
            bail_validation!("MQTT pub topic must not contain the single-level wildcard '+'");
        }
        Ok(Self(trimmed.to_owned()))
    }
}

/// Whether a published `topic` matches a subscription `filter` under MQTT semantics:
/// `+` matches one level, `#` the rest (zero or more levels), and a filter that starts
/// with a wildcard never matches a `$`-topic.
#[must_use]
pub fn topic_matches(topic: &str, filter: &str) -> bool {
    let mut filter_levels = filter.split('/');
    let mut topic_levels = topic.split('/');

    if topic.starts_with('$') && matches!(filter_levels.clone().next(), Some("+" | "#")) {
        return false;
    }

    loop {
        match (filter_levels.next(), topic_levels.next()) {
            (Some("#"), _) | (None, None) => return true,
            (Some("+"), Some(_)) => {}
            (Some(filter_level), Some(topic_level)) if filter_level == topic_level => {}
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    mod topic_matches {
        use crate::mqtt::topic_matches;

        #[test]
        fn exact_topic_matches_itself_only() {
            assert!(topic_matches("plant/a/temp", "plant/a/temp"));
            assert!(!topic_matches("plant/a/temp", "plant/a"));
            assert!(!topic_matches("plant/a", "plant/a/temp"));
        }

        #[test]
        fn plus_matches_exactly_one_level() {
            assert!(topic_matches("plant/a/temp", "plant/+/temp"));
            assert!(topic_matches("plant/b/temp", "plant/+/temp"));
            assert!(!topic_matches("plant/a/b/temp", "plant/+/temp"));
            assert!(!topic_matches("plant", "plant/+"));
            assert!(topic_matches("plant//temp", "plant/+/temp"));
        }

        #[test]
        fn hash_matches_the_rest_including_nothing() {
            assert!(topic_matches("plant", "plant/#"));
            assert!(topic_matches("plant/a", "plant/#"));
            assert!(topic_matches("plant/a/b", "plant/#"));
            assert!(!topic_matches("other/a", "plant/#"));
            assert!(topic_matches("anything/at/all", "#"));
        }

        #[test]
        fn dollar_topics_need_an_explicit_first_level() {
            assert!(!topic_matches("$SYS/x", "#"));
            assert!(!topic_matches("$SYS/x", "+/x"));
            assert!(topic_matches("$SYS/x", "$SYS/+"));
            assert!(topic_matches("$SYS/x", "$SYS/x"));
            assert!(topic_matches("$SYS/a/b", "$SYS/#"));
        }
    }

    mod parse_broker_url {
        use crate::mqtt::parse_broker_url;

        #[test]
        fn accepted_urls_yield_host_port_and_tls() {
            for (url, host, port, tls) in [
                ("mqtt://broker.example", "broker.example", 1883, false),
                ("mqtts://broker.example", "broker.example", 8883, true),
                ("mqtts://broker.example:1883", "broker.example", 1883, true),
                ("MQTTS://h", "h", 8883, true),
                ("MQTT://h", "h", 1883, false),
                ("mqtt://[::1]", "::1", 1883, false),
                ("mqtts://[::1]:8883", "::1", 8883, true),
                ("mqtt://[fe80::1]", "fe80::1", 1883, false),
                ("mqtt://[2001:db8::1]:1884", "2001:db8::1", 1884, false),
                ("mqtt://[fe80::1%3]", "fe80::1%3", 1883, false),
            ] {
                let endpoint =
                    parse_broker_url(url).unwrap_or_else(|err| panic!("{url} rejected: {err}"));
                assert_eq!(
                    (endpoint.address.as_ref(), endpoint.port, endpoint.tls),
                    (host, port, tls),
                    "{url}"
                );
            }
        }

        #[test]
        fn rejected_urls_error_with_a_clear_message() {
            for (url, message) in [
                (
                    "mqtts://[fe80::1%3]",
                    "zone ids cannot be verified over TLS",
                ),
                ("mqtt://", "empty host"),
                ("mqtt://u:p@h", "user info"),
                ("mqtt://u@h", "user info"),
                ("mqtt://h/x", "path"),
                ("mqtt://h?x", "query"),
                ("http://h", "unsupported broker scheme 'http'"),
                ("mqtt://[::1", "ipv6 address was invalid"),
                ("mqtt://h:99999", "invalid port"),
                ("mqtt://mqtt_broker", "disallowed special characters"),
                ("mqtt://[broker.example]", "IPv6 address"),
                ("mqtt://[1.2.3.4]", "IPv6 address"),
                ("mqtt://[a:b]", "IPv6 address"),
                ("mqtt://h#x", "disallowed special characters"),
                ("mqtt://h%41", "zone id"),
                ("mqtt://[fe80::1%x]", "invalid scope"),
                ("mqtt://h:", "missing port"),
                ("mqtt://:1883", "empty host"),
                ("mqtt://h ", "whitespace"),
                ("mqtt:// h", "whitespace"),
                ("mqtt://h:0", "must not be 0"),
            ] {
                let err = parse_broker_url(url).expect_err(url).to_string();
                assert!(err.contains(message), "{url}: unexpected error: {err}");
            }
        }

        #[test]
        fn errors_do_not_echo_credentials_or_tokens() {
            for url in [
                "mqtt://user:s3cret@h",
                "mqtt://h?token=s3cret",
                "mqtt://h:s3cret",
                "mqtt://[s3cret",
            ] {
                let err = parse_broker_url(url).expect_err(url).to_string();
                assert!(!err.contains("s3cret"), "{url}: error leaks it: {err}");
            }
        }
    }

    mod sub_topic {
        use crate::mqtt::SubTopic;

        #[test]
        fn test_valid_topic_no_wildcards() {
            let topic = "sensor/temperature";
            let subtopic = SubTopic::new(topic);
            assert!(subtopic.is_ok());
            assert_eq!(subtopic.unwrap().as_ref(), topic);
        }

        #[test]
        fn test_valid_topic_single_level_wildcard() {
            let topic = "sensor/+/temperature";
            let subtopic = SubTopic::new(topic);
            assert!(subtopic.is_ok());
            assert_eq!(subtopic.unwrap().as_ref(), topic);
        }

        #[test]
        fn test_valid_topic_multi_level_wildcard() {
            let topic = "sensor/#";
            let subtopic = SubTopic::new(topic);
            assert!(subtopic.is_ok());
            assert_eq!(subtopic.unwrap().as_ref(), topic);
        }

        #[test]
        fn test_valid_topic_only_wildcard() {
            let topic = "#";
            let subtopic = SubTopic::new(topic);
            assert!(subtopic.is_ok());
            assert_eq!(subtopic.unwrap().as_ref(), topic);
        }

        #[test]
        fn test_invalid_empty_topic() {
            let topic = "";
            let subtopic = SubTopic::new(topic);
            assert!(subtopic.is_err());
        }

        #[test]
        fn test_invalid_topic_with_partial_wildcard() {
            let topic = "sensor/tem+perature";
            let subtopic = SubTopic::new(topic);
            assert!(subtopic.is_err());
        }

        #[test]
        fn test_invalid_topic_with_misplaced_plus() {
            let topic = "sensor/+foo";
            let subtopic = SubTopic::new(topic);
            assert!(subtopic.is_err());
        }

        #[test]
        fn test_invalid_topic_with_multi_level_wildcard_in_middle() {
            let topic = "sensor/#/data";
            let subtopic = SubTopic::new(topic);
            assert!(subtopic.is_err());
        }

        #[test]
        fn test_invalid_topic_with_extra_multi_level_wildcards() {
            let topic = "sensor/#/#";
            let subtopic = SubTopic::new(topic);
            assert!(subtopic.is_err());
        }
    }

    mod pub_topic {
        use crate::mqtt::PubTopic;

        #[test]
        fn test_valid_pub_topic_simple() {
            let topic = "sensor/temperature";
            let pub_topic = PubTopic::new(topic);
            assert!(pub_topic.is_ok());
            assert_eq!(pub_topic.unwrap().as_ref(), topic);
        }

        #[test]
        fn test_valid_pub_topic_complex() {
            let topic = "home/kitchen/lights";
            let pub_topic = PubTopic::new(topic);
            assert!(pub_topic.is_ok());
            assert_eq!(pub_topic.unwrap().as_ref(), topic);
        }

        #[test]
        fn test_invalid_empty_pub_topic() {
            let topic = "";
            let pub_topic = PubTopic::new(topic);
            assert!(pub_topic.is_err());
        }

        #[test]
        fn test_invalid_pub_topic_with_null() {
            let topic = "sensor\0temperature";
            let pub_topic = PubTopic::new(topic);
            assert!(pub_topic.is_err());
        }

        #[test]
        fn test_invalid_pub_topic_with_multi_level_wildcard() {
            // Wildcard '#' is not allowed in a publish topic.
            let topic = "sensor/#/temperature";
            let pub_topic = PubTopic::new(topic);
            assert!(pub_topic.is_err());
        }

        #[test]
        fn test_invalid_pub_topic_with_single_level_wildcard() {
            // Wildcard '+' is not allowed in a publish topic.
            let topic = "sensor/+/temperature";
            let pub_topic = PubTopic::new(topic);
            assert!(pub_topic.is_err());
        }

        #[test]
        fn test_invalid_pub_topic_with_wildcards_embedded() {
            // Even if the wildcard appears in the middle of a token, it should be rejected.
            let topic1 = "sensor#temperature";
            let topic2 = "sensor+temperature";
            assert!(PubTopic::new(topic1).is_err());
            assert!(PubTopic::new(topic2).is_err());
        }
    }

    mod broker_address {
        use crate::mqtt::BrokerAddress;

        #[test]
        fn address_table() {
            for (addr, accepted) in [
                ("192.168.1.1", true),
                ("2001:db8::1", true),
                ("localhost", true),
                ("example.com", true),
                ("broker.example", true),
                ("1.2.3.4", true),
                ("::1", true),
                ("fe80::1%3", true),
                ("", false),
                ("http://localhost", false),
                ("local host", false),
                ("ex@mple.com", false),
                ("[::1]", false),
                ("bröker", false),
                ("h%3", false),
                ("fe80::1%", false),
                ("fe80::1%x", false),
                ("h:80", false),
            ] {
                let result = BrokerAddress::new(addr);
                assert_eq!(result.is_ok(), accepted, "{addr}");
                if let Ok(address) = result {
                    assert_eq!(address.as_ref(), addr);
                }
            }
        }
    }
}
