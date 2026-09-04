use serde::{Deserialize, Serialize};

use rumqttd::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    /// Optional rumqttd router overrides; when omitted, router defaults are used.
    #[serde(default)]
    pub router: Option<RouterConfig>,

    /// MQTT v3.1.1 TCP listeners to expose for incoming client connections.
    #[serde(default)]
    pub v4: Vec<ServerSettings>,

    /// MQTT v5 TCP listeners to expose for incoming client connections.
    #[serde(default)]
    pub v5: Vec<ServerSettings>,

    /// MQTT-over-WebSocket listeners for browser or WS-based MQTT clients.
    #[serde(default)]
    pub ws: Vec<ServerSettings>,

    /// Topic filters that are explicitly allowed through the bridge.
    #[serde(default)]
    pub allow: Vec<Topic>,
}

#[derive(Clone, Debug)]
pub struct Topic(String);

impl Topic {
    #[allow(dead_code)]
    pub fn is_wild(&self) -> bool {
        self.0.contains(['*', '#', '+'])
    }

    pub fn as_mqtt(&self) -> std::borrow::Cow<'_, str> {
        if self.0.contains(['*']) {
            let topic = self.0.replace("**", "#");
            let topic = topic.replace('*', "+");

            std::borrow::Cow::Owned(topic)
        } else {
            std::borrow::Cow::Borrowed(&self.0)
        }
    }

    pub fn as_zenoh(&self) -> std::borrow::Cow<'_, str> {
        if self.0.contains(['+', '#']) {
            let topic = self.0.replace('#', "**");
            let topic = topic.replace('+', "*");

            std::borrow::Cow::Owned(topic)
        } else {
            std::borrow::Cow::Borrowed(&self.0)
        }
    }
}

impl std::str::FromStr for Topic {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.starts_with('/') {
            return Err(format!(
                "Topic not allowed to start with slash: {} (starts with \"/\")",
                s
            ));
        }
        if s.ends_with('/') {
            return Err(format!(
                "Topic not allowed to end with slash: {} (ends with \"/\")",
                s
            ));
        }
        if s.contains("//") {
            return Err(format!(
                "Topic with an empty level isn't supported: {} (contains \"//\")",
                s
            ));
        }

        if s.contains("$*") {
            return Err(format!(
                "Topic not allowed to contain a sub-chunk wildcard: {} (contains \"$*\")",
                s
            ));
        }

        Ok(Topic(String::from(s)))
    }
}

impl<'de> Deserialize<'de> for Topic {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        std::str::FromStr::from_str(&s).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Topic {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}
