use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The `test_hooks` section of a runtime's config: ways a test makes the runtime misbehave on
/// purpose, to bring about a state that is hard to reach otherwise. The type is always there, so
/// a test can describe a runtime's hooks against any build; but a runtime only knows the section,
/// and only has the hooks, when it is built with the `test-hooks` feature. Without it, a config
/// naming the section is rejected.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestHooks {
    /// Hold back everything replication brings into this runtime by this long before handling it:
    /// each replication message that tells of data or carries it, and the data its own catch-up
    /// pulls fetch. So its copies lag behind the other runtimes' by about this long, while
    /// everything else between the runtimes travels without delay: requests, transactions, and
    /// what this runtime serves the other runtimes' replication. The pacing of a pull of many
    /// pages counts the wait too, so the second page of such a pull may come up to this long late
    /// once more. Zero, the default, holds back nothing.
    ///
    /// The other runtimes broadcast their replication messages to all runtimes that hold a copy,
    /// so the receiving runtime is the only place where one runtime's copy alone can be held back.
    #[serde(default, with = "humantime_text")]
    pub replication_delay: Duration,
}

/// (De)serializes a duration as a humantime string (e.g. `"500ms"`), as the runtime's other
/// durations are written in its config.
mod humantime_text {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        humantime::format_duration(*duration)
            .to_string()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        humantime::parse_duration(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::TestHooks;

    /// The test framework writes the section with `serde_json`, and the runtime reads it back.
    #[test]
    fn the_replication_delay_round_trips() {
        let hooks = TestHooks {
            replication_delay: Duration::from_millis(1500),
        };
        let json = serde_json::to_string(&hooks).expect("the hooks serialize");
        let read: TestHooks = serde_yaml::from_str(&json).expect("the hooks deserialize");
        assert_eq!(read, hooks);
    }
}
