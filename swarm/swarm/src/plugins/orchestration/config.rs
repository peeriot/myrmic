use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The orchestration plugin's configuration, as the `orchestration` section of a runtime's config
/// file has it: a field the file leaves out takes its default.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    init_timeout_secs: Option<f64>,
}

impl Config {
    /// How long the orchestration waits for a runtime to confirm a cell it was told to run, e.g.
    /// an embedded runtime that downloads and loads the cell first; 15 s by default.
    pub fn set_init_timeout(&mut self, init_timeout: Duration) -> &mut Self {
        self.init_timeout_secs = Some(init_timeout.as_secs_f64());
        self
    }
}

type OrchConfig = sorg_orchestration::Config;

impl From<Config> for OrchConfig {
    fn from(value: Config) -> Self {
        let mut orch_config = OrchConfig::default();
        if let Some(init_timeout_secs) = value.init_timeout_secs {
            orch_config.set_init_timeout(Duration::from_secs_f64(init_timeout_secs));
        }
        orch_config
    }
}
