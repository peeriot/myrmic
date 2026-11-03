//! Module defining types representing concepts from the MQTT domain.

use serde::{Deserialize, Serialize};

use crate::{Result, bail_validation};

pub use myrmic_common::codegen::bridge_api::Qos;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct MqttConnection {
    pub broker_address: BrokerAddress,
    pub broker_port: u16,
    pub keep_alive_period: core::time::Duration,
    pub client_id: String,
    pub channel_cap: u16,
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

        if trimmed.chars().any(|c| {
            !(c.is_alphanumeric() || c == '-' || c == '.' || c == ':' || c == '[' || c == ']')
        }) {
            bail_validation!(
                "the provided MQTT broker address contains disallowed special characters"
            )
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
        fn test_valid_ipv4() {
            let addr = "192.168.1.1";
            let broker_addr = BrokerAddress::new(addr);
            assert!(broker_addr.is_ok());
            assert_eq!(broker_addr.unwrap().as_ref(), addr);
        }

        #[test]
        fn test_valid_ipv6() {
            let addr = "2001:db8::1";
            let broker_addr = BrokerAddress::new(addr);
            assert!(broker_addr.is_ok());
            assert_eq!(broker_addr.unwrap().as_ref(), addr);
        }

        #[test]
        fn test_valid_hostname() {
            let addr = "localhost";
            let broker_addr = BrokerAddress::new(addr);
            assert!(broker_addr.is_ok());
            assert_eq!(broker_addr.unwrap().as_ref(), addr);
        }

        #[test]
        fn test_valid_hostname_example() {
            let addr = "example.com";
            let broker_addr = BrokerAddress::new(addr);
            assert!(broker_addr.is_ok());
            assert_eq!(broker_addr.unwrap().as_ref(), addr);
        }

        #[test]
        fn test_invalid_empty() {
            let addr = "";
            let broker_addr = BrokerAddress::new(addr);
            assert!(broker_addr.is_err());
        }

        #[test]
        fn test_invalid_with_scheme() {
            let addr = "http://localhost";
            let broker_addr = BrokerAddress::new(addr);
            assert!(broker_addr.is_err());
        }

        #[test]
        fn test_invalid_with_spaces() {
            let addr = "local host";
            let broker_addr = BrokerAddress::new(addr);
            assert!(broker_addr.is_err());
        }

        #[test]
        fn test_invalid_with_special_characters() {
            let addr = "ex@mple.com";
            let broker_addr = BrokerAddress::new(addr);
            assert!(broker_addr.is_err());
        }
    }
}
