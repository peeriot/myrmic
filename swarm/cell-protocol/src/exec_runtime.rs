use myrmic_tags::Platform;
use serde::{Deserialize, Serialize};

use crate::RuntimeId;
use crate::sys::{string::String, vec::Vec};

/// A tag specifying the capability of a runtime or its host node for some user-defined service.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct CapabilityTag(String);

impl AsRef<str> for CapabilityTag {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl CapabilityTag {
    /// Creates a new capability tag.
    pub fn new(str: impl Into<String>) -> Self {
        Self(str.into())
    }

    /// Pulls apart the tag, giving you access to the string itself.
    pub fn into_inner(self) -> String {
        self.0
    }
}

/// The communicated capabilities of a particular exec runtime, including both
/// plugin configuration/compilation capabilities and host node capabilities.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct ExecutionCapabilities {
    tags: Vec<CapabilityTag>,
}

impl ExecutionCapabilities {
    /// Creates a new set of execution capabilities from the given tags.
    #[must_use]
    pub fn new(tags: Vec<CapabilityTag>) -> Self {
        Self { tags }
    }

    /// Returns the capability tags.
    #[must_use]
    pub fn tags(&self) -> &[CapabilityTag] {
        &self.tags
    }
}

/// How a runtime executes cells, derived from its capability tags — limited to
/// the platforms the orchestrator currently knows how to deploy to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    /// A linux host — loads wasm cells directly.
    Linux,
    /// An ESP32-C5 embedded device — loads AOT cells via the DB-mailbox protocol.
    Esp32c5,
    /// An ESP32-C6 embedded device — loads AOT cells via the DB-mailbox protocol.
    Esp32c6,
    /// An ESP32-C61 embedded device — loads AOT cells via the DB-mailbox protocol.
    Esp32c61,
    /// A runtime the orchestrator has no deploy path for: no recognized target
    /// tag, or a known target it does not yet support.
    Unknown,
}

impl RuntimeKind {
    /// Whether this is an embedded runtime. Embedded runtimes are capacity-1:
    /// they host at most one cell at a time.
    #[must_use]
    pub fn is_embedded(self) -> bool {
        match self {
            RuntimeKind::Linux | RuntimeKind::Unknown => false,
            RuntimeKind::Esp32c5 | RuntimeKind::Esp32c6 | RuntimeKind::Esp32c61 => true,
        }
    }
}

impl crate::sys::fmt::Display for RuntimeKind {
    fn fmt(&self, f: &mut crate::sys::fmt::Formatter<'_>) -> crate::sys::fmt::Result {
        let name = match self {
            RuntimeKind::Linux => "linux",
            RuntimeKind::Esp32c5 => "esp32c5",
            RuntimeKind::Esp32c6 => "esp32c6",
            RuntimeKind::Esp32c61 => "esp32c61",
            RuntimeKind::Unknown => "unknown",
        };
        f.write_str(name)
    }
}

/// Record describing an execution runtime.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ExecRuntimeInfo {
    id: RuntimeId,
    name: Option<String>,
    capabilities: ExecutionCapabilities,
}

impl ExecRuntimeInfo {
    /// Creates a new exec runtime info.
    #[must_use]
    pub fn new(
        id: impl Into<RuntimeId>,
        name: Option<String>,
        capabilities: ExecutionCapabilities,
    ) -> Self {
        Self {
            id: id.into(),
            name,
            capabilities,
        }
    }

    /// Returns the name of the runtime, if set.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the runtime ID.
    #[must_use]
    pub fn id(&self) -> RuntimeId {
        self.id
    }

    /// Returns the capabilities of the runtime.
    #[must_use]
    pub fn capabilities(&self) -> &ExecutionCapabilities {
        &self.capabilities
    }

    /// Classifies the runtime from its capability tags.
    ///
    /// The first tag that names a known [`Platform`] decides: `linux` yields
    /// [`RuntimeKind::Linux`] and `esp32c6` yields [`RuntimeKind::Esp32c6`]. Any
    /// other platform, or no platform tag at all, yields [`RuntimeKind::Unknown`].
    #[must_use]
    pub fn runtime_kind(&self) -> RuntimeKind {
        for tag in self.capabilities.tags() {
            if let Ok(platform) = Platform::try_from(tag.as_ref()) {
                return match platform {
                    Platform::Linux => RuntimeKind::Linux,
                    Platform::Esp32c5 => RuntimeKind::Esp32c5,
                    Platform::Esp32c6 => RuntimeKind::Esp32c6,
                    Platform::Esp32c61 => RuntimeKind::Esp32c61,
                };
            }
        }
        RuntimeKind::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::RuntimeKind;

    #[test]
    fn all_esp_runtimes_are_embedded() {
        // Every ESP32 firmware is capacity-1; placement relies on this to cap
        // one cell per embedded node, so no variant may be missed.
        assert!(RuntimeKind::Esp32c5.is_embedded());
        assert!(RuntimeKind::Esp32c6.is_embedded());
        assert!(RuntimeKind::Esp32c61.is_embedded());

        assert!(!RuntimeKind::Linux.is_embedded());
        assert!(!RuntimeKind::Unknown.is_embedded());
    }
}
